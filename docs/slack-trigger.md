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
tokens](#configure-the-tokens)). In place of GitHub's signature check, the
trigger is protected by the workspace the app is installed in and the channels
its bot has been invited to (see [Who can trigger](#who-can-trigger)).

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
   only for `app_mention`. Parsing is structural — *who may act on the result*
   is step 4's decision, so every rejection is one loom can log, count, and
   show rather than a silent drop.
3. **Dedupes.** Socket Mode delivery is *at-least-once* — a missed ACK or a
   reconnect boundary redelivers a frame — so loom keeps the same delivery
   ledger the GitHub webhook uses, keyed on Slack's `event_id` (a mention) or
   `trigger_id` (a slash command). A replay is a no-op.
4. **Authorizes.** The event must come from loom's own workspace, must not be
   loom's own post, and must be typed by a person rather than posted through
   another app (see [Who can trigger](#who-can-trigger)). Every refusal is
   logged with its reason and shown as the last skipped trigger in **Settings →
   Connections**; a person excluded by an explicit allow-list also gets a reply
   telling them to ask an admin.
5. **Resolves the repo** from an `owner/name:` prefix on the command text, or
   the `slack.default_repo` setting (see [Which repo](#which-repo)). Neither
   set gets a reply asking for one.
6. **Continues or launches.** A thread that already has a live session attached
   receives the new request in that session's conversation, then is
   acknowledged — a 👀 reaction on the mention, or a short confirmation for a
   slash command — rather than launched again. If the recorded session is
   unreachable, loom archives it and relaunches on the kept branch so the
   request is not dropped. Otherwise loom clones/resolves the repo, pulls
   conversation history to seed the session goal (up to 40 replies for a
   mention inside a thread; for a top-level mention or slash command, up to 10
   preceding channel messages explicitly labeled as potentially unrelated),
   and creates the session on a stable `slack-<hash>` branch derived
   from the thread identity, so a later trigger on the same thread finds the
   same branch.
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
>
> • 🟢 `Jul 18 21:04` reading the thread; mapping the code
> • 🟠 `Jul 18 22:15` *attention* — ready for review

Up to 15 bullets show in full (oldest first); older ones collapse into a
single `… N earlier update(s)` line rather than growing the message
unbounded. A relaunched session on the same conversation starts a new trail;
the branch's older status history is not repeated. If the tracked message was
deleted, loom posts a fresh one and re-records its `ts` — the same
recreate-on-drop behavior as the GitHub card.

Artifact links are off by default: the thread should receive a self-contained
answer, and an internal design document is rarely useful conversation context.
`slack.status_artifacts` can opt a deployment back in. The progress trail,
header template, and trigger profile are registered settings
(`slack.status_updates`, `slack.status_header_template`, and `slack.profile`),
configurable through both the runtime Settings API and a deployment manifest.
Organization instructions belong on the selected profile, where the same
mechanism also covers GitHub, user, delegated, and automation sessions. The
older `slack.prompt_instructions` remains as an additive compatibility overlay.
See [Configuration policy](configuration.md).

Loom seeds an editable `slack` starter profile from the generic default with a
small, deployment-neutral instruction document. It remains opt-in so existing
installations keep their current profile environment and runtime until an
operator selects it.

Where a trigger anchors differs by shape: a **slash command's payload carries
no thread reference at all**, so it can only start a new thread — loom posts
a placeholder card first and that message's own `ts` becomes the thread root.
An **`@marinbot` mention** continues whatever thread it was typed in
(`thread_ts`), or starts one at its own `ts` if it was posted at the top
level. Either way, the card is edited in place — edits don't renotify — so a
busy thread's status trail never spams the channel.

The launch prompt supplies Slack context, the fixed reply route, and the final
status contract. It does not choose an organization's answer/change, landing,
review, or CI workflow; those conventions come from the selected profile and
the target repository. The agent must replace intermediate progress with a
terminal status and post a self-contained reply in the thread.

## Who can trigger

By default the boundary is the one Slack already enforces: **anyone in the
installed workspace, in a conversation the bot has been invited to.** Both
halves are real grants — someone has an account in this workspace, and someone
ran `/invite @marinbot` in that channel — and Socket Mode only delivers
`app_mention` from channels the bot is in, so there is no second list of opaque
user IDs to keep in sync with the team.

Three rules hold regardless, and none of them is configurable:

- **One workspace.** The socket is authenticated as one workspace's bot, but
  events still carry an explicit `team_id` — Slack Connect delivers events from
  external, shared-channel teams over the same connection, so every trigger's
  `team_id` is checked against the bot's own before anything else runs. An event
  from another workspace is rejected outright.
- **Never itself.** loom does not trigger on its own posts.
- **People, not apps.** A message posted through another app carries a `bot_id`
  while keeping the human's user ID. Those do not trigger, so an alerting bot
  whose text happens to contain `@marinbot` cannot launch a privileged session.

To narrow it further, list the Slack user IDs (`U0123ABCD`, not display names)
allowed to trigger:

```sh
loom config set slack.allowed_users "U0123ABCD U0456EFGH"
```

Also settable in **Settings → Slack**. A listed ID is trusted even when it posts
through another app, which is how an approved automation identity opts in.

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

  It must be the **bot** token, not the user token (`xoxp-…`) issued by the same
  install. A user token authenticates, connects, and passes `auth.test` exactly
  like the bot token — but it resolves to a *person*, so loom posts as them and
  discards every mention they type as its own. **Settings → Connections** names
  this outright; `auth.test` returns a `bot_id` only for the bot token, and that
  is what the pane checks.

Both are held outside the settings registry — like the GitHub webhook secret
and App private key — so `GET /api/settings` never returns them. Set them
through the environment, or in `loom.toml` (see
[`loom.toml.example`](../loom.toml.example)) and run `loom config render-env`
to fold them into the deploy's `.env`.

For `loom.oa.dev`, the tokens travel with every other credential in the
versioned `LOOM_DOTENV` payload managed by the
[Marin `infra/loom` runbook](https://github.com/marin-community/marin/tree/main/infra/loom).
Do not create standalone Slack-token secrets that the deployment never reads.

`slack.enabled` (default on) closes the live socket within ten seconds without
discarding the tokens — use it to pause the integration without losing
configuration. It, along with `slack.allowed_users`, `slack.default_repo`, and
`slack.idle_archive_secs`, lives in **Settings → Slack**. Presentation and
profile-selection settings live there as well. `slack.profile` defaults to
`default`; `slack.effort` defaults Slack-origin sessions to `high` reasoning
effort, while `agent-default` preserves the selected profile's effort. Locked
or incompatible profiles fall back automatically.

## Diagnosing it

**Settings → Connections** shows the whole trigger path in the order the server
checks it — tokens, switch, connection, identity, who can trigger, repository —
so a broken link names itself. A live socket is not the same as a working
integration: the connection can be up while the bot token belongs to a person,
or while no repository is configured, and the pane distinguishes those.

`GET /api/slack/status` returns the same structure for scripting. Alongside it,
the server log carries one line per arriving envelope (`slack: envelope
received`, with the event type, channel, and user), the app id from the `hello`
frame, and a reason for every trigger that did not become a session. The most
recent of those reasons is also kept on the pane, since a mention that is
silently dropped otherwise looks exactly like a mention that never arrived.

## Placement and retention

Slack-origin sessions are placed in **Slack → Inbox**. Sessions created before
this space existed move there only when they are still in the fallback
**User → Inbox**; an operator's manual placement is preserved.

The retention monitor archives a Slack-origin session after
`slack.idle_archive_secs` without session activity (default `86400`, one day).
Activity includes agent output and user prompts through Loom, so an active
conversation keeps extending the deadline. A live ACP turn is never
interrupted. Archiving removes the agent, terminal, and worktree while keeping
the branch, conversation, artifacts, and history recoverable. Set the value to
`0` to disable Slack retention globally, or disable auto-archive on one session
from its actions menu.

## The reply route

A session's own replies — a question, a design to review, the finished
result — post back to the wired thread through `POST
/api/branches/{branch}/slack/reply` with `{"text": "…"}` and the session's
`LOOM_TOKEN`. loom resolves the destination channel and thread from the
branch's `slack` wiring tag server-side; the bot token itself never reaches
the agent, the same separation the GitHub trigger keeps between an agent's
`GH_TOKEN` and any App-level credential.

Adding `"thread": {"channel": "C…", "thread_ts": "…"}` posts to one of the
session's *routed* threads instead (see [Automation-delivered
threads](#automation-delivered-threads)). The route is the authorization: a
thread that was never delivered to this branch is refused, so the field selects
among the session's own threads rather than granting it the workspace. The
`slack_reply` MCP tool takes the same optional `thread`.

## Automation-delivered threads

A `POST /api/runs` body may carry `"slack": {"channel": "C…", "thread_ts": "…"}`
— the thread the caller announced this delivery in. loom **routes** that thread
to whichever session the run lands on, and from then on the thread and the
session are joined in both directions:

- The session may reply into the thread (`thread` on the reply route above).
- An `@marinbot` mention in the thread is delivered into that session's
  conversation and acknowledged with 👀, rather than launching a second session
  on top of it. A routed thread needs no repository: the session already exists,
  so `slack.default_repo` is not consulted.

This is a **many-to-one** relation, which is why it is not the `slack` tag.
That tag fixes *one* thread as a session's status-card home, the right model for
a session born from a conversation. An operator session is the other shape: one
long-lived session fed alerts through an [automation
channel](configuration.md), each alert announced in its own thread. Its `slack`
tag is deliberately left alone — a single card cannot follow a session that is
triaging several incidents at once — so a routed thread gets explicit replies
rather than a live trail.

Routes are delivery records, not caller-chosen grants: loom writes one only
where it accepted a run for that thread, the workspace is always loom's own, and
`channel`/`thread_ts` are shape-checked (a `#channel-name` is rejected — Slack
ids only). Re-delivering the same alert is idempotent. `followups_routed` in
`GET /api/slack/status` counts mentions delivered this way, which is how a
working alert conversation is distinguished from one quietly launching a
duplicate session per reply.

The intended consumer is Marin's Grafana bridge: it posts the alert to Slack,
creates the run with that thread, and the operator session answers in-thread —
see the [Marin `infra/grafana`
runbook](https://github.com/marin-community/marin/tree/main/infra/grafana).

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

See [`crates/loom-deliver/src/slack.rs`](../crates/loom-deliver/src/slack.rs) for the
implementation this document describes.
