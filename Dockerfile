# syntax=docker/dockerfile:1
# loom — the weaver orchestrator — packaged for a reverse-proxy deploy.

# ---- build: loom + tapestry + weaver, plus the embedded Vue bundle ----
FROM rust:1-bookworm AS build
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .

# `debug` (default) is far faster to compile — fine for local/standalone
# try-outs; pass `release` for a production image (`CARGO_PROFILE=release`).
ARG CARGO_PROFILE=debug

# BuildKit cache mounts persist the cargo registry/git and the target dir across
# builds *without* baking them into the image, so an incremental rebuild only
# recompiles what actually changed instead of the whole dependency tree — the
# difference between a multi-minute and a few-second `docker compose up --build`.
# The compiled artifacts live inside the (ephemeral) target mount, which doesn't
# survive into the image layer, so they're copied out to /out (a real layer).
#
# The Vue bundle is built explicitly rather than left to loom's build.rs: with
# the target cached, cargo may skip build.rs (unchanged fingerprint), and
# static/dist is excluded from the build context (.dockerignore) — so an
# incremental build would otherwise have no bundle for the runtime stage to copy.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/root/.npm \
    set -eux; \
    ( cd crates/loom/frontend && npm ci && npm run build ); \
    if [ "$CARGO_PROFILE" = release ]; then \
        cargo build --release -p loom -p tapestry -p weaver; TARGET_DIR=release; \
    else \
        cargo build -p loom -p tapestry -p weaver; TARGET_DIR=debug; \
    fi; \
    mkdir -p /out; \
    cp "target/$TARGET_DIR/loom" "target/$TARGET_DIR/tapestry" "target/$TARGET_DIR/weaver" /out/; \
    cp -r crates/loom/static/dist /out/dist

# ---- runtime: loom + the toolchain its agents shell out to ----
FROM rust:1-bookworm
RUN set -eux; \
    curl -fsSL https://deb.nodesource.com/setup_22.x | bash -; \
    install -m 0755 -d /etc/apt/keyrings; \
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /etc/apt/keyrings/githubcli-archive-keyring.gpg; \
    chmod a+r /etc/apt/keyrings/githubcli-archive-keyring.gpg; \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      > /etc/apt/sources.list.d/github-cli.list; \
    # Google Cloud CLI repo — same keyring pattern. The published key is
    # ASCII-armored, which bookworm's apt accepts via `signed-by` when the file
    # carries a `.asc` extension, so no `gpg --dearmor` (and no gnupg) is needed.
    curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg \
      -o /etc/apt/keyrings/cloud.google.asc; \
    chmod a+r /etc/apt/keyrings/cloud.google.asc; \
    echo "deb [signed-by=/etc/apt/keyrings/cloud.google.asc] https://packages.cloud.google.com/apt cloud-sdk main" \
      > /etc/apt/sources.list.d/google-cloud-sdk.list; \
    apt-get update; \
    apt-get install -y --no-install-recommends nodejs git ca-certificates gh google-cloud-cli tini; \
    rm -rf /var/lib/apt/lists/*

# Claude Code — the agent runtime loom's sessions launch — is deliberately NOT
# baked into the image. The container runs as a non-root user, so a Claude
# installed into the read-only system dirs (what `npm i -g` does) can't
# auto-update and reports it's "installed in a read-only location". Instead
# loom-entrypoint (below) runs Claude's own installer at first boot into the app
# user's $HOME/.local — on the persisted loom_home volume, so it is writable,
# auto-updates itself in place, and the updates survive container recreates.

# code-server — the per-session embedded VS Code that `crate::ide` spawns and
# reverse-proxies (one rooted at each worktree, behind loom's auth). The `.deb`
# bundles its own Node, so it's self-contained on bookworm (glibc 2.28+) and
# doesn't couple to the system node above. Pinned; bump deliberately. Without it
# loom still runs — the editor panel just reports "not installed".
ARG CODE_SERVER_VERSION=4.124.2
RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    curl -fLo /tmp/code-server.deb \
      "https://github.com/coder/code-server/releases/download/v${CODE_SERVER_VERSION}/code-server_${CODE_SERVER_VERSION}_${arch}.deb"; \
    dpkg -i /tmp/code-server.deb; \
    rm -f /tmp/code-server.deb

# uv — for the Python repos loom's agents work in. Only the binary lives in the
# image; its downloaded interpreters and wheel cache live in a named volume (see
# UV_PYTHON_INSTALL_DIR / UV_CACHE_DIR below + docker-compose.yml), so the
# container manages its own Pythons — self-contained and persisted across
# recreates, isolated from the host's uv. Pinned to a recent uv.
COPY --from=ghcr.io/astral-sh/uv:0.11.21 /uv /uvx /usr/local/bin/

# Run as the host user that owns the bind-mounted code, so the worktrees and
# edits loom's agents create are owned by you on the host, not root. The uid/gid
# come from build args (set HOST_UID/HOST_GID in secrets/weaver.env); the
# in-container name/home stay generic — only the numeric ids affect ownership.
ARG HOST_UID=1000
ARG HOST_GID=1000
# Create the group only if that gid is free; a real groupadd failure (bad gid)
# still aborts the build instead of being masked by `|| true`.
RUN if ! getent group "${HOST_GID}" >/dev/null; then groupadd -g "${HOST_GID}" app; fi; \
    useradd -m -u "${HOST_UID}" -g "${HOST_GID}" -d /home/app -s /bin/bash app

# Set $HOME explicitly: the entrypoint and Claude's installer resolve
# $HOME/.local, and `USER app` alone doesn't reliably export HOME.
ENV HOME=/home/app
# The writable, self-updating Claude install (see loom-entrypoint) and any client
# packages added at runtime live under the app user's persisted $HOME, early on
# PATH. NPM_CONFIG_PREFIX points `npm i -g` at a home dir too, so new CLI packages
# can be installed live (and persist on the loom_home volume) without an image
# rebuild — only OS/apt packages still need one.
ENV NPM_CONFIG_PREFIX=/home/app/.npm-global \
    PATH=/home/app/.local/bin:/home/app/.npm-global/bin:${PATH}

# Where uv keeps its managed interpreters and wheel cache. Kept off /home/app so
# the (large) Python builds don't bloat the home volume and can be reset on their
# own; compose mounts a named volume here. Created owned by the host uid/gid so
# the fresh volume initialises app-writable (Docker seeds a new volume from the
# image dir's contents + ownership).
RUN mkdir -p /opt/uv/python /opt/uv/cache && chown -R "${HOST_UID}:${HOST_GID}" /opt/uv
ENV UV_PYTHON_INSTALL_DIR=/opt/uv/python \
    UV_CACHE_DIR=/opt/uv/cache

# Where the Google Cloud CLI keeps its config + credentials (CLOUDSDK_CONFIG).
# Same pattern as uv: a dedicated dir off /home/app so compose can back it with
# its own named volume — `gcloud auth login` (run via `docker exec`) then
# persists across recreates, and the creds can be reset on their own without
# touching the home volume. Created owned by the host uid/gid so the fresh
# volume initialises app-writable (Docker seeds a new volume from the image
# dir's contents + ownership).
RUN mkdir -p /opt/gcloud && chown -R "${HOST_UID}:${HOST_GID}" /opt/gcloud
ENV CLOUDSDK_CONFIG=/opt/gcloud

# Let agents `git push` over HTTPS with the injected GH_TOKEN — no mounted SSH
# key. The bind-mounted host repos usually have `git@github.com:` SSH remotes, so
# also rewrite GitHub SSH URLs to HTTPS: with no key in the container an SSH push
# fails with "Permission denied (publickey)", but rewritten it rides the token
# helper below. (Non-GitHub SSH remotes still need ~/.ssh mounted; see compose.)
RUN <<'EOF'
cat > /usr/local/bin/git-credential-ghtoken <<'SH'
#!/bin/sh
# git invokes the helper for get/store/erase; only `get` returns a credential,
# and store/erase must exit 0 or git warns about a failing helper.
[ "$1" = get ] || exit 0
printf 'username=x-access-token\npassword=%s\n' "$GH_TOKEN"
SH
chmod +x /usr/local/bin/git-credential-ghtoken
git config --system credential.https://github.com.helper ghtoken
git config --system url.https://github.com/.insteadOf git@github.com:
git config --system url.https://github.com/.insteadOf ssh://git@github.com/
EOF

# Container entrypoint: before the daemon starts, make sure the writable,
# self-updating Claude install exists on the persisted $HOME volume, then hand
# off to the CMD. Runs the install only for `loom server …` — one-shot admin
# commands (`loom config set`, `loom setup`, the compose init service) skip it.
RUN <<'EOF'
cat > /usr/local/bin/loom-entrypoint <<'SH'
#!/bin/sh
set -eu
if [ "${1:-}" = loom ] && [ "${2:-}" = server ] && [ ! -x "$HOME/.local/bin/claude" ]; then
  echo "loom: installing self-updating Claude Code into $HOME/.local ..." >&2
  # The stock native installer drops claude into $HOME/.local/bin (writable, on
  # the volume); Claude then auto-updates itself in place. Pin with
  # CLAUDE_CODE_VERSION (stable|latest|<version>; default stable). `curl | bash`
  # can't surface a download failure through the pipe, so check the binary landed
  # rather than trusting the exit status. Non-fatal: loom still boots either way;
  # agents just lack `claude` until a boot with network installs it.
  curl -fsSL https://claude.ai/install.sh | bash -s -- "${CLAUDE_CODE_VERSION:-stable}" || true
  [ -x "$HOME/.local/bin/claude" ] \
    || echo "loom: WARNING: Claude install failed (offline?); agents lack 'claude' until a later boot installs it" >&2
fi
exec "$@"
SH
chmod +x /usr/local/bin/loom-entrypoint
EOF

# loom resolves the tapestry PTY supervisor as a sibling of its own binary
# (current_exe dir + /tapestry), so the two must land in the same directory.
COPY --from=build /out/loom     /usr/local/bin/loom
COPY --from=build /out/tapestry /usr/local/bin/tapestry
# `weaver` is the agent-facing CLI — kept on PATH for `docker exec weaver
# weaver config set …` (settings live in the shared sqlite db).
COPY --from=build /out/weaver   /usr/local/bin/weaver
COPY --from=build /out/dist     /app/static/dist

# static_dir() defaults to a build-time CARGO_MANIFEST_DIR path that doesn't
# exist here, so point it at the copied bundle explicitly. WEAVER_HOME is left to
# default to $HOME/.weaver — the persisted /home/app volume — holding the
# sqlite db, server.json, and the 0600 machine-local token.
ENV WEAVER_STATIC_DIR=/app/static/dist

USER app
EXPOSE 7878
# tini as PID 1. loom is a tokio app that reaps only the children it spawns, but
# as the container's init it also inherits every orphan its agents leave behind:
# they shell out to `gh`, `sleep`, MCP servers, etc. and routinely detach, so
# those processes reparent to PID 1 when their immediate parent exits. With no
# init to `wait()` on them they pile up as zombies for the container's whole
# lifetime. tini reaps them and forwards signals through to loom. loom-entrypoint
# runs first (ensuring the writable Claude install) and then `exec`s the CMD, so
# loom still ends up as tini's direct child for the reaping above to work.
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/loom-entrypoint"]
# `server run` is the foreground daemon (REST API + Vue UI + monitor loop); bind
# off loopback so the Caddy container can reach it over the `web` network.
CMD ["loom", "server", "run", "--addr", "0.0.0.0:7878"]
