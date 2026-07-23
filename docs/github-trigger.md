# GitHub trigger (`@loom`)

loom turns an issue or PR comment into a session. Comment **`@loom`** on a GitHub
issue or pull request and loom launches a session against that repo — attached to
the PR's own branch (so the agent's commits land on the PR) or, for an issue, a
stable `weaver/issue-<n>` branch — seeded with the thread's context, and replies
with a link to the live session (`On it — {base}/s/{id}`). That reply is the
session's **status card**: as the agent reports progress with `weaver status`,
loom edits the comment in place into a live trail (see [The status
card](#the-status-card)). A follow-up `@loom` on a thread that already has a
running session is handed to that session instead of starting a second one.

This is an internet-exposed, untrusted-input endpoint. Two gates protect it:
every delivery is verified cryptographically (HMAC), and the commenter is
authorized — against the [approved-user allowlist](#who-can-trigger) — before
anything is launched.

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
4. **Matches the trigger.** The comment must tag the trigger phrase
   (`github.trigger_phrase`, default `@loom`), matched case-insensitively
   **anywhere** in the comment's prose — `please @loom rebase this` fires just as
   `@loom rebase this` does. Two kinds of text don't count as tagging, because
   they quote or discuss the phrase rather than address the bot with it: quoted
   lines (`>`), so a quote-reply pasting an earlier `@loom` never re-fires the
   thread; and code, fenced or inline, so a comment asking whether `` `@loom` ``
   still works launches nothing. The mention must stand alone — `@loom-bot` and
   `me@loom.dev` are not `@loom`.
5. **Authorizes the commenter.** Their GitHub login must be an
   [approved loom user](#who-can-trigger) — the *same* allowlist that gates
   signing in to the app. Repo write access is **not** by itself a grant. An
   unapproved commenter gets a one-line "request access" reply — rather than a
   silent drop, so they know to ask instead of assuming loom is broken. A per-repo
   rate limit bounds both the launch and that reply against comment spam.
6. **Resolves the repo** through the [managed repo store](#which-repos) — an
   approved user's trigger on any repo the App is installed on registers it — and
   picks the branch to work on: a **pull request** comment attaches the session's
   worktree to the PR's head branch, so the agent's commits push straight to the
   PR; an **issue** comment gets a stable `weaver/issue-<n>` branch. (A PR from a
   fork, whose head loom can't push to, falls back to a fresh branch.)
7. **Reuses or creates.** If an active session already owns that branch, the new
   comment is forwarded into it (a nudge in its terminal) rather than launching a
   duplicate. Otherwise loom creates the session, seeded with the issue/PR title,
   body, and the triggering comment, plus a primer on how to respond — push to the
   PR branch (or open a PR for an issue) and reply on the thread with `gh`.
8. **Replies** on the thread with the session URL. A forwarded comment is
   acknowledged with a 👀 **reaction** on the triggering comment instead — seen,
   passed along, no ack comment accumulating on an active thread. When the
   reaction can't land (no comment id in the delivery, or a token that can't
   react), loom falls back to the ack comment so the feedback isn't lost.

Steps 6–8 (clone, create-or-forward, reply) run in a **detached task**: the
handler returns `200` as soon as the gates pass. Cloning a large repo can outlast
GitHub's ~10s delivery timeout, and a timed-out delivery would cancel an inline
handler mid-clone. Each launch is tracked on **Settings → Debug** (running / done
/ error, with the outcome), so you can follow it after the `200`.

The reply (step 8) reaches GitHub through the [GitHub App](#the-github-app) when
one is configured — with a short-lived, per-installation token — and otherwise
through the `gh` CLI's ambient `GH_TOKEN`. The **session itself** acts as the
commenter: its `GH_TOKEN` is that user's personal token (**Settings → Account**),
falling back to its selected profile when they have none — so its pushes and
`gh` replies use the configured session identity. Separately, the poll loop
posts a one-time back-link comment (`Working on this in loom: {base}/s/{id}`) on
a session's open PR when one isn't already linked, so a reader of the PR can
jump to the session.

## The status card

The "On it" reply doubles as the thread's live view of the session. At launch
the trigger **wires** the branch to the thread — a quiet `github` tag whose
value is `owner/name#number` — and records the reply's comment id. From then
on, every `weaver status <level> "<message>"` the agent writes re-renders that
comment:

> On it — {base}/s/{id}
> Docs: [design]({base}/s/{id}/artifacts/design)
>
> - 🟢 `Jul 18 21:04Z` reading the thread; mapping the code
> - 🟠 `Jul 18 22:15Z` **attention** — ready for review

One comment, edited in place — comment edits notify no one, so subscribers are
never spammed; a reader opening the thread sees the whole arc, plus links to
the documents the agent has published (the dashboard's artifact viewer). The
agent still posts real comments when it needs a person — a question, a design
review, the result — and those do notify.

The mirroring works on any session, not just triggered ones: `weaver tag set
github owner/name#123` wires a session by hand (the card appears on its next
status report), and `weaver tag rm github` stops the mirroring, freezing the
comment. The card requires a configured `auth.base_url` (the session link must
resolve for a GitHub reader) and posts through the same App-token/`gh`
credential ladder as the reply. Two loom-internal tags do the bookkeeping —
`github.status_comment` (the comment id the card lives in) and `github.linked`
(the PR back-link dedupe); both are machine-owned, hidden from the dashboard's
pill row, and refused by the tag-set routes (clearing one is harmless: loom
re-creates its state).

If a follow-up `@loom` lands on a thread whose session's terminal is gone, the
relaunch posts a fresh "On it" card, marks the old one *superseded*, and — the
wiring survives the relaunch — the new card carries the full trail.

Everything past the signature check returns **200** whether or not it launched a
session (a non-trigger comment, a replay, an unregistered or rate-limited repo,
or an unauthorized commenter — who gets the access-request reply), so GitHub does
not retry a delivery loom handled without launching.

## Who can trigger

The people who can trigger `@loom` are exactly the people who can sign in to
loom: the **approved users** (the `users` table). One allowlist governs both
surfaces — being approved lets someone sign in *and* drive the trigger by
commenting; no one else can do either. Write access to the repo is **not** by
itself a grant, so opening a repo to loom never hands the trigger to everyone who
can push to it.

The first approved user is seeded from `LOOM_OWNER_GITHUB` on a fresh database.
Manage the rest in **Settings → Approved users** or over the API (set
`github_login` so the person can both sign in with GitHub and trigger):

```sh
curl -X POST {base}/api/auth/users -H 'Authorization: Bearer $LOOM_TOKEN' \
  -H 'content-type: application/json' \
  -d '{"username":"octocat","github_login":"octocat"}'
```

> **Extending this later.** Today an approved user is an explicit login. A
> role-scoped rule — e.g. "admins of org `acme`" — would slot into the same
> authorization step ([`github_trigger::authorize`]), evaluated against the
> GitHub API; it is not implemented yet.

[`github_trigger::authorize`]: ../crates/loom/src/github_trigger.rs

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

### Which repos

The trigger acts on repos in the managed repo store. A repo lands there one of
two ways:

- **An approved user triggers on it.** When an approved user comments `@loom` on a
  repo the [GitHub App](#the-github-app) is installed on, loom registers that repo
  automatically and launches. The *person* is the trust boundary — since only an
  approved user can trigger, a stranger installing the public App on their own
  repo changes nothing (no approved user will comment there). The App installation
  scopes *which* repos loom can reach; the approved-user gate decides *whether* it
  acts.
- **You register it explicitly.** An operator's deliberate act, independent of the
  App:

  ```sh
  curl -X POST {base}/api/repos -H 'Authorization: Bearer $LOOM_TOKEN' \
    -H 'content-type: application/json' -d '{"repo":"owner/name"}'
  ```

A comment on a repo that is neither registered nor one the App is installed on
launches nothing.

### Optional settings

- `github.trigger_phrase` — the phrase that tags loom (default `@loom`). Matched
  case-insensitively anywhere in a comment's prose, as a standalone mention
  ([step 4](#how-it-works)) — both `@loom rebase onto main` and `can you rebase
  this, @loom?` trigger. Settable in **Settings → GitHub** or
  `loom config set github.trigger_phrase "…"`.
- `github.bot_login` (or `LOOM_GITHUB_BOT_LOGIN`) — a GitHub login whose own
  comments are ignored, so a bot account can post without re-triggering itself.

## The GitHub App

A **GitHub App** is the hardened identity loom acts through. Instead of a
long-lived, broadly-scoped shared `GH_TOKEN`, loom mints a **short-lived,
least-privilege installation token** scoped to a single repo's installation for
the reply, and treats the repos the App is installed on as the set it can reach
(the [approved-user gate](#who-can-trigger) decides whether it acts). Tokens are
signed from the App's private key (an RS256 JWT exchanged for an installation
token via `POST /app/installations/{id}/access_tokens`) and cached until they
near expiry.

When the App is **not configured**, loom falls back to the ambient `GH_TOKEN`
(the `gh` CLI), so the webhook works without it. The webhook receiver and its
security controls (HMAC, idempotency, authorization) are identical either way —
the App only changes which credential the outbound reply uses.

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
one registration, not two.

Run it again on an instance that already has an App and it offers to **update**
it instead of creating a second one: it opens the App's GitHub settings page to
adjust permissions (which loom can't change itself — you edit them there, then
re-approve on each installation) or the install page to add repositories, or you
can choose to replace the App with a fresh one.

See `loom setup github-app --help` for `--org` (App
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

With both set, loom uses the App for the reply and reaches any repo the App is
installed on (registered on an approved user's first trigger). With either unset
it falls back to the ambient `GH_TOKEN`. (Cloning still uses the ambient git
credentials; granting the App **Contents** access keeps a single least-privilege
credential set as the deploy migrates off the shared PAT.)

## Debugging the trigger

The trigger is a **webhook, not a poll** — "nothing happened" always means the
`POST` either never arrived or was dropped at one of the gates above. Nothing
picks a comment up later, so debugging is one question: *did the delivery arrive,
and which gate did it hit?* Work through it in order:

1. **Was the comment the right shape?** It must be a PR/issue **conversation**
   comment (an `issue_comment`) that tags the trigger phrase in its prose — a
   mention that sits only inside a quote or a code span is ignored by design
   ([step 4](#how-it-works)). An inline code-review comment and a review summary
   are different events loom does not subscribe to, so `@loom` in those never
   fires.

2. **Did GitHub deliver it, and what did loom answer?** The GitHub App's
   **Settings → Advanced → Recent Deliveries** (or the repo/org webhook's) shows
   every delivery, its payload, and loom's HTTP response:
   - *no delivery* — the App is not installed on the repo, or it was not an
     `issue_comment`;
   - *401* — the webhook secret loom holds does not match the one GitHub signs
     with;
   - *200 but nothing launched* — it hit a gate (below), or the launch is still
     running / failed in the background; read the logs and the **Debug** page's
     background-task list, which shows each launch's outcome.

   `scripts/gh_app_deliveries.py` prints the same delivery log from the command
   line (it mints an App JWT from the key in `loom.toml`).

3. **Read the server logs.** The quickest path is **Settings → Debug** in the web
   UI — a live, filterable mirror of the server's log stream (plus the
   background-task list), so you can watch a delivery land without shelling into
   the box (handy on the Docker deploy). The
   same lines go to the process stdout, so `docker compose logs -f loom` (or
   `RUST_LOG=loom=debug` for the outbound `gh`/REST calls) works too. Each gate
   logs a distinct line — look for: `signature verification failed` (401, secret
   mismatch), `duplicate delivery ignored` (a replay — see below), `commenter not
   authorized` (not an approved user), `session create failed` (repo not
   registered, or the clone failed), `per-repo rate limit hit`, and the success
   line `launched session`.

### Reproduce without a PR or a deploy

The webhook is just a signed HTTP `POST`, so you can exercise the whole handler
without touching a PR or redeploying. `scripts/loom_webhook_replay.py` signs a
synthetic `issue_comment` with the webhook secret and posts it, minting a fresh
delivery GUID each run so it is never dropped as a duplicate:

```sh
export LOOM_GITHUB_WEBHOOK_SECRET=<the secret>
# against a local dev loom:
scripts/loom_webhook_replay.py --url http://127.0.0.1:8080 \
  --repo owner/name --author your-login --body '@loom rebase onto main'
# or replay a payload captured from Recent Deliveries:
scripts/loom_webhook_replay.py --url http://127.0.0.1:8080 --payload delivery.json
```

GitHub's own **Redeliver** button re-sends the exact payload, but it reuses the
original `X-GitHub-Delivery` GUID — which loom deduplicates *before* any business
logic — so redelivering an already-processed delivery is a no-op. Use Redeliver
to retry a delivery that 401'd (a rejected delivery is never recorded); use the
replay script (fresh GUID) to re-exercise the logic.
