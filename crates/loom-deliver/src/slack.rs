//! Slack integration — Socket Mode.
//!
//! The Slack analog of the GitHub `@loom` trigger ([`crate::github_trigger`] +
//! [`crate::github`]), with the transport inverted: instead of receiving an
//! inbound, HMAC-verified webhook, loom is an **outbound websocket client**. A
//! background task ([`run`]) opens a [Socket Mode] connection with the app-level
//! token, receives `slash_commands` / `app_mention` envelopes, and — for an
//! authorized trigger — pulls the conversation, launches a session, and replies
//! in-thread with a live "On it" card. As the session reports `weaver status`,
//! [`sync_status_message`] edits that Slack message in place, exactly as
//! [`crate::github::sync_status_comment`] edits the GitHub comment.
//!
//! The socket is authenticated (the app token) and single-workspace, so there is
//! no HMAC to verify — but delivery is still *at-least-once* (a missed 3-second
//! ACK or a reconnect boundary redelivers), so we keep GitHub's
//! [`crate::github_trigger::record_delivery`] dedupe, keyed on Slack's `event_id`.
//!
//! [Socket Mode]: https://docs.slack.dev/apis/events-api/using-socket-mode/

use std::sync::RwLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use weaver_core::{config, events, tags};

use crate::AppState;
use crate::Db;

/// Env var / settings key for the app-level token (`xapp-…`). Held outside the
/// settings registry (like the GitHub webhook secret) so `GET /api/settings`
/// never returns it; set it through the environment or `loom config`.
pub const APP_TOKEN_ENV: &str = "LOOM_SLACK_APP_TOKEN";
pub const APP_TOKEN_KEY: &str = "slack.app_token";
/// Env var / settings key for the bot-user OAuth token (`xoxb-…`).
pub const BOT_TOKEN_ENV: &str = "LOOM_SLACK_BOT_TOKEN";
pub const BOT_TOKEN_KEY: &str = "slack.bot_token";

/// Branch tag wiring a session to a Slack thread — see [`tags::SLACK_KEY`].
pub const WIRED_TAG: &str = tags::SLACK_KEY;
/// Branch tag holding the status card's message `ts` — see
/// [`tags::SLACK_STATUS_MSG_KEY`].
pub const STATUS_MSG_TAG: &str = tags::SLACK_STATUS_MSG_KEY;

/// How many trail bullets the status card shows before older ones collapse into
/// a count line — matches the GitHub card's [`crate::github`] cap.
const STATUS_CARD_CAP: usize = 15;
/// Cap on how many conversation messages seed a session, so a busy channel can't
/// produce an unbounded launch prompt.
const HISTORY_CAP: usize = 40;

// ---------------------------------------------------------------------------
// Live supervisor health.
// ---------------------------------------------------------------------------

/// What the Socket Mode supervisor is doing right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketState {
    /// A token is missing, or `slack.enabled` is off. Nothing is listening.
    #[default]
    Idle,
    /// Opening the websocket.
    Connecting,
    /// Live — envelopes are arriving on this connection.
    Connected,
    /// The last attempt failed; the supervisor is backing off before retrying.
    Failed,
}

/// What the supervisor has seen, for the Connections pane and the logs. An
/// `auth.test` probe only proves a credential works; this proves the socket is
/// actually up and shows what it has done with what arrived.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SocketHealth {
    pub state: SocketState,
    /// The app this connection belongs to, from the `hello` frame's
    /// `connection_info`. The one way to tell — from inside — that the app token
    /// and the bot token belong to the same Slack app.
    pub app_id: Option<String>,
    pub connected_at: Option<String>,
    pub last_error: Option<String>,
    pub last_event_at: Option<String>,
    pub events_received: u64,
    pub sessions_launched: u64,
    /// Why the most recent trigger was not acted on. A dropped mention is the
    /// integration's quietest failure, so it is kept where an operator looks.
    pub last_skip: Option<String>,
    pub last_skip_at: Option<String>,
}

static HEALTH: RwLock<Option<SocketHealth>> = RwLock::new(None);

/// The supervisor's current health snapshot.
pub fn health() -> SocketHealth {
    HEALTH
        .read()
        .expect("health lock")
        .clone()
        .unwrap_or_default()
}

fn update_health(f: impl FnOnce(&mut SocketHealth)) {
    let mut guard = HEALTH.write().expect("health lock");
    f(guard.get_or_insert_with(SocketHealth::default));
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// `env`, else the `key` setting; empty when neither is set. Mirrors
/// [`crate::github_trigger`]'s private resolver — kept per-module by design.
async fn env_or_setting(db: &Db, env: &str, key: &str) -> String {
    if let Ok(v) = std::env::var(env) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return v;
        }
    }
    config::get(db, key).await.unwrap_or_default()
}

/// The app-level token (`xapp-…`), empty when unset.
pub async fn app_token(db: &Db) -> String {
    env_or_setting(db, APP_TOKEN_ENV, APP_TOKEN_KEY).await
}

/// The bot token (`xoxb-…`), empty when unset.
pub async fn bot_token(db: &Db) -> String {
    env_or_setting(db, BOT_TOKEN_ENV, BOT_TOKEN_KEY).await
}

/// Whether the integration is switched on: both tokens present *and*
/// `slack.enabled` not turned off. Token presence is the real enabler (a deploy
/// that ships the tokens Just Works); `slack.enabled` is a kill switch that
/// closes the socket without removing the tokens.
pub async fn is_enabled(db: &Db) -> bool {
    !app_token(db).await.is_empty()
        && !bot_token(db).await.is_empty()
        && config::get_bool(db, "slack.enabled", true).await
}

/// The Slack Web API base — overridable via `LOOM_SLACK_API_BASE` so a test can
/// point the gateway at a local fixture. Defaults to the real API.
fn api_base() -> String {
    std::env::var("LOOM_SLACK_API_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://slack.com/api".to_string())
}

/// Who may launch a session from Slack.
///
/// Two gates hold regardless of this setting and are not configurable: the
/// event must come from the workspace the app is installed in (which excludes
/// Slack Connect channels shared with an external org), and loom never triggers
/// on its own posts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// The default. Any person in the installed workspace, in a conversation the
    /// bot has been invited to. Slack's own two grants — workspace membership
    /// and the channel invite — are the boundary, so there is no second list of
    /// opaque user ids to maintain. Posts from *other apps* are excluded: an
    /// alerting bot that happens to say `@loom` must not launch a privileged
    /// session.
    Workspace,
    /// Only these Slack user ids, from `slack.allowed_users`. A tighter boundary
    /// for a large or mixed workspace. A listed id is trusted even when it posts
    /// through another app's user token, which is how an approved automation
    /// identity opts in.
    Listed(Vec<String>),
}

/// Read the configured access boundary. `slack.allowed_users` empty ⇒
/// [`Access::Workspace`].
pub async fn access(db: &Db) -> Access {
    let listed: Vec<String> = config::get(db, "slack.allowed_users")
        .await
        .unwrap_or_default()
        .split([' ', ',', '\n', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if listed.is_empty() {
        Access::Workspace
    } else {
        Access::Listed(listed)
    }
}

/// Why a trigger was not acted on. Every variant is logged, counted, and shown
/// in the Connections pane — a silently dropped mention is indistinguishable
/// from a broken socket, which is the failure this whole path exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// From another Slack workspace, including a Slack Connect channel shared
    /// with an external org.
    OtherWorkspace,
    /// loom's own post. Never trigger on ourselves.
    Own,
    /// Posted through another app rather than typed by a person, under
    /// [`Access::Workspace`].
    Automation,
    /// Not on `slack.allowed_users`, under [`Access::Listed`].
    NotListed,
}

impl Denial {
    /// The one-line reason recorded in [`SocketHealth::last_skip`] and the log.
    pub fn reason(&self) -> &'static str {
        match self {
            Denial::OtherWorkspace => "event came from another Slack workspace",
            Denial::Own => "loom's own message",
            Denial::Automation => {
                "posted by an app, not a person (add its user id to slack.allowed_users to allow it)"
            }
            Denial::NotListed => "user is not on slack.allowed_users",
        }
    }

    /// What to say in-thread, or `None` to drop silently. Only a person who
    /// could have meant to trigger gets a reply. Replying to our own posts would
    /// loop, replying to an app would answer a machine, and replying into a
    /// foreign workspace would talk to an org loom does not serve.
    pub fn reply(&self) -> Option<&'static str> {
        match self {
            Denial::NotListed => Some(
                "You're not on this bot's allowed-users list — ask an admin to add your Slack user id.",
            ),
            Denial::Own | Denial::Automation | Denial::OtherWorkspace => None,
        }
    }
}

/// Decide whether a parsed trigger may launch a session. Pure, so the whole
/// policy is unit-testable without a database or a socket.
pub fn authorize(access: &Access, bot: &AuthTest, trigger: &Trigger) -> Result<(), Denial> {
    if trigger.user_id == bot.user_id {
        return Err(Denial::Own);
    }
    // The socket is one workspace, but events still carry an explicit team_id and
    // Slack Connect delivers events from external teams.
    if trigger.team_id != bot.team_id {
        return Err(Denial::OtherWorkspace);
    }
    match access {
        Access::Workspace if trigger.via_app => Err(Denial::Automation),
        Access::Workspace => Ok(()),
        Access::Listed(users) if users.iter().any(|u| u == &trigger.user_id) => Ok(()),
        Access::Listed(_) => Err(Denial::NotListed),
    }
}

// ---------------------------------------------------------------------------
// The Web API gateway.
// ---------------------------------------------------------------------------

/// A message pulled from conversation history — the fields we seed a session
/// with. `user` is `None` for a non-user message (a bot post, a system join).
#[derive(Debug, Clone)]
pub struct HistMsg {
    pub user: Option<String>,
    pub text: String,
}

/// The Slack Web API gateway: a thin `reqwest` client bound to the bot token.
/// Cheap to build per use, so [`sync_status_message`] and the trigger handler
/// each construct one rather than threading a shared object through `AppState`.
#[derive(Clone)]
pub struct SlackWeb {
    http: reqwest::Client,
    bot_token: String,
    base: String,
}

async fn slack_response(method: &str, response: reqwest::Response) -> Result<Value> {
    // HTTP 429 carries a `Retry-After` (seconds); surface it so the caller
    // can back off rather than hammer a rate-limited method.
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("1");
        return Err(anyhow!(
            "slack {method}: rate limited (retry after {retry}s)"
        ));
    }
    let value: Value = response
        .json()
        .await
        .with_context(|| format!("slack {method}: decoding response failed"))?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(anyhow!("slack {method}: {err}"));
    }
    Ok(value)
}

impl SlackWeb {
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            bot_token: bot_token.into(),
            base: api_base(),
        }
    }

    /// Build from the configured bot token, or `None` when it isn't set.
    pub async fn from_db(db: &Db) -> Option<Self> {
        let token = bot_token(db).await;
        (!token.is_empty()).then(|| Self::new(token))
    }

    /// POST `method` with a JSON body and a Bearer token, returning the parsed
    /// response. Slack signals application errors with `{"ok": false, "error":
    /// …}` under an HTTP 200, so a 200 is *not* success — every call checks `ok`.
    async fn call(&self, method: &str, body: Value, token: &str) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}/{method}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("slack {method}: request failed"))?;
        slack_response(method, resp).await
    }

    /// Bot-token call (the common case).
    async fn call_bot(&self, method: &str, body: Value) -> Result<Value> {
        let token = self.bot_token.clone();
        self.call(method, body, &token).await
    }

    /// GET `method` with query parameters and the bot token. Slack documents
    /// both JSON and form encodings for conversation reads, but
    /// `conversations.replies` currently ignores a JSON POST body and reports
    /// its required fields as missing. Query parameters work consistently for
    /// both history endpoints.
    async fn get_bot(&self, method: &str, query: &[(&str, &str)]) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/{method}", self.base))
            .bearer_auth(&self.bot_token)
            .query(query)
            .send()
            .await
            .with_context(|| format!("slack {method}: request failed"))?;
        slack_response(method, resp).await
    }

    /// Resolve our own identity — the bot user id (to skip our own events), the
    /// workspace `team_id` (the authorization boundary), and whether the token
    /// is really a bot token.
    pub async fn auth_test(&self) -> Result<AuthTest> {
        let v = self.call_bot("auth.test", json!({})).await?;
        Ok(AuthTest {
            user_id: v["user_id"].as_str().unwrap_or_default().to_string(),
            team_id: v["team_id"].as_str().unwrap_or_default().to_string(),
            bot_id: v["bot_id"].as_str().map(str::to_string),
        })
    }

    /// Post a message, optionally threaded under `thread_ts`. Returns the new
    /// message's `ts`.
    pub async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<String> {
        let mut body = json!({ "channel": channel, "text": text, "unfurl_links": false });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        let v = self.call_bot("chat.postMessage", body).await?;
        Ok(v["ts"].as_str().unwrap_or_default().to_string())
    }

    /// Edit a message in place. `Ok(false)` means the message is gone (deleted),
    /// so the caller re-posts — mirroring the GitHub card's recreate path.
    pub async fn update_message(&self, channel: &str, ts: &str, text: &str) -> Result<bool> {
        let body = json!({ "channel": channel, "ts": ts, "text": text, "unfurl_links": false });
        match self.call_bot("chat.update", body).await {
            Ok(_) => Ok(true),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("message_not_found") || msg.contains("cant_update_message") {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Add an emoji reaction (best-effort ack). Swallows `already_reacted`.
    pub async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<()> {
        let body = json!({ "channel": channel, "timestamp": ts, "name": name });
        match self.call_bot("reactions.add", body).await {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("already_reacted") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// One page of a thread's replies, oldest-first (Slack returns them in
    /// order). Best-effort: a `not_in_channel` (the bot was never invited)
    /// surfaces as the error so the caller can tell the user.
    pub async fn conversations_replies(&self, channel: &str, ts: &str) -> Result<Vec<HistMsg>> {
        let limit = HISTORY_CAP.to_string();
        let v = self
            .get_bot(
                "conversations.replies",
                &[("channel", channel), ("ts", ts), ("limit", &limit)],
            )
            .await?;
        Ok(parse_history(&v))
    }

    /// The channel's recent top-level messages. Slack returns newest-first, so
    /// the result is reversed to chronological order.
    pub async fn conversations_history(&self, channel: &str) -> Result<Vec<HistMsg>> {
        let limit = HISTORY_CAP.to_string();
        let v = self
            .get_bot(
                "conversations.history",
                &[("channel", channel), ("limit", &limit)],
            )
            .await?;
        let mut msgs = parse_history(&v);
        msgs.reverse();
        Ok(msgs)
    }

    /// Open a Socket Mode connection with the **app-level** token, returning the
    /// short-lived `wss://` URL. The URL's `ticket` query param is a live
    /// credential — never log it.
    pub async fn open_connection(&self, app_token: &str) -> Result<String> {
        let v = self
            .call("apps.connections.open", json!({}), app_token)
            .await?;
        v["url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("apps.connections.open: no url in response"))
    }
}

/// The identity `auth.test` resolves at startup.
#[derive(Debug, Clone)]
pub struct AuthTest {
    pub user_id: String,
    pub team_id: String,
    /// Set only for a real bot token. `auth.test` answers for a human's user
    /// token (`xoxp-…`) too — with that person's user id and no `bot_id` — so a
    /// deployment handed the wrong token connects, reports healthy, and then
    /// discards every mention its operator types as loom's own. This field is
    /// what tells the two apart.
    pub bot_id: Option<String>,
}

impl AuthTest {
    /// Whether the configured token is a bot token rather than a user token.
    pub fn is_bot(&self) -> bool {
        self.bot_id.is_some()
    }
}

/// Extract `{user, text}` messages from a `conversations.*` response.
fn parse_history(v: &Value) -> Vec<HistMsg> {
    v["messages"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let text = m["text"].as_str().unwrap_or_default().trim().to_string();
                    if text.is_empty() {
                        return None;
                    }
                    Some(HistMsg {
                        user: m["user"].as_str().map(str::to_string),
                        text,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Envelope + trigger parsing (pure — the unit-tested core).
// ---------------------------------------------------------------------------

/// Where a trigger's session thread is anchored. Slack slash commands are
/// *thread-blind* (their payload has no `ts`/`thread_ts`), so a slash command
/// can only start a new root — the card's own `ts` becomes the thread. A mention
/// anchors on the thread it was typed in, or on its own `ts` at top level.
#[derive(Debug, Clone, PartialEq)]
pub enum Anchor {
    /// A `/marinbot` slash command — post the card as a new root message.
    Slash,
    /// An `@marinbot` mention. `root_ts` is the thread to attach to (the
    /// mention's own `ts` at top level, or its `thread_ts` inside a thread), and
    /// `event_ts` is the mention message itself (what we 👀-react to as an ack).
    Mention { root_ts: String, event_ts: String },
}

/// A parsed, deduped trigger ready to authorize and act on.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    pub team_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub text: String,
    pub anchor: Anchor,
    /// The id we dedupe on (`event_id` for events; the slash `trigger_id`).
    pub dedupe_id: String,
    /// The source message carried a `bot_id`: posted through an app rather than
    /// typed by a person. Slack keeps the human's user id on such a message, so
    /// this is the only signal that separates the two.
    pub via_app: bool,
}

/// A decoded Socket Mode envelope.
#[derive(Debug, Clone, PartialEq)]
pub enum Envelope {
    /// The connection is live. `app_id` comes from `connection_info` and is the
    /// only in-band evidence of *which* Slack app the app-level token opened —
    /// the check that catches an app token and a bot token from two different
    /// apps, a pairing that otherwise connects and then receives nothing.
    Hello { app_id: Option<String> },
    /// Slack is about to close this socket. `reason` is `warning` (a ~10s
    /// pre-close notice), `refresh_requested`, or `link_disabled`.
    Disconnect { reason: String },
    /// A payload-bearing envelope (`events_api` / `slash_commands` /
    /// `interactive`) that must be ACKed by echoing `envelope_id`.
    Payload {
        envelope_id: String,
        kind: String,
        payload: Value,
    },
    /// Anything else — acked if it carries an `envelope_id`, else ignored.
    Other { envelope_id: Option<String> },
}

/// Decode a raw Socket Mode frame.
pub fn parse_envelope(v: &Value) -> Envelope {
    match v["type"].as_str().unwrap_or_default() {
        "hello" => Envelope::Hello {
            app_id: v["connection_info"]["app_id"].as_str().map(str::to_string),
        },
        "disconnect" => Envelope::Disconnect {
            reason: v["reason"].as_str().unwrap_or_default().to_string(),
        },
        kind @ ("events_api" | "slash_commands" | "interactive") => {
            match v["envelope_id"].as_str() {
                Some(id) => Envelope::Payload {
                    envelope_id: id.to_string(),
                    kind: kind.to_string(),
                    payload: v["payload"].clone(),
                },
                None => Envelope::Other { envelope_id: None },
            }
        }
        _ => Envelope::Other {
            envelope_id: v["envelope_id"].as_str().map(str::to_string),
        },
    }
}

/// Build a [`Trigger`] from a `slash_commands` payload. Slash payloads have no
/// thread anchor (see [`Anchor::Slash`]).
pub fn trigger_from_slash(payload: &Value) -> Option<Trigger> {
    let team_id = payload["team_id"].as_str()?.to_string();
    let channel_id = payload["channel_id"].as_str()?.to_string();
    let user_id = payload["user_id"].as_str()?.to_string();
    let text = payload["text"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    // `trigger_id` is unique per invocation — a stable dedupe key.
    let dedupe_id = payload["trigger_id"]
        .as_str()
        .map(|t| format!("slash:{t}"))
        .unwrap_or_else(|| format!("slash:{team_id}:{channel_id}:{user_id}:{text}"));
    Some(Trigger {
        team_id,
        channel_id,
        user_id,
        text,
        anchor: Anchor::Slash,
        dedupe_id,
        // A slash command is always typed by a person.
        via_app: false,
    })
}

/// Build a [`Trigger`] from an `events_api` payload, for `app_mention` only.
/// Returns `None` for any other event type.
///
/// Parsing is structural only — who may act on the result is
/// [`authorize`]'s decision, so every rejection is one that can be logged and
/// shown rather than a silent `None` here.
pub fn trigger_from_event(payload: &Value) -> Option<Trigger> {
    let event = &payload["event"];
    if event["type"].as_str() != Some("app_mention") {
        return None;
    }
    let user_id = event["user"].as_str()?.to_string();
    let team_id = payload["team_id"].as_str().unwrap_or_default().to_string();
    let channel_id = event["channel"].as_str()?.to_string();
    let event_ts = event["ts"].as_str()?.to_string();
    // Anchor on the enclosing thread if there is one, else the mention itself.
    let root_ts = event["thread_ts"].as_str().unwrap_or(&event_ts).to_string();
    let text = strip_leading_mention(event["text"].as_str().unwrap_or_default());
    let dedupe_id = payload["event_id"]
        .as_str()
        .map(|e| format!("slack:{e}"))
        .unwrap_or_else(|| format!("slack:{channel_id}:{event_ts}"));
    Some(Trigger {
        team_id,
        channel_id,
        user_id,
        text,
        anchor: Anchor::Mention { root_ts, event_ts },
        dedupe_id,
        via_app: event.get("bot_id").is_some(),
    })
}

/// Drop a leading `<@U…>` bot mention (and following whitespace) from an
/// app_mention's text, leaving just the user's instruction.
fn strip_leading_mention(text: &str) -> String {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix('<') {
        if let Some((mention, tail)) = rest.split_once('>') {
            if mention.starts_with('@') {
                return tail.trim_start().to_string();
            }
        }
    }
    t.to_string()
}

/// Split an optional `owner/name:` repo prefix off the command text. Returns
/// `(repo, remaining_text)`. Only a bare `owner/name:` at the very start counts
/// — anything else leaves the text untouched and the repo `None`.
pub fn parse_repo_prefix(text: &str) -> (Option<String>, String) {
    let trimmed = text.trim_start();
    if let Some((head, rest)) = trimmed.split_once(':') {
        let head = head.trim();
        // owner/name: exactly one slash, and both halves look like path atoms.
        if let Some((owner, name)) = head.split_once('/') {
            let atom = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            };
            if head.matches('/').count() == 1 && atom(owner) && atom(name) {
                return (Some(head.to_string()), rest.trim_start().to_string());
            }
        }
    }
    (None, text.trim().to_string())
}

// ---------------------------------------------------------------------------
// Slack mrkdwn rendering.
// ---------------------------------------------------------------------------

/// Escape the three characters Slack mrkdwn treats specially in text spans, so
/// an agent-controlled status note can't inject links or `<url|label>` markup.
/// (Slack's documented escaping is exactly `&`, `<`, `>`.)
fn escape_mrkdwn(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One rendered bullet of the status trail. Mirrors the GitHub card's
/// [`crate::github`] `status_bullet`, but emits Slack mrkdwn: single-`*` bold
/// (Slack renders `**` literally) and escaped text.
fn status_bullet(event: &events::Event) -> Option<String> {
    let value = event.data["value"].as_str().unwrap_or_default();
    let note = event.data["note"].as_str().unwrap_or_default().trim();
    let loud = !value.is_empty();
    if note.is_empty() && !loud {
        return None;
    }
    let icon = match value {
        "blocked" => "\u{1f534}",
        "attention" => "\u{1f7e0}",
        _ => "\u{1f7e2}",
    };
    let when = chrono::DateTime::parse_from_rfc3339(&event.created_at)
        .map(|t| t.format("%b %e %H:%M").to_string())
        .unwrap_or_default();
    let mut line = format!("• {icon} `{when}Z`");
    if loud {
        line.push_str(&format!(" *{value}*"));
        if !note.is_empty() {
            line.push_str(" —");
        }
    }
    if !note.is_empty() {
        line.push_str(&format!(" {}", escape_mrkdwn(note)));
    }
    Some(line)
}

/// Render the Slack status card: the "On it" header linking the session, the
/// published documents, then the `weaver status` trail (oldest-first, capped).
/// Pure, so the mrkdwn format is unit-testable. Slack link syntax is
/// `<url|label>`, not Markdown's `[label](url)`.
pub fn render_status(session_url: &str, artifacts: &[String], events: &[events::Event]) -> String {
    let bullets: Vec<String> = events
        .iter()
        .filter(|e| {
            e.kind == "tag" && e.data["key"] == tags::ATTENTION_KEY && e.data["by"] == "agent"
        })
        .filter_map(status_bullet)
        .collect();
    let mut body = format!("On it — <{session_url}>");
    if !artifacts.is_empty() {
        let links: Vec<String> = artifacts
            .iter()
            .map(|name| {
                use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
                let path = utf8_percent_encode(name, NON_ALPHANUMERIC);
                format!("<{session_url}/artifacts/{path}|{}>", escape_mrkdwn(name))
            })
            .collect();
        body.push_str(&format!("\nDocs: {}", links.join(" · ")));
    }
    if bullets.is_empty() {
        return body;
    }
    let shown = if bullets.len() > STATUS_CARD_CAP {
        let hidden = bullets.len() - STATUS_CARD_CAP;
        let mut v = vec![format!("_… {hidden} earlier update(s)_")];
        v.extend_from_slice(&bullets[bullets.len() - STATUS_CARD_CAP..]);
        v
    } else {
        bullets
    };
    format!("{body}\n\n{}", shown.join("\n"))
}

// ---------------------------------------------------------------------------
// The status-card mirror — the Slack twin of `github::sync_status_comment`.
// ---------------------------------------------------------------------------

/// Serializes card writes so two near-simultaneous status reports can't post two
/// cards or interleave an edit — the same guard the GitHub mirror uses.
static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Mirror a wired branch's status trail onto its Slack thread: render the card
/// and edit the tracked message in place — posting a fresh one the first time,
/// or again if it was deleted. A no-op for unwired branches; best-effort
/// everywhere (a Slack hiccup logs and never fails the status write that spawned
/// it). Retries transient failures — a terminal status has no later write to
/// self-heal through.
pub async fn sync_status_message(state: AppState, branch_id: String) {
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(3 * 3u64.pow(attempt - 1))).await;
        }
        if sync_status_message_once(&state, &branch_id).await {
            return;
        }
    }
    tracing::warn!(branch = %branch_id, "slack status card: giving up after retries");
}

/// One sync attempt. `true` = done (synced, or nothing to do); `false` = a
/// transient Slack failure worth retrying.
async fn sync_status_message_once(state: &AppState, branch_id: &str) -> bool {
    let wired = match tags::get(&state.db, branch_id, WIRED_TAG).await {
        Ok(Some(tag)) => tag,
        Ok(None) => return true, // unwired — nothing to mirror
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "slack status card: reading wiring tag failed");
            return true;
        }
    };
    let Some((_team, channel, _root)) = parse_wiring(&wired.value) else {
        tracing::warn!(branch = %branch_id, value = %wired.value, "slack status card: unparsable `slack` tag");
        return true;
    };
    // The card links the live session; without a public base_url the link would
    // be a loopback URL a Slack reader can't follow, so require one (a socket
    // task has no request headers to derive it from).
    let base = config::get(&state.db, "auth.base_url")
        .await
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    if base.is_empty() {
        tracing::debug!(branch = %branch_id, "slack status card: no auth.base_url; skipping");
        return true;
    }
    let session = match crate::session::active_for_branch(&state.db, branch_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return true,
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "slack status card: session lookup failed");
            return true;
        }
    };
    let Some(web) = SlackWeb::from_db(&state.db).await else {
        return true; // bot token gone — nothing to post through
    };

    let _guard = SYNC_LOCK.lock().await;
    let mut events = match events::history(&state.db, branch_id, 500).await {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "slack status card: reading event history failed");
            return true;
        }
    };
    // Statuses from before the wiring stay private — hand-wiring an old session
    // must not retroactively publish its history.
    events.retain(|e| e.created_at >= wired.set_at);
    let artifacts: Vec<String> = match crate::branch::get(&state.db, branch_id).await {
        Ok(Some(branch)) => {
            weaver_core::artifact::list_for_session(&state.db, &branch.repo_root, branch_id)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.name)
                .filter(|n| n != "goal")
                .collect()
        }
        _ => Vec::new(),
    };
    let body = render_status(
        &crate::links::session_url(&base, &session.id),
        &artifacts,
        &events,
    );

    // Trust the tracked message ts only while its note still names the current
    // wiring: a re-pointed `slack` tag must get a fresh card on the new thread,
    // never an edit of the old one.
    let tracked: Option<String> = match tags::get(&state.db, branch_id, STATUS_MSG_TAG).await {
        Ok(tag) => tag.filter(|t| t.note == wired.value).map(|t| t.value),
        Err(e) => {
            tracing::warn!(branch = %branch_id, error = %e, "slack status card: reading message tag failed");
            return true;
        }
    };
    if let Some(ts) = tracked {
        match web.update_message(&channel, &ts, &body).await {
            Ok(true) => return true,
            Ok(false) => {
                tracing::info!(channel = %channel, ts = %ts, "slack status card: message gone; posting a fresh one");
            }
            Err(e) => {
                tracing::warn!(channel = %channel, error = %e, "slack status card: update failed");
                return false;
            }
        }
    }
    // The reply is threaded under the wiring's root ts.
    let root = wired
        .value
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    match web.post_message(&channel, Some(&root), &body).await {
        Ok(ts) => {
            record_status_message(&state.db, branch_id, &wired.value, &ts).await;
            true
        }
        Err(e) => {
            tracing::warn!(channel = %channel, error = %e, "slack status card: posting failed");
            false
        }
    }
}

/// Stamp the [`STATUS_MSG_TAG`] bookkeeping tag after a card lands. The note
/// records the wiring the message belongs to — [`sync_status_message`] trusts
/// the ts only while that note matches the current `slack` tag.
pub async fn record_status_message(db: &Db, branch_id: &str, wiring: &str, ts: &str) {
    tags::set(db, branch_id, STATUS_MSG_TAG, ts, wiring, "loom")
        .await
        .ok();
}

/// Parse a [`WIRED_TAG`] value — `team_id/channel_id/thread_ts` — into its
/// parts. `None` for anything else.
pub fn parse_wiring(value: &str) -> Option<(String, String, String)> {
    let mut parts = value.trim().splitn(3, '/');
    let team = parts.next()?.trim();
    let channel = parts.next()?.trim();
    let root = parts.next()?.trim();
    if team.is_empty() || channel.is_empty() || root.is_empty() {
        return None;
    }
    Some((team.to_string(), channel.to_string(), root.to_string()))
}

/// Spawn every status-card mirror a branch is wired for. The one seam the status
/// write path (and artifact publish/delete) calls — GitHub self-gates on the
/// `github` tag, Slack on the `slack` tag, so an unwired branch is a no-op for
/// each. Fans a status change out to whichever origin thread(s) a session came
/// from; adding a third surface is one line here, not another edit at five call
/// sites.
pub fn spawn_status_mirrors(state: AppState, branch_id: String) {
    tokio::spawn(crate::github::sync_status_comment(
        state.clone(),
        branch_id.clone(),
    ));
    tokio::spawn(sync_status_message(state, branch_id));
}

// ---------------------------------------------------------------------------
// The trigger handler.
// ---------------------------------------------------------------------------

/// Serializes the reuse-or-create decision so two triggers racing on the same
/// thread can't both see "no session" and both launch. Slack trigger volume is
/// tiny, so one global lock (rather than a per-wiring map) is ample.
static CREATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Act on an authorized-shaped trigger: dedupe, authorize, resolve the repo,
/// launch (or forward), wire the branch, and post the "On it" card. Runs in a
/// detached task after the envelope is ACKed. `bot` is our own identity (its
/// `team_id` is the authorization boundary).
async fn handle_trigger(state: AppState, web: SlackWeb, bot: AuthTest, trigger: Trigger) {
    // Dedupe: a redelivered envelope (missed ACK / reconnect) is a no-op.
    match crate::github_trigger::record_delivery(&state.db, &trigger.dedupe_id).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(id = %trigger.dedupe_id, "slack: duplicate delivery ignored");
            return;
        }
        Err(e) => tracing::warn!(error = %e, "slack: dedupe insert failed; proceeding"),
    }

    if let Err(denial) = authorize(&access(&state.db).await, &bot, &trigger) {
        tracing::info!(
            user = %trigger.user_id,
            channel = %trigger.channel_id,
            reason = denial.reason(),
            "slack: trigger not authorized"
        );
        record_skip(denial.reason());
        if let Some(text) = denial.reply() {
            let _ = notify(&web, &trigger, text).await;
        }
        return;
    }

    // Resolve the repo: an `owner/name:` prefix wins, else the configured
    // default. Without either there's nothing to work on.
    let (prefix_repo, instruction) = parse_repo_prefix(&trigger.text);
    let repo = match prefix_repo {
        Some(r) => r,
        None => {
            let d = config::get(&state.db, "slack.default_repo")
                .await
                .unwrap_or_default()
                .trim()
                .to_string();
            if d.is_empty() {
                record_skip("no repository: slack.default_repo is unset and the request has no owner/name: prefix");
                let _ = notify(&web, &trigger, "No repository to work on. Prefix your request with `owner/name:` or set a Slack default repository in loom's settings.").await;
                return;
            }
            d
        }
    };

    match launch(&state, &web, &bot, &trigger, &repo, &instruction).await {
        Ok(Outcome::Launched) => update_health(|h| h.sessions_launched += 1),
        // An ack on a thread that already has a session is not a launch, and
        // counting it as one would overstate what the trigger path did.
        Ok(Outcome::AlreadyRunning) => {}
        Err(e) => {
            tracing::warn!(error = %e, repo = %repo, "slack: launch failed");
            record_skip(&format!("launch failed: {e}"));
            let _ = notify(&web, &trigger, &format!("Couldn't start a session: {e}")).await;
        }
    }
}

/// Record why a trigger did not become a session, for the Connections pane.
fn record_skip(reason: &str) {
    update_health(|h| {
        h.last_skip = Some(reason.to_string());
        h.last_skip_at = Some(now());
    });
}

/// Post a short message back to the trigger's thread (an error/ack). For a
/// mention we thread under its root; a slash command posts a new message.
async fn notify(web: &SlackWeb, trigger: &Trigger, text: &str) -> Result<()> {
    let thread = match &trigger.anchor {
        Anchor::Mention { root_ts, .. } => Some(root_ts.as_str()),
        Anchor::Slash => None,
    };
    web.post_message(&trigger.channel_id, thread, text).await?;
    Ok(())
}

/// What an authorized trigger did.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Launched,
    /// The thread already had a live session, so the trigger was acknowledged
    /// rather than acted on.
    AlreadyRunning,
}

/// The launch body: seed context, reuse-or-create the session, wire it, and post
/// the card. Serialized on [`CREATE_LOCK`].
async fn launch(
    state: &AppState,
    web: &SlackWeb,
    bot: &AuthTest,
    trigger: &Trigger,
    repo: &str,
    instruction: &str,
) -> Result<Outcome> {
    let _guard = CREATE_LOCK.lock().await;

    // Establish the thread root. A slash command is thread-blind, so we post the
    // card first and let its ts be the root; a mention already has one.
    let (root_ts, card_ts): (String, Option<String>) = match &trigger.anchor {
        Anchor::Slash => {
            let ts = web
                .post_message(&trigger.channel_id, None, "On it — starting a session…")
                .await
                .context("posting the slash-command card")?;
            (ts.clone(), Some(ts))
        }
        Anchor::Mention { root_ts, .. } => (root_ts.clone(), None),
    };

    let wiring = format!("{}/{}/{}", bot.team_id, trigger.channel_id, root_ts);
    // A short, collision-free branch name — the full identity lives in the tag
    // (slugify would truncate a raw channel/ts and collide).
    let branch_name = format!("slack-{}", short_hash(&wiring));
    let branch_ref = format!("weaver/{branch_name}");

    // Pull the conversation to seed the session.
    let history = pull_history(web, trigger).await;

    // Reuse or relaunch: does a branch already exist for this thread?
    let repo_root = crate::repo::resolve_clone(&state.db, repo, state.trigger.app())
        .await
        .map_err(|e| anyhow!("{e:?}"))?;
    let repo_root_str = repo_root.to_string_lossy().to_string();
    let existing = crate::branch::find_by_repo_branch(&state.db, &repo_root_str, &branch_ref)
        .await
        .ok()
        .flatten();
    if let Some(b) = &existing {
        // An archived session no longer counts as active, so a thread whose
        // session was archived falls straight through to a fresh launch on the
        // kept branch.
        if let Ok(Some(_)) = crate::session::active_for_branch(&state.db, &b.id).await {
            // A live session already owns this thread — don't launch a
            // second. Ack (👀 on the mention; a note for a slash) and stop.
            if let Anchor::Mention { event_ts, .. } = &trigger.anchor {
                web.add_reaction(&trigger.channel_id, event_ts, "eyes")
                    .await
                    .ok();
            } else {
                notify(web, trigger, "Already working on this thread.")
                    .await
                    .ok();
            }
            return Ok(Outcome::AlreadyRunning);
        }
    }

    let goal = slack_goal(repo, trigger, instruction, &history);
    let branch_exists = crate::git::branch_exists(&repo_root, &branch_ref).await;
    let mut req = weaver_api::dto::CreateReq {
        repo: Some(repo.to_string()),
        goal: Some(goal),
        ..Default::default()
    };
    if branch_exists || existing.is_some() {
        req.existing_branch = Some(branch_ref.clone());
    } else {
        req.name = Some(branch_name.clone());
    }

    let actor = crate::provision::Actor::producer("slack", "slack");
    let created = crate::provision::create(state.clone(), req, actor)
        .await
        .map_err(|e| anyhow!("{e:?}"))?;

    // Wire the branch to the thread — what `sync_status_message` reads to mirror
    // every `weaver status` write. Left untouched if already wired to this thread.
    let already = matches!(
        tags::get(&state.db, &created.branch.id, WIRED_TAG).await,
        Ok(Some(ref t)) if t.value == wiring
    );
    if !already {
        tags::set(
            &state.db,
            &created.branch.id,
            WIRED_TAG,
            &wiring,
            "wired by /marinbot",
            "loom",
        )
        .await
        .ok();
    }

    // Post (or reuse) the card, record its ts, then run one locked full sync so
    // an early status that landed before wiring still renders.
    let base = config::get(&state.db, "auth.base_url")
        .await
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    let card_body = if base.is_empty() {
        "On it — session started (set loom's public base URL to link it here).".to_string()
    } else {
        format!(
            "On it — <{}>",
            crate::links::session_url(&base, &created.session.id)
        )
    };
    let ts = match card_ts {
        Some(ts) => {
            web.update_message(&trigger.channel_id, &ts, &card_body)
                .await
                .ok();
            ts
        }
        None => web
            .post_message(&trigger.channel_id, Some(&root_ts), &card_body)
            .await
            .context("posting the mention card")?,
    };
    record_status_message(&state.db, &created.branch.id, &wiring, &ts).await;
    tracing::info!(session = %created.session.id, repo = %repo, channel = %trigger.channel_id, "slack: launched session");
    drop(_guard);
    sync_status_message(state.clone(), created.branch.id.clone()).await;
    Ok(Outcome::Launched)
}

#[derive(Debug, PartialEq)]
enum HistoryTarget<'a> {
    Channel,
    Thread(&'a str),
}

fn history_target(trigger: &Trigger) -> HistoryTarget<'_> {
    match &trigger.anchor {
        Anchor::Mention { root_ts, event_ts } if root_ts != event_ts => {
            HistoryTarget::Thread(root_ts)
        }
        Anchor::Mention { .. } | Anchor::Slash => HistoryTarget::Channel,
    }
}

/// Pull the conversation context to seed the session — the thread's replies for
/// a mention inside an existing thread, or the channel's recent messages for a
/// top-level mention or slash command. Best-effort: a `not_in_channel` becomes a
/// note in the seed rather than a hard failure.
async fn pull_history(web: &SlackWeb, trigger: &Trigger) -> String {
    let result = match history_target(trigger) {
        HistoryTarget::Thread(root_ts) => {
            web.conversations_replies(&trigger.channel_id, root_ts)
                .await
        }
        HistoryTarget::Channel => web.conversations_history(&trigger.channel_id).await,
    };
    match result {
        Ok(msgs) => msgs
            .iter()
            .map(|m| {
                let who = m.user.as_deref().unwrap_or("someone");
                format!("<@{who}>: {}", m.text)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Err(e) => {
            let note = e.to_string();
            if note.contains("not_in_channel") {
                "(loom's bot isn't a member of this conversation, so it couldn't read the history — invite it with /invite.)".to_string()
            } else {
                format!("(couldn't read the conversation history: {note})")
            }
        }
    }
}

/// The seed goal handed to the session — the Slack analog of GitHub's
/// `trigger_goal`.
fn slack_goal(repo: &str, trigger: &Trigger, instruction: &str, history: &str) -> String {
    let instruction = if instruction.trim().is_empty() {
        "(no explicit instruction — infer the task from the conversation)"
    } else {
        instruction.trim()
    };
    format!(
        "You've been summoned from Slack (channel {channel}) to work on `{repo}`.\n\n\
         ## Request (from <@{user}>)\n{instruction}\n\n\
         ## Conversation context\n{history}\n\n\
         ## How to respond\n\
         - Do the work on this branch and open a pull request against the default branch when it's ready.\n\
         - Your `weaver status` messages and the built-in `status_update` MCP tool are mirrored onto the Slack thread (loom edits its \"On it\" message into a live status trail), so progress reporting is automatic — write status messages for that audience.\n\
         - Reply on the thread only when you need a person — a question, a design to review, or the finished result. Prefer the built-in `slack_reply` MCP tool; if it is unavailable, POST to `$WEAVER_API/api/branches/$WEAVER_BRANCH/slack/reply` with `{{\"text\": \"…\"}}` and your `LOOM_TOKEN`.",
        channel = trigger.channel_id,
        user = trigger.user_id,
    )
}

/// A short, filesystem-safe digest of the thread identity for the branch name.
fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    hex::encode(&digest[..6])
}

// ---------------------------------------------------------------------------
// The Socket Mode supervisor.
// ---------------------------------------------------------------------------

/// The Slack background task: connect, receive/ACK/dispatch, reconnect. Always
/// spawned; self-gates on [`is_enabled`], so an unconfigured deploy idles here.
/// Never returns — any top-level error is caught, logged, and retried (the
/// server discards this task's handle and won't restart it).
pub async fn run(state: AppState) {
    let mut backoff = 1u64;
    loop {
        if !is_enabled(&state.db).await {
            // Not configured / switched off — poll occasionally for a later
            // config change rather than spin.
            update_health(|h| {
                h.state = SocketState::Idle;
                h.app_id = None;
                h.connected_at = None;
            });
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        }
        update_health(|h| h.state = SocketState::Connecting);
        match connect_and_run(&state).await {
            Ok(reason) => {
                // A clean disconnect (`warning`/`refresh_requested`) — reconnect
                // promptly, no backoff, but with a 1s floor so a socket that
                // drops the instant it opens can't spin `apps.connections.open`
                // (itself rate-limited).
                tracing::debug!(reason = %reason, "slack: reconnecting");
                update_health(|h| {
                    h.state = SocketState::Connecting;
                    h.connected_at = None;
                });
                backoff = 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, backoff, "slack: connection error; backing off");
                update_health(|h| {
                    h.state = SocketState::Failed;
                    h.connected_at = None;
                    h.last_error = Some(e.to_string());
                });
                // Jittered, capped backoff so a flapping endpoint isn't hammered
                // in lockstep.
                let jitter = rand::random::<u64>() % 500;
                tokio::time::sleep(Duration::from_millis(backoff * 1000 + jitter)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

/// How often a live socket re-reads `slack.enabled`. The switch documents
/// itself as closing a live connection, so it cannot wait for Slack's own
/// refresh (tens of minutes away) to take effect.
const KILL_SWITCH_POLL: Duration = Duration::from_secs(10);

/// One connection lifecycle: open the socket, then read/ACK/dispatch until Slack
/// asks us to disconnect or the stream ends. Returns the disconnect reason on a
/// clean close; `Err` on a connection/auth failure.
async fn connect_and_run(state: &AppState) -> Result<String> {
    use tokio_tungstenite::tungstenite::Message;

    let app_tok = app_token(&state.db).await;
    let web = SlackWeb::from_db(&state.db)
        .await
        .ok_or_else(|| anyhow!("bot token not configured"))?;
    let bot = web.auth_test().await.context("auth.test")?;
    let url = web
        .open_connection(&app_tok)
        .await
        .context("apps.connections.open")?;

    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .context("opening the socket")?;
    tracing::info!(
        bot = %bot.user_id,
        team = %bot.team_id,
        token = if bot.is_bot() { "bot" } else { "user" },
        "slack: socket connected"
    );
    // A user token authenticates and connects exactly like a bot token, then
    // silently discards every mention typed by the person it belongs to. Say so
    // once per connection rather than leaving a healthy-looking dead socket.
    if !bot.is_bot() {
        tracing::warn!(
            user = %bot.user_id,
            "slack: LOOM_SLACK_BOT_TOKEN is a user token (xoxp-…), not a bot token (xoxb-…) — \
             loom will post as that person and ignore their mentions as its own"
        );
    }
    update_health(|h| {
        h.state = SocketState::Connected;
        h.connected_at = Some(now());
        h.last_error = None;
    });

    let mut kill_switch = tokio::time::interval(KILL_SWITCH_POLL);
    kill_switch.tick().await; // the first tick completes immediately

    loop {
        let msg = tokio::select! {
            frame = ws.next() => match frame {
                Some(msg) => msg.context("reading from the socket")?,
                None => break,
            },
            _ = kill_switch.tick() => {
                if is_enabled(&state.db).await {
                    continue;
                }
                tracing::info!("slack: disabled by settings; closing the socket");
                ws.close(None).await.ok();
                return Ok("disabled".to_string());
            }
        };
        let text = match msg {
            Message::Text(t) => t.as_str().to_string(),
            Message::Ping(p) => {
                ws.send(Message::Pong(p)).await.ok();
                continue;
            }
            Message::Close(_) => return Ok("close".to_string()),
            _ => continue,
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        match parse_envelope(&frame) {
            Envelope::Hello { app_id } => {
                tracing::info!(app = app_id.as_deref().unwrap_or("?"), "slack: hello");
                update_health(|h| h.app_id = app_id);
            }
            Envelope::Disconnect { reason } => return Ok(reason),
            Envelope::Other {
                envelope_id: Some(id),
            } => {
                ws.send(Message::text(json!({ "envelope_id": id }).to_string()))
                    .await
                    .ok();
            }
            Envelope::Other { envelope_id: None } => {}
            Envelope::Payload {
                envelope_id,
                kind,
                payload,
            } => {
                // ACK first — within Slack's 3s budget — then act detached.
                ws.send(Message::text(
                    json!({ "envelope_id": envelope_id }).to_string(),
                ))
                .await
                .ok();
                // Every arriving envelope is logged. Whether loom is receiving
                // at all is the first question a stalled integration raises, and
                // it used to be unanswerable from the outside.
                let event_type = payload["event"]["type"].as_str().unwrap_or("-");
                tracing::info!(
                    kind = %kind,
                    event = %event_type,
                    channel = payload["event"]["channel"]
                        .as_str()
                        .or_else(|| payload["channel_id"].as_str())
                        .unwrap_or("-"),
                    user = payload["event"]["user"]
                        .as_str()
                        .or_else(|| payload["user_id"].as_str())
                        .unwrap_or("-"),
                    "slack: envelope received"
                );
                update_health(|h| {
                    h.events_received += 1;
                    h.last_event_at = Some(now());
                });
                let trigger = match kind.as_str() {
                    "slash_commands" => trigger_from_slash(&payload),
                    "events_api" => trigger_from_event(&payload),
                    _ => None, // `interactive` is unused in V1
                };
                match trigger {
                    Some(trigger) => {
                        tokio::spawn(handle_trigger(
                            state.clone(),
                            web.clone(),
                            bot.clone(),
                            trigger,
                        ));
                    }
                    // Slack delivers every subscribed event type, so this is the
                    // ordinary case for anything that isn't an app_mention.
                    None => {
                        tracing::debug!(kind = %kind, event = %event_type, "slack: not a trigger")
                    }
                }
            }
        }
    }
    Ok("stream ended".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn auth(user_id: &str, team_id: &str) -> AuthTest {
        AuthTest {
            user_id: user_id.into(),
            team_id: team_id.into(),
            bot_id: Some("B1".into()),
        }
    }

    fn trigger(user_id: &str, team_id: &str, via_app: bool) -> Trigger {
        Trigger {
            team_id: team_id.into(),
            channel_id: "C1".into(),
            user_id: user_id.into(),
            text: "do it".into(),
            anchor: Anchor::Mention {
                root_ts: "1.1".into(),
                event_ts: "1.1".into(),
            },
            dedupe_id: "slack:Ev1".into(),
            via_app,
        }
    }

    #[test]
    fn parse_envelope_classifies_frames() {
        assert_eq!(
            parse_envelope(&json!({"type": "hello"})),
            Envelope::Hello { app_id: None }
        );
        // The hello frame names the app the app-level token opened.
        assert_eq!(
            parse_envelope(&json!({
                "type": "hello", "connection_info": {"app_id": "A123"}
            })),
            Envelope::Hello {
                app_id: Some("A123".into())
            }
        );
        assert_eq!(
            parse_envelope(&json!({"type": "disconnect", "reason": "warning"})),
            Envelope::Disconnect {
                reason: "warning".into()
            }
        );
        match parse_envelope(&json!({
            "type": "slash_commands", "envelope_id": "e1", "payload": {"x": 1}
        })) {
            Envelope::Payload {
                envelope_id, kind, ..
            } => {
                assert_eq!(envelope_id, "e1");
                assert_eq!(kind, "slash_commands");
            }
            other => panic!("expected payload, got {other:?}"),
        }
    }

    #[test]
    fn slash_trigger_has_no_thread_anchor() {
        let t = trigger_from_slash(&json!({
            "team_id": "T1", "channel_id": "C1", "user_id": "U1",
            "text": "fix the flake", "trigger_id": "tr1"
        }))
        .unwrap();
        assert_eq!(t.anchor, Anchor::Slash);
        assert_eq!(t.user_id, "U1");
        assert_eq!(t.dedupe_id, "slash:tr1");
    }

    #[test]
    fn mention_anchors_on_its_thread() {
        let payload = json!({
            "team_id": "T1", "event_id": "Ev1",
            "event": {"type": "app_mention", "user": "U2", "channel": "C1",
                      "ts": "1.1", "thread_ts": "0.9", "text": "<@BOT> do it"}
        });
        let t = trigger_from_event(&payload).unwrap();
        assert_eq!(
            t.anchor,
            Anchor::Mention {
                root_ts: "0.9".into(),
                event_ts: "1.1".into()
            }
        );
        assert_eq!(t.text, "do it");
        assert_eq!(t.dedupe_id, "slack:Ev1");
        assert!(!t.via_app);

        // A message posted through another app keeps the human's user id, so
        // `bot_id` is the only thing separating the two.
        let mut app_post = payload;
        app_post["event"]["bot_id"] = json!("OTHER_APP");
        assert!(trigger_from_event(&app_post).unwrap().via_app);
    }

    #[test]
    fn only_app_mentions_parse_as_triggers() {
        let payload = json!({
            "team_id": "T1", "event_id": "Ev9",
            "event": {"type": "message", "user": "U2", "channel": "C1", "ts": "1.1", "text": "hi"}
        });
        assert!(trigger_from_event(&payload).is_none());
    }

    /// The default boundary: the workspace plus the channel invite. No list to
    /// maintain, and a deployment that ships tokens works for the people who can
    /// already see the bot.
    #[test]
    fn workspace_access_admits_any_person_in_the_workspace() {
        let bot = auth("BOT", "T1");
        let t = trigger("U2", "T1", false);
        assert_eq!(authorize(&Access::Workspace, &bot, &t), Ok(()));
    }

    #[test]
    fn workspace_access_rejects_other_apps() {
        let bot = auth("BOT", "T1");
        // An alerting bot whose message happens to contain `@loom` must not
        // launch a privileged session.
        assert_eq!(
            authorize(&Access::Workspace, &bot, &trigger("U2", "T1", true)),
            Err(Denial::Automation)
        );
        // …unless its identity is named explicitly, which is how an approved
        // automation opts in.
        assert_eq!(
            authorize(
                &Access::Listed(vec!["U2".into()]),
                &bot,
                &trigger("U2", "T1", true)
            ),
            Ok(())
        );
    }

    #[test]
    fn hard_gates_hold_under_every_access_mode() {
        let bot = auth("BOT", "T1");
        for access in [
            Access::Workspace,
            Access::Listed(vec!["BOT".into(), "U2".into()]),
        ] {
            // loom's own post never triggers loom, even when listed.
            assert_eq!(
                authorize(&access, &bot, &trigger("BOT", "T1", false)),
                Err(Denial::Own)
            );
            // Another workspace (a Slack Connect channel) is always rejected.
            assert_eq!(
                authorize(&access, &bot, &trigger("U2", "T_OTHER", false)),
                Err(Denial::OtherWorkspace)
            );
        }
    }

    #[test]
    fn listed_access_admits_only_listed_users() {
        let bot = auth("BOT", "T1");
        let access = Access::Listed(vec!["U2".into()]);
        assert_eq!(
            authorize(&access, &bot, &trigger("U2", "T1", false)),
            Ok(())
        );
        assert_eq!(
            authorize(&access, &bot, &trigger("U3", "T1", false)),
            Err(Denial::NotListed)
        );
        // A rejected person is told why; the quiet denials stay quiet.
        assert!(Denial::NotListed.reply().is_some());
        assert!(Denial::Own.reply().is_none());
        assert!(Denial::Automation.reply().is_none());
        assert!(Denial::OtherWorkspace.reply().is_none());
    }

    /// A user token authenticates and connects like a bot token; `bot_id` is the
    /// only field that distinguishes them.
    #[test]
    fn auth_test_separates_bot_tokens_from_user_tokens() {
        assert!(auth("U1", "T1").is_bot());
        assert!(!AuthTest {
            user_id: "U1".into(),
            team_id: "T1".into(),
            bot_id: None,
        }
        .is_bot());
    }

    #[test]
    fn top_level_mention_anchors_on_its_own_ts() {
        let t = trigger_from_event(&json!({
            "team_id": "T1", "event_id": "Ev2",
            "event": {"type": "app_mention", "user": "U2", "channel": "C1",
                      "ts": "5.5", "text": "<@BOT> hi"}
        }))
        .unwrap();
        assert_eq!(
            t.anchor,
            Anchor::Mention {
                root_ts: "5.5".into(),
                event_ts: "5.5".into()
            }
        );
        assert_eq!(history_target(&t), HistoryTarget::Channel);
    }

    #[test]
    fn threaded_mention_reads_its_thread() {
        let mut t = trigger("U1", "T1", false);
        t.anchor = Anchor::Mention {
            root_ts: "4.4".into(),
            event_ts: "5.5".into(),
        };

        assert_eq!(history_target(&t), HistoryTarget::Thread("4.4"));
    }

    #[test]
    fn repo_prefix_splits_only_a_leading_slug() {
        assert_eq!(
            parse_repo_prefix("acme/web: fix it"),
            (Some("acme/web".into()), "fix it".into())
        );
        assert_eq!(parse_repo_prefix("just do it"), (None, "just do it".into()));
        // A colon that isn't a repo prefix is left alone.
        assert_eq!(parse_repo_prefix("note: hi"), (None, "note: hi".into()));
    }

    #[test]
    fn wiring_round_trips_and_rejects_junk() {
        assert_eq!(
            parse_wiring("T1/C1/1720.5"),
            Some(("T1".into(), "C1".into(), "1720.5".into()))
        );
        // The ts (last segment) is what a reply threads under and the card edits.
        let (_t, _c, root) = parse_wiring("T1/C1/1720.5").unwrap();
        assert_eq!(root, "1720.5");
        assert_eq!(parse_wiring("T1/C1"), None);
        assert_eq!(parse_wiring("T1//x"), None);
        // Two branches for the same thread hash to the same short name.
        assert_eq!(short_hash("T1/C1/1720.5"), short_hash("T1/C1/1720.5"));
        assert_ne!(short_hash("T1/C1/1720.5"), short_hash("T1/C1/1720.6"));
    }

    #[test]
    fn render_status_uses_slack_mrkdwn_and_escapes() {
        let ev = events::Event {
            id: 1,
            branch_id: "b".into(),
            kind: "tag".into(),
            data: json!({"key": "attention", "value": "attention", "note": "ready <now>", "by": "agent"}),
            created_at: "2026-07-20T21:04:00Z".into(),
        };
        let card = render_status("http://loom/s/abc", &[], std::slice::from_ref(&ev));
        assert!(card.starts_with("On it — <http://loom/s/abc>"));
        assert!(card.contains("*attention*"), "single-star bold: {card}");
        assert!(card.contains("ready &lt;now&gt;"), "escaped: {card}");
        assert!(!card.contains("**"), "no markdown bold: {card}");
    }
}
