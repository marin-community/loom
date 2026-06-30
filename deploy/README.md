# Deploying loom

loom ships as a single container image (the repo-root `Dockerfile`). How you put
a TLS front-door in front of it is the choice this directory captures.

| Path | Front-door | Use it when |
|---|---|---|
| **[`standalone/`](standalone/)** | **bundled** (Caddy, automatic HTTPS) | You want one self-contained, internet-facing host. **Start here.** |
| repo-root `docker-compose.yml` | external (a Caddy you already run) | You already operate a shared reverse proxy on a `web` network (the maintainer "halcyon" setup). |
| cloud / cluster | — | Future work — see [below](#future-cloud--cluster). |

The rest of this README documents the **standalone** stack. It exposes loom to
the internet, so its front-door and auth settings are wired for untrusted
exposure out of the box.

## What the standalone stack runs

[`standalone/docker-compose.yml`](standalone/docker-compose.yml) brings up three
services:

- **caddy** — a [Caddy](https://caddyserver.com) front-door. It is the only
  service that publishes ports (`80`/`443`). It obtains and renews a real TLS
  certificate for your domain automatically, terminates TLS, and reverse-proxies
  everything to loom — including the WebSocket terminal. See
  [`standalone/Caddyfile`](standalone/Caddyfile).
- **loom-init** — a one-shot that seeds the security-relevant auth settings into
  the database before loom starts (see [Security posture](#security-posture)),
  then exits.
- **loom** — the loom daemon, bound to `0.0.0.0:7878` *inside* the compose
  network only. It is never published to the host; the only way in is through
  Caddy.

loom drives the agents your sessions run (`gh`, `git`, `claude`, `uv`, an
embedded VS Code), so the image is large and self-contained.

## Prerequisites

- A host with Docker and the Compose plugin, reachable from the internet on
  ports **80 and 443**.
- A **domain** whose DNS `A`/`AAAA` record points at the host. Caddy needs `:80`
  reachable for the ACME challenge and your domain resolving to the host *before*
  first start, or the certificate won't issue.
- The credentials in the [env reference](#required-environment) — at minimum a
  `GH_TOKEN`, a webhook secret, and an Anthropic key (or an interactive Claude
  login).

## Quick start

```sh
cd deploy/standalone
cp .env.example .env
$EDITOR .env                 # fill in LOOM_DOMAIN, GH_TOKEN, the secrets…

docker compose up -d --build # builds the image, then starts the stack
docker compose logs -f caddy # watch the certificate get issued
```

Then open `https://<LOOM_DOMAIN>` and [log in](#first-run-login).

To validate the config without starting anything:

```sh
docker compose config -q                              # compose parses
docker run --rm -v "$PWD/Caddyfile:/c:ro" \
  -e LOOM_DOMAIN=loom.example.com caddy:2 \
  caddy validate --config /c --adapter caddyfile      # Caddyfile is well-formed
```

## Required environment

All of these live in `deploy/standalone/.env` (template:
[`.env.example`](standalone/.env.example)). Compose reads the file both to
interpolate the compose file and as each container's `env_file`.

| Variable | Required | Purpose |
|---|---|---|
| `LOOM_DOMAIN` | **yes** | Public domain Caddy serves and gets a cert for (e.g. `loom.team.dev`); `localhost` for local testing. Also seeded as `auth.base_url`. |
| `LOOM_OWNER_GITHUB` | **yes** | GitHub login seeded as the first approved user on a fresh database. |
| `GH_TOKEN` | **yes** | GitHub token loom uses to clone private repos, push branches, and reply to `@loom` comments. |
| `LOOM_GITHUB_WEBHOOK_SECRET` | for `@loom` | Shared secret for the inbound webhook; must match the secret on the GitHub webhook. Until set, the webhook rejects every delivery. |
| `ANTHROPIC_API_KEY` | for Claude | API key for the Claude agents. Alternatively log in interactively (see [first-run](#claude-authentication)). |
| `HOST_UID` / `HOST_GID` | no (1000) | uid:gid the image runs as — matters only if you bind-mount a host dir. |
| `LOOM_TLS_EMAIL` | no | ACME contact for cert-expiry notices; only used if you uncomment the global block in the Caddyfile. |
| `LOOM_GITHUB_CLIENT_ID` / `_SECRET` | for login | GitHub OAuth app — the owner's only way to sign in on a fresh DB (see [first-run](#first-run-login)). Callback: `https://<LOOM_DOMAIN>/api/auth/github/callback`. |
| `LOOM_IMAGE` | no | Override the image tag (defaults to the locally-built `loom:latest`). |

## Security posture

This deploy is reachable by anyone on the internet, so two auth settings must
differ from their single-user defaults. The **loom-init** service sets them in
the database automatically on every `up`, so the secure posture is not something
you can forget to apply:

- **`auth.trust_loopback = false`** — off, the default trusts requests from the
  loopback interface as the machine owner with no login. (Caddy reaches loom over
  the compose network, not loopback, so proxied traffic is unaffected either way;
  turning it off is defense-in-depth against anything that can reach the daemon
  locally.)
- **`auth.cookie_secure = true`** — marks the login cookie `Secure` so the
  browser only sends it over the HTTPS Caddy terminates.
- **`auth.base_url = https://<LOOM_DOMAIN>`** — the canonical public origin, used
  for the GitHub OAuth callback and trusted by the terminal's WebSocket
  origin check.

Background on these knobs is in the repo
[README "Authentication"](../README.md#authentication) and
[docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md). To change one after the fact:

```sh
docker compose exec loom weaver config set auth.trust_loopback false
```

Access past the front-door is then gated by GitHub/password login for the UI and
bearer tokens for automation — see the repo README.

## First-run login

The only account that can log in on a fresh database is `LOOM_OWNER_GITHUB`, and
it has no password yet — so **GitHub sign-in is the way in**. Since the loopback
trust that bootstraps a local install is off here (see above), set up OAuth
before first start:

1. Register a GitHub OAuth app with callback
   `https://<LOOM_DOMAIN>/api/auth/github/callback`, and put its credentials in
   `.env` as `LOOM_GITHUB_CLIENT_ID` / `LOOM_GITHUB_CLIENT_SECRET`
   (`docker compose up -d` to apply).
2. Open `https://<LOOM_DOMAIN>` and **Continue with GitHub** as
   `LOOM_OWNER_GITHUB`.
3. Once in, approve teammates, set a password for password-login, and mint
   automation tokens under **Settings → Account / Tokens**.

For automation, the `loom` CLI inside the container is already authenticated as
the owner (via the machine-local token loom injects), so it works without a
login — e.g. `docker compose exec loom loom token add ci`.

### Claude authentication

If you did not set `ANTHROPIC_API_KEY`, log in to Claude once interactively; the
credentials persist in `~/.claude.json` on the `loom_home` volume:

```sh
docker compose exec loom claude    # follow the prompts, then exit
```

## Wire the `@loom` GitHub trigger

Commenting **`@loom work on this`** on an issue launches a session. To enable it:

1. **Register the repo** in loom's managed store (the clone allowlist). From the
   host, using a token minted under Settings → Tokens:

   ```sh
   curl -X POST https://<LOOM_DOMAIN>/api/repos \
     -H "Authorization: Bearer $LOOM_TOKEN" \
     -H 'content-type: application/json' \
     -d '{"repo":"owner/name"}'
   ```

2. **Add the webhook** on the repo or org (**Settings → Webhooks → Add webhook**):
   - **Payload URL** — `https://<LOOM_DOMAIN>/api/github/webhook`
   - **Content type** — `application/json`
   - **Secret** — the same value as `LOOM_GITHUB_WEBHOOK_SECRET`
   - **Events** — *Let me select individual events* → **Issue comments** only

Full behaviour, authorization rules, and hardening notes:
[docs/github-trigger.md](../docs/github-trigger.md).

## Where state lives

Everything persistent is in named Docker volumes (survive `up`/`down`/recreate;
removed only by `docker compose down -v` or `docker volume rm`):

| Volume | Mount | Holds |
|---|---|---|
| `loom_home` | `/home/app` | The sqlite db (`~/.weaver/weaver.db`), the machine-local loom token, `~/.claude.json`, and the managed repo store (`WEAVER_REPOS_DIR`, default `~/.weaver/repos`) — the repos the `@loom` trigger clones and their worktrees. |
| `uv` | `/opt/uv` | uv's managed Python interpreters and wheel cache. |
| `caddy_data` | `/data` | Issued TLS certificates and the ACME account — back this up to avoid re-issuing on every fresh host. |
| `caddy_config` | `/config` | Caddy's autosaved config. |

The managed repo store lives inside `loom_home` (not its own volume) so the
non-root `app` user the image runs as can write to it: a fresh standalone named
volume would mount root-owned. To put repos on their own disk, point
`WEAVER_REPOS_DIR` at a **bind mount** of a host directory you've `chown`ed to
`HOST_UID:HOST_GID` instead.

## Operations

```sh
docker compose ps                       # service status
docker compose logs -f loom             # daemon logs
docker compose exec loom bash           # a shell in the container
docker compose pull caddy && \
  docker compose up -d --build          # update: rebuild loom, refresh Caddy
docker compose down                     # stop (keeps volumes/state)
docker compose down -v                  # stop and DELETE all state
```

## Future: cloud / cluster

A multi-host cloud/cluster deploy is intentionally **not** included here. It is
not a manifest exercise: loom sessions are stateful PTYs (a long-lived terminal
per session) that do not survive being rescheduled, so spreading sessions across
workers is coupled to the per-session isolation model (Model 2 in the shared-loom
design, weaver issue #337), not to packaging. That is a separate design. Until
then, the standalone single-host stack is the supported way to run a shared team
loom.
