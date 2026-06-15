# loom — the weaver orchestrator — packaged for a reverse-proxy deploy.

# ---- build: loom + tapestry + weaver, plus the embedded Vue bundle ----
FROM rust:1-bookworm AS build
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# loom's build.rs runs `npm install` + rspack, emitting crates/loom/static/dist.
RUN cargo build --release -p loom -p tapestry -p weaver

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
    apt-get install -y --no-install-recommends nodejs git ca-certificates gh google-cloud-cli; \
    npm i -g @anthropic-ai/claude-code; \
    rm -rf /var/lib/apt/lists/*

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

# loom resolves the tapestry PTY supervisor as a sibling of its own binary
# (current_exe dir + /tapestry), so the two must land in the same directory.
COPY --from=build /src/target/release/loom     /usr/local/bin/loom
COPY --from=build /src/target/release/tapestry /usr/local/bin/tapestry
# `weaver` is the agent-facing CLI — kept on PATH for `docker exec weaver
# weaver config set …` (settings live in the shared sqlite db).
COPY --from=build /src/target/release/weaver   /usr/local/bin/weaver
COPY --from=build /src/crates/loom/static/dist /app/static/dist

# static_dir() defaults to a build-time CARGO_MANIFEST_DIR path that doesn't
# exist here, so point it at the copied bundle explicitly. WEAVER_HOME is left to
# default to $HOME/.weaver — the persisted /home/app volume — holding the
# sqlite db, server.json, and the 0600 machine-local token.
ENV WEAVER_STATIC_DIR=/app/static/dist

USER app
EXPOSE 7878
# `server run` is the foreground daemon (REST API + Vue UI + monitor loop); bind
# off loopback so the Caddy container can reach it over the `web` network.
CMD ["loom", "server", "run", "--addr", "0.0.0.0:7878"]
