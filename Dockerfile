# loom — the weaver orchestrator — packaged for a reverse-proxy deploy.
#
# loom is not a stateless web app: it creates git worktrees and launches coding
# agents (claude, git, gh) as subprocesses *inside this container*. The runtime
# stage therefore stays on the Rust image and carries node + gh + the Claude
# Code CLI, so an in-container agent can build/test weaver itself. Slim this
# down once you know exactly which repos loom will drive.

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
    apt-get update; \
    apt-get install -y --no-install-recommends nodejs git ca-certificates gh; \
    npm i -g @anthropic-ai/claude-code; \
    rm -rf /var/lib/apt/lists/*

# Run as the host user that owns the bind-mounted code, so the worktrees and
# edits loom's agents create are owned by you on the host, not root. The uid/gid
# come from build args (set HOST_UID/HOST_GID in secrets/weaver.env); the
# in-container name/home stay generic — only the numeric ids affect ownership.
ARG HOST_UID=1000
ARG HOST_GID=1000
RUN groupadd -g "${HOST_GID}" app 2>/dev/null || true; \
    useradd -m -u "${HOST_UID}" -g "${HOST_GID}" -d /home/app -s /bin/bash app

# Let agents `git push` over HTTPS with the injected GH_TOKEN — no mounted SSH
# key. (Repos with git@github.com SSH remotes need ~/.ssh mounted; see compose.)
RUN <<'EOF'
cat > /usr/local/bin/git-credential-ghtoken <<'SH'
#!/bin/sh
[ "$1" = get ] && printf 'username=x-access-token\npassword=%s\n' "$GH_TOKEN"
SH
chmod +x /usr/local/bin/git-credential-ghtoken
git config --system credential.https://github.com.helper ghtoken
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
# Bind off loopback so the Caddy container can reach it over the `web` network.
CMD ["loom", "serve", "--addr", "0.0.0.0:7878"]
