# GitHub trigger (`@loom work on this`)

loom turns an issue comment into a session. Comment **`@loom work on this`** on a
GitHub issue and loom launches a session against that repo — seeded from the
issue — and replies on the issue with a link to the live session
(`On it — {base}/s/{id}`).

This is an internet-exposed, untrusted-input endpoint. Two gates protect it:
every delivery is verified cryptographically (HMAC), and the commenter is
authorized before anything is launched.

## How it works

GitHub delivers `issue_comment` events to `POST /api/github/webhook`. The
receiver, in order:

1. **Verifies the signature.** It recomputes HMAC-SHA256 over the raw request
   body with the webhook secret and constant-time-compares it to
   `X-Hub-Signature-256`. A missing or wrong signature is rejected with **401**.
2. **Dedupes** on `X-GitHub-Delivery`. A replayed or GitHub-retried delivery is a
   no-op, so a repeat never launches a second session.
3. **Filters** to `issue_comment` / `action == created`. Edits, deletions, other
   events, and the bot's own comments (set `github.bot_login`) are ignored.
4. **Matches the command.** The comment must *begin* with the trigger phrase
   (`github.trigger_phrase`, default `@loom work on this`), matched
   case-insensitively. Fixed phrase only — no free-text in v1.
5. **Authorizes the commenter.** They must be a known loom operator (their GitHub
   login is on the allowlist) **or** have write/admin permission on the repo
   (checked via `GET /repos/{owner}/{repo}/collaborators/{login}/permission`).
   Anyone else is silently ignored. A per-repo rate limit blunts comment spam.
6. **Resolves the repo** through the [managed repo store](#repo-allowlist) — only
   an allowlisted repo is cloned and launched against — and creates the session,
   seeded with the issue title and body.
7. **Replies** on the issue with the session URL.

The permission check (step 5) and the reply (step 7) reach GitHub through the
[GitHub App](#the-github-app) when one is configured — with a short-lived,
per-installation token — and otherwise through the `gh` CLI's ambient
`GH_TOKEN`.

Everything past the signature check returns **200** whether or not it launched a
session (a non-trigger comment, a replay, an unauthorized commenter, an
unregistered repo, a rate-limited repo), so GitHub does not retry a delivery loom
deliberately ignored.

## Configure the webhook

This section is the classic, App-less path: a shared secret plus a manually
added repo/org webhook. If you're setting up the [GitHub App](#the-github-app)
(the recommended path — `loom setup github-app` does it in one step), skip
straight there: the App's own webhook delivers events for any repo it's
installed on, so there's no separate webhook to add.

Set a shared secret in loom's environment (it is held outside the settings
registry and never returned by `GET /api/settings`, like the OAuth client
secret):

```sh
LOOM_GITHUB_WEBHOOK_SECRET=<a long random string>
```

Add a webhook on the repo or org (**Settings → Webhooks → Add webhook**):

- **Payload URL** — `{base}/api/github/webhook`, where `{base}` is loom's public
  URL (the same one `auth.base_url` names).
- **Content type** — `application/json`.
- **Secret** — the same value as `LOOM_GITHUB_WEBHOOK_SECRET`.
- **Events** — "Let me select individual events" → **Issue comments** only.

Until `LOOM_GITHUB_WEBHOOK_SECRET` is set the endpoint rejects every delivery
(it cannot verify a signature without it).

### Repo allowlist

The trigger only acts on allowlisted repos. A repo is allowlisted when **either**
it is registered in the managed repo store **or** the [GitHub App](#the-github-app)
is installed on it **and** its owner is a [trusted owner](#trusted-owner-allowlist)
— in which case loom auto-registers it into the store on first trigger.

Register a repo explicitly with:

```sh
curl -X POST {base}/api/repos -H 'Authorization: Bearer $LOOM_TOKEN' \
  -H 'content-type: application/json' -d '{"repo":"owner/name"}'
```

A comment on a repo that is neither registered nor an App-installed repo under a
trusted owner launches nothing.

### Trusted-owner allowlist

An installation counts as a grant only when the installing account is a **trusted
owner** — a GitHub org or user in the `github_owners` allowlist. This is what makes
the App safe to run **public**: GitHub only lets a *private* App be installed on
the account that owns it, so wiring loom across an account boundary pushes you to
make the App public, at which point anyone can install it. Anchoring auto-trust in
an explicit owner list — rather than in "an installation exists" — keeps a
stranger's installation from ever auto-registering their repo.

The list is seeded at first run from the deploy owner (`LOOM_OWNER_GITHUB`) plus
`LOOM_ALLOWED_OWNERS` (comma/space-separated), and the setup wizard adds the
account the App was created under. Operators manage it in **Settings → Authorized
GitHub owners** or over the API:

```sh
curl -X POST {base}/api/github/owners -H 'Authorization: Bearer $LOOM_TOKEN' \
  -H 'content-type: application/json' -d '{"login":"your-org"}'
```

Explicitly registering a repo (`POST /api/repos`) is unaffected — that is an
operator's deliberate act, already gated by the operator allowlist.

### Optional settings

- `github.trigger_phrase` — the phrase a comment must begin with
  (default `@loom work on this`). Settable in **Settings → GitHub** or
  `weaver config set github.trigger_phrase "…"`.
- `github.bot_login` (or `LOOM_GITHUB_BOT_LOGIN`) — a GitHub login whose own
  comments are ignored, so a bot account can post without re-triggering itself.

## The GitHub App

A **GitHub App** is the hardened identity loom acts through. Instead of a
long-lived, broadly-scoped shared `GH_TOKEN`, loom mints a **short-lived,
least-privilege installation token** scoped to a single repo's installation for
each GitHub call (the permission check and the reply), and treats the App's
installations under a [trusted owner](#trusted-owner-allowlist) as the access
allowlist. Tokens are signed from the App's private key (an RS256 JWT exchanged
for an installation token via `POST /app/installations/{id}/access_tokens`) and
cached until they near expiry.

When the App is **not configured**, loom falls back to the ambient `GH_TOKEN`
(the `gh` CLI), so the webhook works without it. The webhook receiver and its
security controls (HMAC, idempotency, authorization) are identical either way —
the App only changes which credential the two outbound GitHub calls use.

### Create the App

The fast path is the guided wizard, which drives GitHub's [manifest
flow](https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest):

```sh
loom setup github-app --base-url https://loom.team.dev
```

It opens a local page that auto-submits the App configuration to GitHub, waits
for your one confirmation click, exchanges the redirect for the full credential
set (App id, private key, webhook secret, OAuth client id/secret), writes them
into the typed `loom.toml` (along with `LOOM_DOMAIN` and, unless `--org` is
set, `LOOM_OWNER_GITHUB`), and — when it can reach the running daemon's
database — live-applies them too, no restart needed for that part. The App's
`callback_urls` are set to loom's GitHub login callback too, so the same App's
OAuth client also covers "Sign in with GitHub" (see
[deploy/README.md "First-run login"](../deploy/README.md#first-run-login)) —
one registration, not two. See `loom setup github-app --help` for `--org` (App
under an organization instead of your account), `--port` (pin a port when
tunnelling into a remote host), and `--config` (where to write `loom.toml`;
defaults to `./loom.toml` or `$LOOM_CONFIG`). `loom config render-env` turns
`loom.toml` into a deploy `.env` — see
[deploy/README.md "First-run login"](../deploy/README.md#first-run-login).

To register by hand instead, under **Settings → Developer settings → GitHub
Apps → New GitHub App**:

- **Webhook** — set the **URL** to `{base}/api/github/webhook` (loom's public URL,
  the same `auth.base_url` names) and the **Secret** to the value you put in
  `LOOM_GITHUB_WEBHOOK_SECRET`.
- **Repository permissions:**
  - **Issues** — Read & write (read the comment, post the reply).
  - **Contents** — Read & write (clone the repo, and push the work branch).
  - **Metadata** — Read-only (mandatory; granted automatically).
- **Subscribe to events** — **Issue comment**.
- After creating it, **generate a private key** (downloads a `.pem`) and note the
  **App ID**.

Then **install** the App on each repo (or org) loom should act on — the
installation is what authorizes a repo to trigger.

### Configure loom

`loom setup github-app` already does this (see [above](#create-the-app)) — this
section is for the manual path, or to move the credentials to a different host.
Provide the App id and private key through the environment (both held outside the
settings registry, never returned by `GET /api/settings`, like the OAuth client
secret):

```sh
LOOM_GITHUB_APP_ID=<the App ID>
# The PEM, newlines preserved (e.g. "$(cat app.private-key.pem)").
LOOM_GITHUB_APP_PRIVATE_KEY=<the private-key PEM>
```

With both set, loom uses the App for the permission check and the reply and
treats App-installed repos as allowlisted. With either unset it falls back to the
ambient `GH_TOKEN`. (Cloning still uses the ambient git credentials; granting the
App **Contents** access keeps a single least-privilege credential set as the
deploy migrates off the shared PAT.)
