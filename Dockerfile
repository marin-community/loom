# syntax=docker/dockerfile:1
# Loom orchestrator packaged for a reverse-proxy deploy.

# ---- build: Loom + Tapestry, plus the embedded Vue bundle ----
FROM rust:1-bookworm AS build
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
 && apt-get install -y --no-install-recommends nodejs sccache \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .

# `debug` (default) is far faster to compile — fine for local/standalone
# try-outs; pass `release` for a production image (`CARGO_PROFILE=release`).
ARG CARGO_PROFILE=debug
# Immutable remote Git build contexts normally omit `.git`, so deployment
# tooling can pass the resolved source SHA explicitly. Local contexts may leave
# this empty and loom's build script discovers HEAD itself.
ARG LOOM_BUILD_REVISION

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
        cargo build --release -p loom -p tapestry; TARGET_DIR=release; \
    else \
        cargo build -p loom -p tapestry; TARGET_DIR=debug; \
    fi; \
    mkdir -p /out; \
    cp "target/$TARGET_DIR/loom" "target/$TARGET_DIR/tapestry" /out/; \
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
    # Docker CLI — client only, NO daemon. loom sessions run `docker build`; the
    # container reaches the *host's* Docker daemon over the bind-mounted
    # /var/run/docker.sock (see docker-compose.yml), so only the CLI + the buildx
    # and compose plugins ship here, not docker-ce/containerd. Same signed-repo
    # keyring idiom as gh/gcloud above.
    curl -fsSL https://download.docker.com/linux/debian/gpg \
      -o /etc/apt/keyrings/docker.asc; \
    chmod a+r /etc/apt/keyrings/docker.asc; \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
      > /etc/apt/sources.list.d/docker.list; \
    apt-get update; \
    # The index is deliberately kept in the image — no closing `rm -rf
    # /var/lib/apt/lists/*`. It costs ~20 MB here, and it is the difference
    # between an agent's `sudo apt-get install -y <pkg>` (the sudoers grant
    # further down) working on the first try and failing with "Unable to locate
    # package" until the agent works out that it has to refresh an index it cannot
    # see. A stale index only bites once a bookworm point release moves a version,
    # and `sudo apt-get update` fixes that.
    #
    # Base runtime + the repo tools loom's agents shell out to. gh/gcloud/docker
    # come from the signed repos configured above; the rest are stock bookworm.
    # The last groups are the everyday dev CLIs an agent reaches for in a checkout —
    # jq/ripgrep/fd for search, build-essential/pkg-config for native builds, a
    # system python3, and the usual archive/editor/pager tools. (cargo, rustc and
    # cc already ship in the rust base image, so they are not repeated here.)
    #
    # bubblewrap + socat are the sandbox the agent runtimes reach for on Linux:
    # Claude Code's sandboxed Bash runs commands under `bwrap` (with `socat`
    # relaying network) and otherwise degrades to unsandboxed with a warning;
    # Codex prefers a system `bwrap` over the one it bundles. Both need the
    # unprivileged user namespaces this container already permits (the
    # SYS_ADMIN + apparmor=unconfined + seccomp=unconfined grants in
    # docker-compose.yml).
    #
    # The remaining tools round out what a general coding agent expects to find:
    # sqlite3 (loom's own store is sqlite; agents inspect/seed .db files),
    # openssh-client (git-over-SSH to non-GitHub remotes; see compose), patch +
    # diffutils (applying diffs when a native edit won't fit), procps (ps/pkill
    # to manage servers an agent spawns), rsync, file, gettext-base (envsubst in
    # CI/templating), and shellcheck (agents lint the shell they write).
    #
    # git-lfs is needed to clone the Hugging Face repos and other LFS-backed
    # checkouts agents work in; without it the pointer files check out instead of
    # the payload. cmake + ninja-build + python3-dev + the -dev libraries are what
    # a source build of a Python C extension asks for when no wheel matches.
    # iproute2 (ss), lsof, netcat-openbsd, dnsutils and strace answer "is the
    # server I just started actually listening, and on what" — the loop an agent
    # runs constantly and cannot run with ps alone. tmux keeps a dev server alive
    # across commands. graphviz renders dot; poppler-utils (pdftotext) reads the
    # PDFs agents are pointed at. (ImageMagick already ships in the rust base.)
    apt-get install -y --no-install-recommends \
      nodejs git git-lfs ca-certificates gh google-cloud-cli tini \
      docker-ce-cli docker-buildx-plugin docker-compose-plugin sudo \
      jq ripgrep fd-find build-essential pkg-config cmake ninja-build \
      python3 python3-pip python3-venv python3-dev \
      libssl-dev libffi-dev zlib1g-dev \
      unzip zip less wget vim tree \
      bubblewrap socat \
      sqlite3 openssh-client patch diffutils procps rsync file \
      gettext-base shellcheck sccache \
      iproute2 lsof netcat-openbsd dnsutils strace tmux \
      graphviz poppler-utils; \
    # Chromium's shared-library and font dependencies, so a Playwright browser
    # downloaded at runtime (`playwright install chromium`, cached under the
    # persisted $HOME) actually launches — screenshotting a UI is routine agent
    # work and it should not need a root install per session. These are exactly
    # Playwright's own debian12 `chromium` + `tools` lists, i.e. what `playwright
    # install-deps` would apt-get; keep them in sync with upstream's nativeDeps
    # table rather than pruning to whatever the current Chromium build happens to
    # dlopen. The fonts are the same list's, and are what keeps a screenshot from
    # rendering CJK/emoji text as tofu. Firefox and WebKit are not covered: they
    # need a much larger (GTK/GStreamer) set, and agents here drive Chromium.
    apt-get install -y --no-install-recommends \
      libasound2 libatk-bridge2.0-0 libatk1.0-0 libatspi2.0-0 libcairo2 \
      libcups2 libdbus-1-3 libdrm2 libgbm1 libglib2.0-0 libnspr4 libnss3 \
      libpango-1.0-0 libx11-6 libxcb1 libxcomposite1 libxdamage1 libxext6 \
      libxfixes3 libxkbcommon0 libxrandr2 \
      xvfb libfontconfig1 libfreetype6 xfonts-scalable \
      fonts-liberation fonts-freefont-ttf fonts-noto-color-emoji fonts-unifont \
      fonts-ipafont-gothic fonts-wqy-zenhei fonts-tlwg-loma-otf; \
    # Debian ships fd as `fdfind` to avoid a name clash; expose the conventional
    # `fd` name agents (and fd-aware tools) expect.
    ln -s "$(command -v fdfind)" /usr/local/bin/fd; \
    # Runtime system packages for the agent user: a session that turns out to need
    # a package installs it itself (`sudo apt-get install -y <pkg>`; the retained
    # index above means no `update` step) instead of improvising around the missing
    # one. dpkg maintainer scripts run arbitrary code, so this grant is effectively
    # container root — but it is only ever handed to containers that already mount
    # the root-equivalent host Docker socket (docker-compose.yml, crate::runner),
    # so it adds no reach; see deploy/README.md "Security posture". It names the
    # three package tools rather than a blanket ALL, and restricted profiles
    # (docs/restricted-sessions.md) get no shell at all. Same single-line sudoers
    # idiom as loom-cgroup-init further down; `app` is created later in the build,
    # which sudo resolves at runtime.
    #
    # Such an install lands in the container's own writable layer: it lasts as long
    # as that session container and is invisible to other sessions. It is the
    # escape hatch for a one-off — anything agents reach for repeatedly belongs in
    # the package lists above.
    echo 'app ALL=(root) NOPASSWD: /usr/bin/apt-get, /usr/bin/apt, /usr/bin/dpkg' \
      > /etc/sudoers.d/loom-packages; \
    chmod 440 /etc/sudoers.d/loom-packages

# The agent runtimes loom's sessions launch — Claude Code (`claude`) and the
# OpenAI Codex CLI (`codex`) — are deliberately NOT baked into the image. The
# container runs as a non-root user, so a runtime installed into the read-only
# system dirs can neither self-update nor be bumped live. Instead
# loom-entrypoint (below) installs both native CLIs at first boot into the app
# user's $HOME on the persisted loom_home volume, where they are writable,
# update in place, and survive container recreates. Claude can be pinned with
# CLAUDE_CODE_VERSION (see compose).

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

# Let agents `git push` over HTTPS with the credential selected by Loom — no
# mounted SSH key. Loom stamps LOOM_GITHUB_AUTH_MODE on every managed session:
# `direct` consumes only a per-user PAT that Loom injected, `broker` requests an
# allowlisted GitHub App token through the session API, and restricted sessions
# or sessions without either are disabled. These scripts are transport adapters;
# Loom owns credential selection. The
# bind-mounted host repos usually have `git@github.com:` SSH remotes, so also
# rewrite GitHub SSH URLs to HTTPS. (Non-GitHub SSH remotes still need ~/.ssh
# mounted; see compose.)
RUN <<'EOF'
cat > /usr/local/bin/git-credential-ghtoken <<'SH'
#!/bin/sh
# git invokes the helper for get/store/erase; only `get` returns a credential,
# and store/erase must exit 0 or git warns about a failing helper.
[ "$1" = get ] || exit 0
case "${LOOM_GITHUB_AUTH_MODE:-}" in
  broker)
    if [ -z "$LOOM_SESSION_ID" ] || [ -z "$LOOM_TOKEN" ]; then
      echo "Loom-managed GitHub auth is missing its session credential" >&2
      exit 1
    fi
    # git writes the request as key=value lines on stdin. `credential.useHttpPath`
    # is enabled for github.com below, so `path` carries `owner/repo`; ask for a
    # token scoped to exactly that repository. An App installation token covers
    # one owner, so a session whose access spans owners has no single token.
    repository=
    while IFS= read -r line; do
      case "$line" in
        path=*) repository="${line#path=}" ;;
      esac
    done
    repository="${repository%.git}"
    if [ -n "$repository" ]; then
      token="$(/usr/local/bin/loom github-token --repository "$repository")" || exit $?
    else
      token="$(/usr/local/bin/loom github-token)" || exit $?
    fi
    ;;
  direct)
    # Loom sets direct only after injecting the launching user's Account token.
    token="$GH_TOKEN"
    if [ -z "$token" ]; then
      echo "Loom-managed direct GitHub auth is missing GH_TOKEN" >&2
      exit 1
    fi
    ;;
  disabled)
    exit 0
    ;;
  "")
    echo "missing LOOM_GITHUB_AUTH_MODE; this environment was not prepared by Loom" >&2
    exit 1
    ;;
  *)
    echo "invalid LOOM_GITHUB_AUTH_MODE: $LOOM_GITHUB_AUTH_MODE" >&2
    exit 1
    ;;
esac
[ -n "$token" ] || exit 0
printf 'username=x-access-token\npassword=%s\n' "$token"
SH
chmod +x /usr/local/bin/git-credential-ghtoken
cat > /usr/local/bin/gh <<'SH'
#!/bin/sh
case "${LOOM_GITHUB_AUTH_MODE:-}" in
  broker)
    if [ -z "$LOOM_SESSION_ID" ] || [ -z "$LOOM_TOKEN" ]; then
      echo "Loom-managed GitHub auth is missing its session credential" >&2
      exit 1
    fi
    # Same per-repository scoping as the git helper. gh names its target with
    # GH_REPO or the checkout's origin; without either, fall back to the
    # session's whole set, which the App can mint while it stays single-owner.
    repository="${GH_REPO:-}"
    if [ -z "$repository" ]; then
      origin="$(git config --get remote.origin.url 2>/dev/null || true)"
      origin="${origin%.git}"
      case "$origin" in
        *github.com[:/]*) repository="${origin##*github.com[:/]}" ;;
      esac
    fi
    if [ -n "$repository" ]; then
      GH_TOKEN="$(/usr/local/bin/loom github-token --repository "$repository")" || exit $?
    else
      GH_TOKEN="$(/usr/local/bin/loom github-token)" || exit $?
    fi
    export GH_TOKEN
    unset GITHUB_TOKEN
    ;;
  direct)
    # GH_TOKEN was injected by Loom from the launching user's Account settings.
    if [ -z "$GH_TOKEN" ]; then
      echo "Loom-managed direct GitHub auth is missing GH_TOKEN" >&2
      exit 1
    fi
    unset GITHUB_TOKEN
    ;;
  disabled)
    echo "GitHub CLI access is disabled for this Loom session" >&2
    exit 1
    ;;
  "")
    echo "missing LOOM_GITHUB_AUTH_MODE; this environment was not prepared by Loom" >&2
    exit 1
    ;;
  *)
    echo "invalid LOOM_GITHUB_AUTH_MODE: $LOOM_GITHUB_AUTH_MODE" >&2
    exit 1
    ;;
esac
exec /usr/bin/gh "$@"
SH
chmod +x /usr/local/bin/gh
git config --system credential.https://github.com.helper ghtoken
# Without this git omits `path` from the helper request, leaving the broker
# unable to tell which repository a push is for.
git config --system credential.https://github.com.useHttpPath true
git config --system url.https://github.com/.insteadOf git@github.com:
git config --system url.https://github.com/.insteadOf ssh://git@github.com/
EOF

# Per-session memory limits. loom confines each terminal session to its own
# memory-limited cgroup under /sys/fs/cgroup/agents (see backend::new_session);
# this root-only script prepares that subtree at boot and delegates it to the
# app user. cgroup v2's no-internal-process rule means the container-root cgroup
# must be emptied into a leaf (init/) before controllers can be enabled on its
# children, and migrating a session into agents/<name> additionally needs write
# access to the source/destination *common ancestor's* cgroup.procs — hence the
# chowns at the bottom. Needs the rw cgroupfs remount, which is why the compose
# service runs with SYS_ADMIN + apparmor=unconfined (docker-compose.yml); where
# that grant is absent this script fails and the entrypoint just warns — loom
# runs fine, sessions are simply unlimited. The app user may run exactly this
# script as root (sudoers line below) — that and the package tools above are its
# only privileged commands.
RUN <<'EOF'
cat > /usr/local/bin/loom-cgroup-init <<'SH'
#!/bin/sh
set -eu
[ -f /sys/fs/cgroup/cgroup.controllers ] || { echo "cgroup v2 not mounted" >&2; exit 1; }
if [ ! -w /sys/fs/cgroup ]; then
  # Docker mounts the container's cgroup view read-only; replace it with a rw
  # mount of the same (namespaced) tree.
  umount /sys/fs/cgroup
  mount -t cgroup2 cgroup2 /sys/fs/cgroup
fi
grep -qw memory /sys/fs/cgroup/cgroup.controllers || { echo "memory controller not delegated" >&2; exit 1; }
mkdir -p /sys/fs/cgroup/init /sys/fs/cgroup/agents
while read -r p; do
  echo "$p" > /sys/fs/cgroup/init/cgroup.procs || true
done < /sys/fs/cgroup/cgroup.procs
echo +memory > /sys/fs/cgroup/cgroup.subtree_control
echo +memory > /sys/fs/cgroup/agents/cgroup.subtree_control
chown -R app /sys/fs/cgroup/agents
chown app /sys/fs/cgroup/cgroup.procs /sys/fs/cgroup/init/cgroup.procs
SH
chmod 755 /usr/local/bin/loom-cgroup-init
echo 'app ALL=(root) NOPASSWD: /usr/local/bin/loom-cgroup-init' > /etc/sudoers.d/loom-cgroup-init
chmod 440 /etc/sudoers.d/loom-cgroup-init
EOF

# Container entrypoint: before the daemon starts, make sure the agent runtimes
# (`claude` + `codex`) are installed on the persisted $HOME volume and the
# delegated session-cgroup subtree is prepared, then hand off to the CMD. Both
# run only for `loom server …` — one-shot admin commands (`loom config set`,
# `loom setup`, the compose init service) skip them.
RUN <<'EOF'
cat > /usr/local/bin/loom-entrypoint <<'SH'
#!/bin/sh
set -eu
if [ "${1:-}" = loom ] && [ "${2:-}" = server ]; then
  if [ ! -x "$HOME/.local/bin/claude" ]; then
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
  if [ ! -x "$HOME/.local/bin/codex" ]; then
    echo "loom: installing native Codex CLI into $HOME/.local ..." >&2
    # Use OpenAI's native installer rather than the npm wrapper. It downloads the
    # platform binary into the persisted, writable home volume, which is already
    # early on PATH. CODEX_NON_INTERACTIVE prevents an entrypoint without a TTY
    # from blocking on installer prompts. Non-fatal — like Claude, loom still
    # boots and agents can add it on a later networked boot.
    # Codex is useless without OpenAI auth (OPENAI_API_KEY, or `codex login`),
    # but installing the CLI needs neither.
    curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh || true
    [ -x "$HOME/.local/bin/codex" ] \
      || echo "loom: WARNING: native Codex install failed (offline?); the codex runtime is unavailable until a later boot installs it" >&2
  fi
  if [ -x "$HOME/.local/bin/codex" ]; then
    # The ChatGPT login is shared for model access, but account-level apps carry
    # the identity of whoever authorized them. Keep them unavailable to every
    # Codex process in this multi-user container; Loom supplies GitHub identity.
    "$HOME/.local/bin/codex" features disable apps >/dev/null \
      || echo "loom: WARNING: could not disable Codex account-level apps" >&2
  fi
  claude_acp_version="${CLAUDE_ACP_VERSION:-0.66.0}"
  codex_acp_version="${CODEX_ACP_VERSION:-1.4.0}"
  if ! command -v claude-agent-acp >/dev/null 2>&1 \
    || ! command -v codex-acp >/dev/null 2>&1 \
    || ! npm list -g --depth=0 "@agentclientprotocol/claude-agent-acp@$claude_acp_version" >/dev/null 2>&1 \
    || ! npm list -g --depth=0 "@agentclientprotocol/codex-acp@$codex_acp_version" >/dev/null 2>&1; then
    echo "loom: installing pinned ACP adapters into $HOME/.npm-global ..." >&2
    # The ACP adapters Loom's sessions speak through. Exact
    # pins, not dist-tags: two upstream projects releasing weekly sit between
    # loom and the agents, so the fleet moves versions only via a deliberate
    # CLAUDE_ACP_VERSION / CODEX_ACP_VERSION bump. Installed on the volume so
    # they persist across recreates; loom's launch default (`npx --yes …`)
    # resolves these installed bins from PATH without a network fetch.
    # Non-fatal like the CLIs above.
    npm install -g \
      "@agentclientprotocol/claude-agent-acp@$claude_acp_version" \
      "@agentclientprotocol/codex-acp@$codex_acp_version" || true
    { command -v claude-agent-acp >/dev/null 2>&1 \
      && command -v codex-acp >/dev/null 2>&1 \
      && npm list -g --depth=0 "@agentclientprotocol/claude-agent-acp@$claude_acp_version" >/dev/null 2>&1 \
      && npm list -g --depth=0 "@agentclientprotocol/codex-acp@$codex_acp_version" >/dev/null 2>&1; } \
      || echo "loom: WARNING: pinned ACP adapter install failed (offline?); existing adapters may remain in use" >&2
  fi
  # Delegate the per-session cgroup subtree (see loom-cgroup-init above).
  # Non-fatal: without it sessions run with no memory limit.
  sudo -n /usr/local/bin/loom-cgroup-init \
    || echo "loom: WARNING: cgroup delegation failed; sessions run without memory limits" >&2
fi
exec "$@"
SH
chmod +x /usr/local/bin/loom-entrypoint
EOF

# loom resolves the tapestry PTY supervisor as a sibling of its own binary
# (current_exe dir + /tapestry), so the two must land in the same directory.
COPY --from=build /out/loom     /usr/local/bin/loom
COPY --from=build /out/tapestry /usr/local/bin/tapestry
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
