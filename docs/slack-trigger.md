# Slack trigger (`/marinbot`)

loom turns a Slack slash command or mention into a session. Type **`/marinbot
<prompt>`** anywhere, or **`@marinbot <prompt>`** in a channel or thread, and
loom pulls the surrounding conversation, launches a session against a repo,
and replies in-thread with a link to the live session (`On it — {base}/s/{id}`).
That reply is the session's **status card**: as the agent reports progress
with `weaver status`, loom edits the message in place into a live trail — the
Slack analog of the [GitHub `@loom` trigger](github-trigger.md)'s status
comment (see [The status card](#the-status-card)).

The transport is inverted from GitHub's: instead of receiving an inbound,
HMAC-verified webhook, loom is an **outbound [Socket Mode]** websocket client
— there is no public URL to expose or secret to verify a signature against.
The connection self-gates on configuration: it only opens once both Slack
tokens are set and the `slack.enabled` kill switch is on (see [Configure the
tokens](#configure-the-tokens)). In place of GitHub's signature check, two
authorization gates protect the trigger — the event's workspace must be the
one the app is installed in, and the Slack user must be on an explicit
allowlist (see [Who can trigger](#who-can-trigger)).

[Socket Mode]: https://docs.slack.dev/apis/events-api/using-socket-mode/

## How it works

A background task holds the socket open for the whole process lifetime,
reconnecting with jittered backoff on any error. Once connected, for every
frame it receives, in order:

1. **ACKs within budget.** Slack requires an `envelope_id` echo within 3
   seconds of a payload-bearing frame (`slash_commands`, `events_api`); loom
   sends that first and handles the trigger in a detached task, the same
   reason GitHub's webhook handler returns `200` before finishing a clone.
2. **Parses the trigger.** A `slash_commands` payload becomes a thread-blind
   trigger (see [Anchor](#the-status-card)); an `events_api` payload is kept
   only for `app_mention` — other event types, and any event from the bot's
   own user id, are dropped without dispatch (a self-trigger guard, since
   loom subscribes to `app_mention` only, not `message.*` — see [Slack app
   configuration](#slack-app-configuration)).
3. **Dedupes.** Socket Mode delivery is *at-least-once* — a missed ACK or a
   reconnect boundary redelivers a frame — so loom keeps the same delivery
   ledger the GitHub webhook uses, keyed on Slack's `event_id` (a mention) or
   `trigger_id` (a slash command). A replay is a no-op.
4. **Authorizes.** The event's `team_id` must match the workspace loom's bot
   token belongs to, and the Slack user id must be on the [allowed-users
   list](#who-can-trigger). Either failing drops the trigger — the second
   with a reply nudging the user to ask an admin, rather than a silent drop.
5. **Resolves the repo** from an `owner/name:` prefix on the command text, or
   the `slack.default_repo` setting (see [Which repo](#which-repo)). Neither
   set gets a reply asking for one.
6. **Reuses or launches.** A thread that already has a live session attached
   is acknowledged — a 👀 reaction on the mention, or an "Already working on
   this thread." reply for a slash command — rather than launched again.
   Otherwise loom clones/resolves the repo, pulls conversation history to
   seed the session goal (the thread's replies for a mention, the channel's
   recent messages for a slash command, capped at 40 messages), and creates
   the session on a stable `slack-<hash>` branch derived from the thread
   identity, so a later trigger on the same thread finds the same branch.
7. **Wires and replies.** The branch is tagged with the thread's identity (the
   `slack` tag — see [The status card](#the-status-card)), and loom posts (or,
   for a slash command, edits its placeholder into) the "On it" card.

## The status card

The "On it" reply doubles as the thread's live view of the session, exactly
as the GitHub comment does. At launch the trigger wires the branch to the
thread — a `slack` tag whose value is `team_id/channel_id/thread_ts` — and
records the card message's `ts`. From then on, every `weaver status <level>
"<message>"` the agent writes re-renders that message via `chat.update`:

> On it — <{base}/s/{id}>
> Docs: <{base}/s/{id}/artifacts/design|design>
>
> • 🟢 `Jul 18 21:04` reading the thread; mapping the code
> • 🟠 `Jul 18 22:15` *attention* — ready for review

Up to 15 bullets show in full (oldest first); older ones collapse into a
single `… N earlier update(s)` line rather than growing the message
unbounded. If the tracked message was deleted, loom posts a fresh one and
re-records its `ts` — the same recreate-on-drop behavior as the GitHub card.

Where a trigger anchors differs by shape: a **slash command's payload carries
no thread reference at all**, so it can only start a new thread — loom posts
a placeholder card first and that message's own `ts` becomes the thread root.
An **`@marinbot` mention** continues whatever thread it was typed in
(`thread_ts`), or starts one at its own `ts` if it was posted at the top
level. Either way, the card is edited in place — edits don't renotify — so a
busy thread's status trail never spams the channel.

## Who can trigger

Two gates, both deny-by-default:

- **Workspace.** The socket is authenticated as one workspace's bot, but
  events still carry an explicit `team_id` — Slack Connect delivers events
  from external, shared-channel teams over the same connection, so every
  trigger's `team_id` is checked against the bot's own before anything else
  runs. An event from another workspace is rejected outright.
- **`slack.allowed_users`** — a space- or comma-separated list of Slack user
  IDs (`U0123ABCD`, not display names) permitted to trigger. A session is
  privileged — it holds repo and agent credentials — so this defaults to
  empty, meaning **no one** can launch until it's set, even from inside the
  installed workspace:

  ```sh
  loom config set slack.allowed_users "U0123ABCD U0456EFGH"
  ```

  Also settable in **Settings → Slack**. Membership in a channel or workspace
  admin status is not by itself a grant — the same principle as GitHub's
  approved-users list being separate from repo write access.

## Which repo

The command text may start with a bare `owner/name:` prefix — exactly one
slash, both halves plain path atoms — naming the repo for that trigger:

```
/marinbot acme/web: fix the flaky login test
```

Without a prefix, loom falls back to **`slack.default_repo`**, since a Slack
conversation has no repo of its own the way a GitHub issue does:

```sh
loom config set slack.default_repo "acme/web"
```

With neither, the trigger replies asking for one rather than guessing.

## Configure the tokens

Two secrets enable the integration — set **both**, or it stays idle
(`slack.enabled` is a kill switch, not the enabler; token presence is):

- **`LOOM_SLACK_APP_TOKEN`** (`loom.toml`'s `slack_app_token`) — the
  app-level token (`xapp-…`) that opens the Socket Mode connection. Needs the
  `connections:write` scope (see [Slack app
  configuration](#slack-app-configuration)).
- **`LOOM_SLACK_BOT_TOKEN`** (`loom.toml`'s `slack_bot_token`) — the bot-user
  OAuth token (`xoxb-…`) every Web API call (`chat.postMessage`,
  `conversations.history`, …) authenticates as.

Both are held outside the settings registry — like the GitHub webhook secret
and App private key — so `GET /api/settings` never returns them. Set them
through the environment, or in `loom.toml` (see
[`loom.toml.example`](../loom.toml.example)) and run `loom config render-env`
to fold them into the deploy's `.env`.

On the GCP deploy, the tokens travel the same handoff as every other
credential: put them in `loom.toml` and push with

```sh
PROJECT=hai-gcp-models deploy/gcp/secrets.py
```

which re-renders the single `LOOM_DOTENV` blob the VM's startup script
fetches on boot (see [`deploy/gcp/README.md` "Credential
handoff"](../deploy/gcp/README.md#credential-handoff)). Don't push a Slack
token as its own standalone Secret Manager secret — the startup script only
ever reads `LOOM_DOTENV`, so a separately-stored secret is simply never
fetched.

`slack.enabled` (default on) closes the socket without discarding the tokens
— use it to pause the integration without losing configuration. It, along
with `slack.allowed_users` and `slack.default_repo`, lives in **Settings →
Slack**.

## The reply route

A session's own replies — a question, a design to review, the finished
result — post back to the wired thread through `POST
/api/branches/{branch}/slack/reply` with `{"text": "…"}` and the session's
`LOOM_TOKEN`. loom resolves the destination channel and thread from the
branch's `slack` wiring tag server-side; the bot token itself never reaches
the agent, the same separation the GitHub trigger keeps between an agent's
`GH_TOKEN` and any App-level credential.

## Slack app configuration

Under **Settings → Socket Mode**, enable it and generate an app-level token
with the **`connections:write`** scope — that's `LOOM_SLACK_APP_TOKEN`.

Under **OAuth & Permissions → Bot Token Scopes**, add:

- `commands` — receive the `/marinbot` slash command.
- `app_mentions:read` — receive `@marinbot` mentions.
- `chat:write` — post and edit the status card.
- `reactions:write` — the 👀 acknowledgment on a reused thread.
- History, per conversation type the bot should read: `channels:history`
  (public), `groups:history` (private), `im:history` (DMs), `mpim:history`
  (group DMs). Only add the types you intend to use it in.

Under **Slash Commands**, create `/marinbot` — Socket Mode delivers it over
the open connection, so it needs no Request URL.

Under **Event Subscriptions**, subscribe to **`app_mention`** only.
Subscribing to `message.*` as well is deliberately avoided: it would deliver
every message in a watched conversation, including the bot's own status-card
posts and edits, back over the same socket.

Two things are easy to miss after any of the above:

- **Reinstall the app** to the workspace after changing scopes — Slack does
  not apply a new scope to an existing installation's token until you do.
- **Invite the bot** to each channel it should trigger from or read history
  in — `/invite @marinbot`. The bot scopes above grant *capability*; channel
  membership is what makes a specific conversation reachable. A trigger from
  a channel the bot hasn't been invited to still authorizes, but seeding the
  session fails to read history (the reply notes it couldn't read the
  conversation and to invite the bot).

See [`crates/loom/src/slack.rs`](../crates/loom/src/slack.rs) for the
implementation this document describes.
