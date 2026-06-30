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
is installed on it (the installation *is* the grant — installing the App on a repo
authorizes it, and loom auto-registers it into the store on first trigger).

Register a repo explicitly with:

```sh
curl -X POST {base}/api/repos -H 'Authorization: Bearer $LOOM_TOKEN' \
  -H 'content-type: application/json' -d '{"repo":"owner/name"}'
```

A comment on a repo that is neither registered nor App-installed launches
nothing.

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
installations as the access allowlist. Tokens are signed from the App's private
key (an RS256 JWT exchanged for an installation token via
`POST /app/installations/{id}/access_tokens`) and cached until they near expiry.

When the App is **not configured**, loom falls back to the ambient `GH_TOKEN`
(the `gh` CLI), so the webhook works without it. The webhook receiver and its
security controls (HMAC, idempotency, authorization) are identical either way —
the App only changes which credential the two outbound GitHub calls use.

### Create the App

Under **Settings → Developer settings → GitHub Apps → New GitHub App**:

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
