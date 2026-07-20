//! Loom's Agent Client Protocol client.
//!
//! For an ACP session (`protocol='acp'`) the agent is a headless adapter
//! subprocess under a detached tapestry *relay* supervisor. Loom drives it over
//! JSON-RPC 2.0, newline-delimited, spooled and replayed by the relay (see
//! `crate::backend::{new_relay_session, subscribe_relay, ...}`). One
//! [`tokio`] task per live session — a [`Session`](Task) — owns the relay
//! subscription, a JSON-RPC id map, the delta-consolidation buffers, the
//! [journal](crate::chat) writer, and a per-session [`broadcast`] the `/chat/stream`
//! SSE route tails.
//!
//! ## Journal blocks
//!
//! The task consolidates streaming chunks in memory and writes one journal block
//! per *block* boundary; live updates ride SSE only. Block `kind`s + payloads:
//!
//! - `user_message`   `{ text, by }` — a dispatched prompt, journaled once at
//!   dispatch ([`Task::start_turn`]). Adapter-streamed `user_message_chunk`s are
//!   never journaled: loom is the only prompt source, so every user chunk is an
//!   echo or a history replay (`session/load`, post-`/compact` context replay)
//!   and journaling it would duplicate the transcript.
//! - `agent_message`  `{ text }` — a whole consolidated agent message.
//! - `thought`        `{ text, ms }` — a whole consolidated reasoning passage.
//! - `tool_call`      `{ tool_call_id, title, tool_kind, status, content, locations }`
//!   — written once at terminal status; live state rides `tool` SSE.
//! - `plan`           `{ entries: [{content, status}] }`.
//! - `permission_request` `{ request_id, tool_call_id, title, options, outcome }`
//!   — inserted open, `UPDATE`d in place on resolution.
//! - `mode_change`    `{ mode_id, by }`.
//! - `usage`          `{ used, size }`.
//! - `turn_end`       `{ stop_reason }`.
//! - `handoff`        `{ from, to, model, effort }` — the provider boundary
//!   that replaces the synthetic bootstrap prompt in the visible journal.
//!
//! ## Acking
//!
//! Every agent→client frame carries a spool seq. The task acks a frame only after
//! the sqlite write for any block that frame *completed* has committed: the ack
//! watermark is held back to just before the earliest frame still feeding an open
//! consolidation buffer, a live tool call, or an unanswered permission request
//! (block-boundary acking). Journal writes are idempotent (`INSERT OR IGNORE` on
//! `(session_id, turn, seq)`, plus upstream-id guards for tool calls and turn
//! ends), so a replay after a loom restart re-ingests without duplicating.

mod wire;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tapestry::RelayEvent;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::chat::{self, kind, ChatBlockView};
use crate::db::{now_iso, Db};
use crate::session;
use crate::web::AppState;
use wire::{
    method, Incoming, IncomingKind, PermissionOption, RequestPermissionParams, SessionNotification,
    SessionUpdate, ToolCall, ToolCallContent, ToolCallLocation,
};

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// How to open the ACP session at [`start`]: a fresh `session/new`, or a
/// `session/load` replay of an existing agent session id.
#[derive(Debug, Clone)]
pub enum NewOrLoad {
    /// A fresh session in `cwd`; `meta` is the optional `_meta` object for
    /// adapter options (e.g. `{"claudeCode":{"options":{...}}}`).
    New { cwd: PathBuf, meta: Option<Value> },
    /// Reopen the agent's existing on-disk session by id (`session/load`).
    Load { acp_session_id: String },
}

/// Everything [`start`] needs to bring an ACP session up.
#[derive(Debug, Clone)]
pub struct AcpLaunch {
    /// The shell command the relay runs to launch the adapter over stdio.
    pub adapter_cmd: String,
    /// The relay child's working directory.
    pub cwd: PathBuf,
    /// Out-of-band environment for the adapter process (delivered over the
    /// supervisor, never on argv).
    pub env: Vec<(String, String)>,
    /// Open a fresh session or reload an existing one.
    pub new_or_load: NewOrLoad,
    /// The initial permission posture (`bypassPermissions`, `acceptEdits`,
    /// `default`, `plan`), applied via `session/set_mode` after setup. `None`
    /// leaves the adapter's default mode.
    pub mode: Option<String>,
    /// The session's goal, sent as the first `session/prompt` (journaled as the
    /// first `user_message`). `None` waits for the first REST prompt.
    pub goal: Option<String>,
}

/// How a prompt was accepted plus the turn it belongs to — the `POST /prompt`
/// 202 body. `steered` is true only when the adapter injected it into the
/// already-running turn; unsupported adapters retain the durable queue.
#[derive(Debug, Clone, Serialize)]
pub struct PromptAck {
    pub queued: bool,
    pub steered: bool,
    pub turn: Option<i64>,
}

/// The outcome of answering a permission request over REST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermAnswer {
    /// Answered — the JSON-RPC response was sent and the block resolved.
    Ok,
    /// No permission request with that id (404).
    NotFound,
    /// The request was already resolved (409).
    AlreadyResolved,
}

/// One SSE event the `/chat/stream` route emits: an event name (`block`, `delta`,
/// `tool`, `turn`, `queue`, `metadata`) and its JSON data.
#[derive(Debug, Clone, Serialize)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

/// Agent-owned controls for the conversation composer. These are deliberately
/// kept as ACP-shaped JSON: command inputs and session config options are an
/// extensible protocol surface, and loom forwards fields it does not yet render
/// instead of narrowing the wire contract and making adapters stale again.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AcpMetadata {
    pub commands: Vec<Value>,
    pub config_options: Vec<Value>,
    pub modes: Vec<Value>,
    /// Whether the adapter advertised the codex-acp steering extension during
    /// initialize. The browser uses this observed capability instead of
    /// optimistically probing a private method that may not exist.
    pub steering_supported: bool,
}

/// A live session's control surface, held in the [`AcpRegistry`]: send commands
/// to its task and subscribe to its SSE stream.
#[derive(Clone)]
pub struct AcpHandle {
    cmd_tx: mpsc::Sender<Command>,
    events_tx: broadcast::Sender<SseEvent>,
    metadata: Arc<Mutex<AcpMetadata>>,
}

impl AcpHandle {
    /// Subscribe to the session's SSE broadcast.
    pub fn subscribe(&self) -> broadcast::Receiver<SseEvent> {
        self.events_tx.subscribe()
    }

    /// Snapshot the latest agent-owned composer metadata. The `/chat` snapshot
    /// carries this before the browser tails updates over SSE.
    pub fn metadata(&self) -> AcpMetadata {
        self.metadata.lock().unwrap().clone()
    }

    /// Send a user message: dispatched as a `session/prompt` when idle, steered
    /// into a live turn when the adapter advertises support, or appended to the
    /// durable queue otherwise.
    pub async fn prompt(
        &self,
        text: String,
        by: Option<String>,
        force_steer: bool,
        resources: Vec<Value>,
    ) -> Result<PromptAck> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Prompt {
                text,
                by,
                force_steer,
                resources,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("acp task dropped the reply"))?
    }

    /// Send the current durable next-turn queue now: promote it when the adapter
    /// advertises steering, otherwise cancel the live turn and start it normally.
    /// The task reads the queue itself so the browser cannot accidentally send
    /// stale or partial text.
    pub async fn force_pending(&self, by: Option<String>) -> Result<PromptAck> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::ForcePending { by, reply: tx })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("acp task dropped the reply"))?
    }

    /// Atomically retract the durable next-turn queue for editing. This runs on
    /// the ACP task so a turn boundary cannot dispatch the same text while the
    /// browser is moving it back into the composer.
    pub async fn retract_pending(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::RetractPending { reply: tx })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("acp task dropped the reply"))?
    }

    /// Interrupt the in-flight turn (`session/cancel`).
    pub async fn cancel(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Cancel { reply: tx })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("acp task dropped the reply"))?
    }

    /// Answer a pending permission request.
    pub async fn answer_permission(
        &self,
        request_id: String,
        option_id: String,
        by: String,
    ) -> Result<PermAnswer> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::AnswerPermission {
                request_id,
                option_id,
                by,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await.map_err(|_| anyhow!("acp task dropped the reply"))
    }

    /// Change the session mode (`session/set_mode`), journaling a `mode_change`.
    pub async fn set_mode(&self, mode_id: String, by: Option<String>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SetMode {
                mode_id,
                by,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("acp task dropped the reply"))?
    }
    /// Change an ACP session configuration option (`model`, reasoning effort,
    /// mode, or another adapter-defined value). The task waits for the agent's
    /// full refreshed option set before acknowledging the REST write and returns
    /// that authoritative state to the caller.
    pub async fn set_config_option(&self, config_id: String, value: Value) -> Result<AcpMetadata> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SetConfigOption {
                config_id,
                value,
                reply: tx,
            })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("acp task dropped the reply"))?
    }

    /// Atomically quiesce an idle task for provider replacement. The command is
    /// ordered with prompts on the same channel; acknowledgement arrives only
    /// after the task has removed its registry slot and will accept no more work.
    pub async fn prepare_handoff(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::PrepareHandoff { reply: tx })
            .await
            .map_err(|_| anyhow!("acp task is gone"))?;
        rx.await
            .map_err(|_| anyhow!("acp task dropped the handoff reply"))?
    }
}

/// The registry of live ACP session tasks, held on [`AppState`]. Clone-cheap
/// (an `Arc`ed map); handlers look a session up to drive it or tail its stream.
/// Each registration carries a generation so a task that exits after it has been
/// superseded (stopped then re-attached) removes only its own slot.
#[derive(Clone, Default)]
pub struct AcpRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    map: HashMap<String, (u64, AcpHandle)>,
    next_gen: u64,
}

impl AcpRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The live handle for `session_id`, or `None` when no task is running.
    pub fn get(&self, session_id: &str) -> Option<AcpHandle> {
        self.inner
            .lock()
            .unwrap()
            .map
            .get(session_id)
            .map(|(_, h)| h.clone())
    }

    /// Whether a task is running for `session_id`.
    pub fn is_live(&self, session_id: &str) -> bool {
        self.inner.lock().unwrap().map.contains_key(session_id)
    }

    /// Stop a live session: drop its handle so the task's command channel closes
    /// and it winds down. Returns whether a task was registered. (Tests use this
    /// to simulate a loom-side crash before re-attaching.)
    pub fn stop(&self, session_id: &str) -> bool {
        self.inner.lock().unwrap().map.remove(session_id).is_some()
    }

    fn register(&self, session_id: &str, handle: AcpHandle) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        let generation = inner.next_gen;
        inner.next_gen += 1;
        inner
            .map
            .insert(session_id.to_string(), (generation, handle));
        generation
    }

    /// Remove the session's slot only if it still holds `generation` — a task
    /// that has been superseded leaves the newer registration untouched.
    fn remove_own(&self, session_id: &str, generation: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.map.get(session_id).map(|(g, _)| *g) == Some(generation) {
            inner.map.remove(session_id);
        }
    }
}

/// Spawn a fresh ACP session: create the relay running `launch.adapter_cmd`,
/// `initialize`, open (or load) the ACP session, store its id on the session row,
/// send the goal as the first prompt when present, then run the session task.
pub async fn start(state: &AppState, session_id: &str, launch: AcpLaunch) -> Result<()> {
    start_inner(state, session_id, launch, None).await
}

/// Start a fresh provider against an existing loom session. The opening prompt
/// carries the provider-neutral history to the adapter, while `handoff` is the
/// compact block journaled in its place.
pub async fn start_handoff(
    state: &AppState,
    session_id: &str,
    launch: AcpLaunch,
    handoff: Value,
) -> Result<()> {
    start_inner(state, session_id, launch, Some(handoff)).await
}

async fn start_inner(
    state: &AppState,
    session_id: &str,
    launch: AcpLaunch,
    handoff: Option<Value>,
) -> Result<()> {
    let session = session::get(&state.db, session_id)
        .await?
        .ok_or_else(|| anyhow!("session {session_id} not found"))?;
    let relay_name = session.term_session.clone();

    let env: Vec<(&str, &str)> = launch
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    crate::backend::new_relay_session(&relay_name, &launch.adapter_cmd, &env, &launch.cwd).await?;
    let stream = crate::backend::subscribe_relay(&relay_name, 0).await?;

    let (events_tx, _) = broadcast::channel(256);
    let mut task = Task::fresh(state, &session, relay_name, stream, events_tx.clone()).await?;
    task.handshake(&launch, handoff).await?;

    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    task.generation = state.acp.register(
        session_id,
        AcpHandle {
            cmd_tx,
            events_tx,
            metadata: task.metadata.clone(),
        },
    );
    tokio::spawn(async move { task.run(cmd_rx).await });
    Ok(())
}

/// Re-attach to an ACP session whose relay outlived a loom restart: subscribe from
/// the persisted ack cursor, re-adopt the in-flight request state and the block
/// cursor from the journal, and run the session task. Un-acked frames replay and
/// re-ingest idempotently.
pub async fn attach(state: &AppState, session_id: &str) -> Result<()> {
    let session = session::get(&state.db, session_id)
        .await?
        .ok_or_else(|| anyhow!("session {session_id} not found"))?;
    if session.protocol != "acp" {
        bail!("session {session_id} is not an acp session");
    }
    let acp_session_id = session
        .acp_session_id
        .clone()
        .ok_or_else(|| anyhow!("session {session_id} has no acp_session_id"))?;
    let relay_name = session.term_session.clone();
    let cursor = session.acp_ack_seq.max(0) as u64;
    let stream = crate::backend::subscribe_relay(&relay_name, cursor).await?;

    let (events_tx, _) = broadcast::channel(256);
    let mut task = Task::recover(
        state,
        &session,
        acp_session_id,
        relay_name,
        stream,
        events_tx.clone(),
    )
    .await?;

    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    task.generation = state.acp.register(
        session_id,
        AcpHandle {
            cmd_tx,
            events_tx,
            metadata: task.metadata.clone(),
        },
    );
    tokio::spawn(async move { task.run(cmd_rx).await });
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands (REST → task)
// ---------------------------------------------------------------------------

enum Command {
    Prompt {
        text: String,
        by: Option<String>,
        force_steer: bool,
        resources: Vec<Value>,
        reply: oneshot::Sender<Result<PromptAck>>,
    },
    ForcePending {
        by: Option<String>,
        reply: oneshot::Sender<Result<PromptAck>>,
    },
    RetractPending {
        reply: oneshot::Sender<Result<String>>,
    },
    Cancel {
        reply: oneshot::Sender<Result<()>>,
    },
    AnswerPermission {
        request_id: String,
        option_id: String,
        by: String,
        reply: oneshot::Sender<PermAnswer>,
    },
    SetMode {
        mode_id: String,
        by: Option<String>,
        reply: oneshot::Sender<Result<()>>,
    },
    SetConfigOption {
        config_id: String,
        value: Value,
        reply: oneshot::Sender<Result<AcpMetadata>>,
    },
    PrepareHandoff {
        reply: oneshot::Sender<Result<()>>,
    },
}

struct PendingMode {
    mode_id: String,
    by: Option<String>,
    reply: oneshot::Sender<Result<()>>,
}

// ---------------------------------------------------------------------------
// Task state
// ---------------------------------------------------------------------------

/// An open consolidation buffer accumulating chunk deltas of one `kind` until a
/// block boundary flushes it.
struct ChunkBuf {
    kind: &'static str,
    text: String,
    first_seq: u64,
}

/// The last-known state of a tool call, tracked from `tool_call`/`tool_call_update`
/// until it reaches a terminal status.
struct LiveTool {
    id: String,
    first_seq: u64,
    title: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    content: Vec<ToolCallContent>,
    locations: Vec<ToolCallLocation>,
}

impl LiveTool {
    fn new(id: &str, first_seq: u64) -> Self {
        Self {
            id: id.to_string(),
            first_seq,
            title: None,
            kind: None,
            status: None,
            content: Vec::new(),
            locations: Vec::new(),
        }
    }

    fn merge(&mut self, tc: &ToolCall) {
        if tc.title.is_some() {
            self.title = tc.title.clone();
        }
        if tc.kind.is_some() {
            self.kind = tc.kind.clone();
        }
        if tc.status.is_some() {
            self.status = tc.status.clone();
        }
        if let Some(c) = &tc.content {
            self.content = c.clone();
        }
        if let Some(l) = &tc.locations {
            self.locations = l.clone();
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("completed") | Some("failed") | Some("cancelled")
        )
    }

    /// Map ACP content into the journal contract's content array.
    fn content_json(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for c in &self.content {
            match c {
                ToolCallContent::Content { content } => out.push(json!({
                    "type": "text",
                    "text": content.text().unwrap_or("").to_string(),
                })),
                ToolCallContent::Diff {
                    path,
                    old_text,
                    new_text,
                } => out.push(json!({
                    "type": "diff",
                    "path": path.clone(),
                    "old": old_text.clone(),
                    "new": new_text.clone(),
                })),
                ToolCallContent::Other => {}
            }
        }
        out
    }

    fn locations_json(&self) -> Vec<Value> {
        self.locations
            .iter()
            .map(|l| json!({ "path": l.path.clone(), "line": l.line }))
            .collect()
    }

    /// The status to journal: a live tool flushed at turn end reads as `cancelled`.
    fn terminal_status(&self) -> &str {
        match self.status.as_deref() {
            Some(s @ ("completed" | "failed" | "cancelled")) => s,
            _ => "cancelled",
        }
    }

    fn block_payload(&self) -> Value {
        json!({
            "tool_call_id": self.id.clone(),
            "title": self.title.clone().unwrap_or_default(),
            "tool_kind": self.kind.clone().unwrap_or_else(|| "other".to_string()),
            "status": self.terminal_status().to_string(),
            "content": self.content_json(),
            "locations": self.locations_json(),
        })
    }

    fn sse(&self, turn: i64) -> Value {
        json!({
            "turn": turn,
            "tool_call_id": self.id.clone(),
            "title": self.title.clone().unwrap_or_default(),
            "tool_kind": self.kind.clone().unwrap_or_else(|| "other".to_string()),
            "status": self.status.clone().unwrap_or_else(|| "pending".to_string()),
            "content": self.content_json(),
            "locations": self.locations_json(),
        })
    }
}

/// An unanswered permission request awaiting a client answer: its JSON-RPC id (to
/// echo in the response) and the frame seq (held un-acked until answered).
struct PendingPerm {
    jsonrpc_id: Value,
    frame_seq: u64,
}

/// A `_session/steering` request waiting for the adapter's acceptance response.
struct PendingSteer {
    text: String,
    by: Option<String>,
    turn: i64,
    forced: bool,
    /// Exact durable queue prefix promoted by this request. Consumed only after
    /// the adapter accepts steering, preserving messages appended behind it.
    promoted_queue: Option<String>,
    resources: Vec<Value>,
    /// Forced steering waits for the adapter's final answer. An ordinary send is
    /// acknowledged as durably queued before steering starts, so it carries no
    /// waiting HTTP reply and cannot lock the composer behind a private RPC.
    reply: Option<oneshot::Sender<Result<PromptAck>>>,
}

struct Task {
    db: Db,
    /// The loom event bus — used to drive the turn-boundary status/idle lifecycle
    /// through [`crate::monitor::record_acp_lifecycle`] (working at turn start,
    /// idle at turn end), the sole reason the acp task holds it.
    bus: crate::events::EventBus,
    registry: AcpRegistry,
    /// This task's registry generation — used to remove only its own slot on exit.
    generation: u64,
    session_id: String,
    #[allow(dead_code)]
    relay_name: String,
    acp_session_id: String,
    events_tx: broadcast::Sender<SseEvent>,
    stream: tapestry::RelayStream,

    next_req_id: u64,
    /// The in-flight `session/prompt` request id + its turn, mirrored on the
    /// session row so a replayed turn-end response is recognized after a restart.
    inflight_prompt: Option<(u64, i64)>,

    current_turn: i64,
    next_seq: i64,
    turns_dispatched: i64,
    turn_live: bool,

    buf: Option<ChunkBuf>,
    tools: HashMap<String, LiveTool>,
    pending_perms: HashMap<String, PendingPerm>,

    /// Permission posture captured when the active turn started. `current_mode`
    /// may advance while that turn is running, but a provider cannot retroactively
    /// rebuild the turn's approval policy or sandbox.
    effective_mode: Option<String>,
    current_mode: Option<String>,
    metadata: Arc<Mutex<AcpMetadata>>,
    pending_mode: HashMap<u64, PendingMode>,
    pending_config: HashMap<u64, oneshot::Sender<Result<AcpMetadata>>>,
    #[allow(dead_code)]
    load_session_cap: bool,
    /// codex-acp advertises this experimental extension in
    /// `_meta.steering.supported`; other adapters keep queue semantics.
    steering_cap: bool,
    pending_steers: HashMap<u64, PendingSteer>,
    /// A turn started internally by the steering extension after the previous
    /// turn raced to completion. It has no `session/prompt` response id, so its
    /// end is taken from codex-acp's `session_info_update` idle edge.
    external_turn: bool,
    pending_external: Option<PendingSteer>,
    /// Latched for the duration of a `session/load` replay: the adapter re-streams
    /// the whole conversation as `session/update` notifications, but we already
    /// hold it in the journal, so journal writes are suppressed (and the seq
    /// cursor left untouched) until the load response lands.
    suppress_journal: bool,

    highest_seq: u64,
    acked: u64,
    /// Latched when a journal write fails: the ack watermark freezes (so the
    /// un-journaled frames replay after a restart) and the failure is logged.
    journal_failed: bool,
}

impl Task {
    async fn fresh(
        state: &AppState,
        session: &session::Session,
        relay_name: String,
        stream: tapestry::RelayStream,
        events_tx: broadcast::Sender<SseEvent>,
    ) -> Result<Self> {
        let cursor = chat::max_turn_seq(&state.db, &session.id).await?;
        let (current_turn, next_seq, turns_dispatched) = match cursor {
            Some((turn, seq)) => (turn, seq + 1, turn + 1),
            None => (0, 0, 0),
        };
        Ok(Self {
            db: state.db.clone(),
            bus: state.bus.clone(),
            registry: state.acp.clone(),
            generation: 0,
            session_id: session.id.clone(),
            relay_name,
            acp_session_id: String::new(),
            events_tx,
            stream,
            next_req_id: 0,
            inflight_prompt: None,
            current_turn,
            next_seq,
            turns_dispatched,
            turn_live: false,
            buf: None,
            tools: HashMap::new(),
            pending_perms: HashMap::new(),
            effective_mode: None,
            current_mode: None,
            metadata: Arc::new(Mutex::new(AcpMetadata::default())),
            pending_mode: HashMap::new(),
            pending_config: HashMap::new(),
            load_session_cap: false,
            steering_cap: false,
            pending_steers: HashMap::new(),
            external_turn: false,
            pending_external: None,
            suppress_journal: false,
            highest_seq: 0,
            acked: 0,
            journal_failed: false,
        })
    }

    async fn recover(
        state: &AppState,
        session: &session::Session,
        acp_session_id: String,
        relay_name: String,
        stream: tapestry::RelayStream,
        events_tx: broadcast::Sender<SseEvent>,
    ) -> Result<Self> {
        let (max_turn, max_seq) = chat::max_turn_seq(&state.db, &session.id)
            .await?
            .unwrap_or((0, -1));
        let inflight_value = session
            .acp_inflight
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        let live_turn = inflight_value
            .as_ref()
            .and_then(|v| v.get("turn"))
            .and_then(Value::as_i64);
        let external_turn = inflight_value
            .as_ref()
            .and_then(|v| v.get("external"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let inflight = inflight_value
            .as_ref()
            .and_then(|v| Some((v.get("prompt_id")?.as_u64()?, v.get("turn")?.as_i64()?)));
        let effective_mode = inflight_value
            .as_ref()
            .and_then(|v| v.get("mode"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let current_turn = live_turn.unwrap_or(max_turn);
        // The journal always holds at least this turn's `user_message`, so
        // `max_seq + 1` continues the turn without colliding.
        let next_seq = max_seq + 1;
        let turns_dispatched = if max_seq < 0 { 0 } else { current_turn + 1 };

        Ok(Self {
            db: state.db.clone(),
            bus: state.bus.clone(),
            registry: state.acp.clone(),
            generation: 0,
            session_id: session.id.clone(),
            relay_name,
            acp_session_id,
            events_tx,
            stream,
            next_req_id: 0,
            inflight_prompt: inflight,
            current_turn,
            next_seq,
            turns_dispatched,
            turn_live: live_turn.is_some(),
            buf: None,
            tools: HashMap::new(),
            pending_perms: HashMap::new(),
            // Old in-flight records have no mode. Keep that unknown rather than
            // applying a newer session selection to an older turn.
            effective_mode,
            current_mode: session.current_mode.clone(),
            metadata: Arc::new(Mutex::new(AcpMetadata::default())),
            pending_mode: HashMap::new(),
            pending_config: HashMap::new(),
            load_session_cap: false,
            steering_cap: false,
            pending_steers: HashMap::new(),
            external_turn,
            pending_external: None,
            suppress_journal: false,
            highest_seq: session.acp_ack_seq.max(0) as u64,
            acked: session.acp_ack_seq.max(0) as u64,
            journal_failed: false,
        })
    }

    fn next_id(&mut self) -> u64 {
        self.next_req_id += 1;
        self.next_req_id
    }

    fn emit(&self, event: &str, data: Value) {
        let _ = self.events_tx.send(SseEvent {
            event: event.to_string(),
            data,
        });
    }

    fn emit_queue(&self, pending_prompt: Option<&str>) {
        self.emit("queue", json!({ "pending_prompt": pending_prompt }));
    }

    fn emit_metadata(&self) {
        let metadata = self.metadata.lock().unwrap().clone();
        self.emit(
            "metadata",
            serde_json::to_value(metadata).unwrap_or(Value::Null),
        );
    }

    fn replace_commands(&self, commands: Vec<Value>, emit: bool) {
        self.metadata.lock().unwrap().commands = commands;
        if emit {
            self.emit_metadata();
        }
    }

    fn replace_config_options(&mut self, config_options: Vec<Value>, emit: bool) {
        if let Some(mode) = config_options.iter().find_map(|option| {
            let is_mode = option.get("category").and_then(Value::as_str) == Some("mode")
                || option.get("id").and_then(Value::as_str) == Some("mode");
            is_mode
                .then(|| option.get("currentValue").and_then(Value::as_str))
                .flatten()
                .map(str::to_string)
        }) {
            self.current_mode = Some(mode);
        }
        self.metadata.lock().unwrap().config_options = config_options;
        if emit {
            self.emit_metadata();
        }
    }

    fn replace_modes(&mut self, modes: wire::SessionModeState, emit: bool) {
        self.current_mode = Some(modes.current_mode_id.clone());
        self.metadata.lock().unwrap().modes = modes.available_modes;
        if emit {
            self.emit_metadata();
        }
    }

    // -- handshake ----------------------------------------------------------

    async fn handshake(&mut self, launch: &AcpLaunch, handoff: Option<Value>) -> Result<()> {
        let id = self.next_id();
        self.stream
            .write(&wire::request_line(
                id,
                method::INITIALIZE,
                wire::initialize_params(),
            ))
            .await?;
        let (res, err) = self.recv_until_response(id).await?;
        let res = res.ok_or_else(|| anyhow!("initialize failed: {err:?}"))?;
        if let Ok(init) = serde_json::from_value::<wire::InitializeResult>(res) {
            self.load_session_cap = init.agent_capabilities.load_session;
            self.steering_cap = init.meta.steering.supported;
            self.metadata.lock().unwrap().steering_supported = self.steering_cap;
        }

        match &launch.new_or_load {
            NewOrLoad::New { cwd, meta } => {
                let id = self.next_id();
                let params = wire::new_session_params(&cwd.to_string_lossy(), meta.as_ref());
                self.stream
                    .write(&wire::request_line(id, method::SESSION_NEW, params))
                    .await?;
                let (res, err) = self.recv_until_response(id).await?;
                let res = res.ok_or_else(|| anyhow!("session/new failed: {err:?}"))?;
                let ns: wire::NewSessionResult = serde_json::from_value(res)?;
                self.acp_session_id = ns.session_id.clone();
                if let Some(m) = ns.modes {
                    self.replace_modes(m, false);
                }
                if let Some(options) = ns.config_options {
                    self.replace_config_options(options, false);
                }
            }
            NewOrLoad::Load { acp_session_id } => {
                self.acp_session_id = acp_session_id.clone();
                // Continue the *existing* journal: seed the turn/seq cursor from it
                // so a post-load prompt opens a fresh turn (rather than colliding
                // with `Task::fresh`'s zeroed counters). `turns_dispatched > 0` makes
                // the next `start_turn` advance the turn.
                let (max_turn, max_seq) = chat::max_turn_seq(&self.db, &self.session_id)
                    .await?
                    .unwrap_or((0, -1));
                self.current_turn = max_turn;
                self.next_seq = max_seq + 1;
                self.turns_dispatched = if max_seq < 0 { 0 } else { max_turn + 1 };
                // The adapter re-streams the whole conversation as `session/update`
                // notifications during the load — we already hold it — so suppress
                // re-journaling for the duration of the call.
                self.suppress_journal = true;
                let id = self.next_id();
                let params =
                    wire::load_session_params(acp_session_id, &launch.cwd.to_string_lossy());
                self.stream
                    .write(&wire::request_line(id, method::SESSION_LOAD, params))
                    .await?;
                // History replays as `session/update` notifications during the call.
                let (res, err) = self.recv_until_response(id).await?;
                self.suppress_journal = false;
                // Drop any consolidation the suppressed replay left half-open so it
                // can't flush stale history into a later turn.
                self.buf = None;
                self.tools.clear();
                if res.is_none() {
                    bail!("session/load failed: {err:?}");
                }
                if let Some(load) =
                    res.and_then(|r| serde_json::from_value::<wire::LoadSessionResult>(r).ok())
                {
                    if let Some(m) = load.modes {
                        self.replace_modes(m, false);
                    }
                    if let Some(options) = load.config_options {
                        self.replace_config_options(options, false);
                    }
                }
            }
        }
        session::set_acp(&self.db, &self.session_id, &self.acp_session_id).await?;

        if let Some(mode) = &launch.mode {
            let id = self.next_id();
            self.stream
                .write(&wire::request_line(
                    id,
                    method::SESSION_SET_MODE,
                    wire::set_mode_params(&self.acp_session_id, mode),
                ))
                .await?;
            let (result, error) = self.recv_until_response(id).await?;
            if result.is_none() {
                bail!("session/set_mode failed: {error:?}");
            }
            self.current_mode = Some(mode.clone());
        }
        if let Some(mode) = &self.current_mode {
            session::set_current_mode(&self.db, &self.session_id, mode).await?;
        }

        if let Some(goal) = &launch.goal {
            match handoff {
                Some(payload) => self.start_handoff_turn(goal.clone(), payload).await?,
                None => self.start_turn(goal.clone(), None, Vec::new()).await?,
            }
        }
        Ok(())
    }

    /// Drive frames until the response to `want` arrives, processing interleaved
    /// notifications/requests normally. Used only during the synchronous handshake.
    async fn recv_until_response(&mut self, want: u64) -> Result<(Option<Value>, Option<Value>)> {
        loop {
            match self.stream.recv().await {
                Some(RelayEvent::Frame { seq, payload }) => {
                    self.highest_seq = seq;
                    let inc: Incoming = match serde_json::from_slice(&payload) {
                        Ok(i) => i,
                        Err(_) => {
                            self.maybe_ack().await?;
                            continue;
                        }
                    };
                    if inc.kind() == IncomingKind::Response
                        && inc.id.as_ref().and_then(Value::as_u64) == Some(want)
                    {
                        self.maybe_ack().await?;
                        return Ok((inc.result, inc.error));
                    }
                    self.dispatch_frame(seq, inc).await;
                    self.maybe_ack().await?;
                }
                Some(RelayEvent::Exit { status }) => {
                    bail!("agent exited during handshake (status {status:?})")
                }
                None => bail!("relay closed during handshake"),
            }
        }
    }

    // -- main loop ----------------------------------------------------------

    async fn run(mut self, mut cmd_rx: mpsc::Receiver<Command>) {
        let mut handoff_reply = None;
        loop {
            tokio::select! {
                ev = self.stream.recv() => match ev {
                    Some(RelayEvent::Frame { seq, payload }) => {
                        self.highest_seq = seq;
                        match serde_json::from_slice::<Incoming>(&payload) {
                            Ok(inc) => self.dispatch_frame(seq, inc).await,
                            Err(e) => tracing::warn!(session = %self.session_id, error = %e, "unparseable acp frame"),
                        }
                        if let Err(e) = self.maybe_ack().await {
                            tracing::warn!(session = %self.session_id, error = %e, "acp ack failed");
                        }
                    }
                    Some(RelayEvent::Exit { status }) => {
                        self.on_exit(status).await;
                        break;
                    }
                    None => break,
                },
                cmd = cmd_rx.recv() => match cmd {
                    Some(Command::PrepareHandoff { reply }) => {
                        let pending = session::read_pending_prompt(&self.db, &self.session_id)
                            .await
                            .unwrap_or_default();
                        if self.turn_live || !pending.trim().is_empty() {
                            let _ = reply.send(Err(anyhow!("cannot hand off while a turn or queued prompt is active")));
                        } else {
                            handoff_reply = Some(reply);
                            break;
                        }
                    }
                    Some(c) => self.on_command(c).await,
                    None => break,
                },
            }
        }
        self.registry.remove_own(&self.session_id, self.generation);
        if let Some(reply) = handoff_reply {
            let _ = reply.send(Ok(()));
        }
        tracing::info!(session = %self.session_id, "acp task stopped");
    }

    /// Route one inbound frame (notification / agent request / response).
    async fn dispatch_frame(&mut self, seq: u64, inc: Incoming) {
        match inc.kind() {
            IncomingKind::Notification => {
                if inc.method.as_deref() == Some(method::SESSION_UPDATE) {
                    if let Err(e) = self
                        .handle_notification(seq, inc.params.unwrap_or(Value::Null))
                        .await
                    {
                        tracing::warn!(session = %self.session_id, error = %e, "bad session/update");
                    }
                }
            }
            IncomingKind::Request => {
                if inc.method.as_deref() == Some(method::SESSION_REQUEST_PERMISSION) {
                    if let Err(e) = self.handle_permission(seq, inc).await {
                        tracing::warn!(session = %self.session_id, error = %e, "bad request_permission");
                    }
                }
            }
            IncomingKind::Response => self.handle_response(inc).await,
            IncomingKind::Unknown => {}
        }
    }

    // -- session/update handling -------------------------------------------

    async fn handle_notification(&mut self, seq: u64, params: Value) -> Result<()> {
        let notif: SessionNotification = serde_json::from_value(params)?;
        match notif.update {
            SessionUpdate::UserMessageChunk => {
                // Never journaled: loom writes the `user_message` block itself at
                // dispatch (`start_turn`), so an adapter-streamed user chunk is
                // always an echo or a history replay — e.g. claude re-streams the
                // retained user turns after a `/compact` — and journaling it here
                // duplicated the visible chat history.
            }
            SessionUpdate::AgentMessageChunk { content } => {
                if let Some(t) = content.text().map(str::to_string) {
                    self.on_chunk(kind::AGENT_MESSAGE, &t, seq).await;
                }
            }
            SessionUpdate::AgentThoughtChunk { content } => {
                if let Some(t) = content.text().map(str::to_string) {
                    self.on_chunk(kind::THOUGHT, &t, seq).await;
                }
            }
            SessionUpdate::ToolCall(tc) | SessionUpdate::ToolCallUpdate(tc) => {
                self.flush_buf().await;
                self.on_tool(seq, tc).await;
            }
            SessionUpdate::Plan(p) => {
                self.flush_buf().await;
                let entries: Vec<Value> = p
                    .entries
                    .iter()
                    .map(|e| json!({ "content": e.content.clone(), "status": e.status.clone() }))
                    .collect();
                self.journal_block(kind::PLAN, json!({ "entries": entries }))
                    .await;
            }
            SessionUpdate::CurrentModeUpdate { current_mode_id } => {
                self.flush_buf().await;
                if self.current_mode.as_deref() != Some(current_mode_id.as_str()) {
                    self.current_mode = Some(current_mode_id.clone());
                    let _ = session::set_current_mode(&self.db, &self.session_id, &current_mode_id)
                        .await;
                    self.journal_block(
                        kind::MODE_CHANGE,
                        json!({ "mode_id": current_mode_id, "by": Value::Null }),
                    )
                    .await;
                }
            }
            SessionUpdate::UsageUpdate { used, size } => {
                // Usage is metadata, not a prose boundary. Flushing here split
                // replies into tiny blocks when an adapter reported it often.
                self.journal_block(kind::USAGE, json!({ "used": used, "size": size }))
                    .await;
            }
            SessionUpdate::SessionInfoUpdate { meta } => {
                let status = meta
                    .codex
                    .and_then(|codex| codex.thread_status)
                    .map(|s| s.kind);
                if self.external_turn && matches!(status.as_deref(), Some("idle" | "systemError")) {
                    self.external_turn = false;
                    let reason = if status.as_deref() == Some("systemError") {
                        "error"
                    } else {
                        "end_turn"
                    };
                    self.on_turn_end(
                        self.current_turn,
                        Some(json!({ "stopReason": reason })),
                        None,
                    )
                    .await;
                }
            }
            SessionUpdate::AvailableCommandsUpdate { available_commands } => {
                self.replace_commands(available_commands, true);
            }
            SessionUpdate::ConfigOptionUpdate { config_options } => {
                self.replace_config_options(config_options, true);
                if let Some(mode) = &self.current_mode {
                    let _ = session::set_current_mode(&self.db, &self.session_id, mode).await;
                }
            }
            SessionUpdate::Other => {}
        }
        Ok(())
    }

    async fn on_chunk(&mut self, kind: &'static str, text: &str, seq: u64) {
        let need_flush = self.buf.as_ref().map(|b| b.kind != kind).unwrap_or(false);
        if need_flush {
            self.flush_buf().await;
        }
        let b = self.buf.get_or_insert_with(|| ChunkBuf {
            kind,
            text: String::new(),
            first_seq: seq,
        });
        b.text.push_str(text);
        self.emit(
            "delta",
            json!({ "turn": self.current_turn, "kind": kind, "text": text }),
        );
    }

    async fn flush_buf(&mut self) {
        let Some(b) = self.buf.take() else { return };
        let (kind, payload) = match b.kind {
            kind::AGENT_MESSAGE => (kind::AGENT_MESSAGE, json!({ "text": b.text })),
            kind::THOUGHT => (kind::THOUGHT, json!({ "text": b.text, "ms": Value::Null })),
            _ => return,
        };
        self.journal_block(kind, payload).await;
    }

    async fn on_tool(&mut self, seq: u64, tc: ToolCall) {
        let mut tool = self
            .tools
            .remove(&tc.tool_call_id)
            .unwrap_or_else(|| LiveTool::new(&tc.tool_call_id, seq));
        tool.merge(&tc);
        self.emit("tool", tool.sse(self.current_turn));
        if tool.is_terminal() {
            if !chat::tool_call_exists(&self.db, &self.session_id, &tool.id)
                .await
                .unwrap_or(false)
            {
                let payload = tool.block_payload();
                self.journal_block(kind::TOOL_CALL, payload).await;
            }
            // Terminal: dropped from the live map.
        } else {
            self.tools.insert(tool.id.clone(), tool);
        }
    }

    // -- responses / turn state machine ------------------------------------

    async fn handle_response(&mut self, inc: Incoming) {
        let Some(id) = inc.id.as_ref().and_then(Value::as_u64) else {
            return;
        };
        if let Some(pending) = self.pending_steers.remove(&id) {
            self.on_steering_response(pending, inc.result, inc.error)
                .await;
            return;
        }
        if let Some(pending) = self.pending_mode.remove(&id) {
            let result = if let Some(error) = inc.error {
                Err(anyhow!("session/set_mode failed: {error}"))
            } else {
                if self.current_mode.as_deref() != Some(pending.mode_id.as_str()) {
                    self.current_mode = Some(pending.mode_id.clone());
                    let _ = session::set_current_mode(&self.db, &self.session_id, &pending.mode_id)
                        .await;
                    let by = pending.by.map(Value::from).unwrap_or(Value::Null);
                    self.journal_block(
                        kind::MODE_CHANGE,
                        json!({ "mode_id": pending.mode_id, "by": by }),
                    )
                    .await;
                }
                Ok(())
            };
            let _ = pending.reply.send(result);
            return;
        }
        if let Some(reply) = self.pending_config.remove(&id) {
            let result = if let Some(error) = inc.error {
                Err(anyhow!("session/set_config_option failed: {error}"))
            } else {
                match inc
                    .result
                    .and_then(|v| serde_json::from_value::<wire::SetConfigOptionResult>(v).ok())
                {
                    Some(updated) => {
                        self.replace_config_options(updated.config_options, true);
                        if let Some(mode) = &self.current_mode {
                            let _ =
                                session::set_current_mode(&self.db, &self.session_id, mode).await;
                        }
                        Ok(self.metadata.lock().unwrap().clone())
                    }
                    None => Err(anyhow!(
                        "session/set_config_option returned an invalid response"
                    )),
                }
            };
            let _ = reply.send(result);
            return;
        }
        if let Some((pid, turn)) = self.inflight_prompt {
            if id == pid {
                self.on_turn_end(turn, inc.result, inc.error).await;
            }
        }
        // Other responses need no follow-up.
    }

    async fn on_turn_end(&mut self, turn: i64, result: Option<Value>, error: Option<Value>) {
        self.current_turn = turn;
        self.flush_buf().await;
        // Flush any tool still live at turn end (mapped to `cancelled`).
        let live: Vec<LiveTool> = self.tools.drain().map(|(_, t)| t).collect();
        for t in live {
            if !chat::tool_call_exists(&self.db, &self.session_id, &t.id)
                .await
                .unwrap_or(false)
            {
                self.journal_block(kind::TOOL_CALL, t.block_payload()).await;
            }
        }
        let stop = result
            .as_ref()
            .and_then(|r| serde_json::from_value::<wire::PromptResult>(r.clone()).ok())
            .map(|p| p.stop_reason)
            .unwrap_or_else(|| {
                if error.is_some() {
                    "error".to_string()
                } else {
                    "end_turn".to_string()
                }
            });
        if !chat::has_turn_end(&self.db, &self.session_id, turn)
            .await
            .unwrap_or(false)
        {
            self.journal_block(kind::TURN_END, json!({ "stop_reason": stop }))
                .await;
        }
        // Settle the durable turn state *before* signalling the end over SSE, so a
        // client that reacts to the event sees a consistent `live_turn`.
        self.turn_live = false;
        self.inflight_prompt = None;
        self.effective_mode = None;
        let _ = session::set_inflight(&self.db, &self.session_id, None).await;
        self.emit(
            "turn",
            json!({ "turn": turn, "state": "ended", "stop_reason": stop }),
        );

        // Turn end ⇒ the `idle` lifecycle edge: stamp the quiet `idle` mark (the
        // "resting, no one needed" state). Mirrors the terminal path's `Stop` hook.
        crate::monitor::record_acp_lifecycle(&self.db, &self.bus, &self.session_id, "idle").await;

        // Stop is a user-owned boundary. In particular, do not immediately
        // turn feedback they have not seen acknowledged into another running
        // turn; leave the durable queue visible until they explicitly send it.
        if stop == "cancelled" {
            return;
        }

        // The steering extension may have raced this boundary and started the
        // user's message as a fresh adapter-owned turn. Adopt it before the
        // ordinary durable queue so prompts retain their arrival order.
        if let Some(pending) = self.pending_external.take() {
            self.begin_external_turn(pending).await;
            return;
        }

        // A steering request can finish just after the original prompt. Let its
        // response decide whether the adapter already started the next turn
        // before dispatching any later, durably queued message.
        if !self.pending_steers.is_empty() {
            return;
        }

        self.dispatch_pending_prompt().await;
    }

    async fn dispatch_pending_prompt(&mut self) {
        // Consuming the durable copy is a precondition for dispatch. Starting a
        // turn after a failed clear leaves the same text eligible at every later
        // boundary and turns one SQLite error into an unbounded replay loop.
        match session::take_pending_prompt(&self.db, &self.session_id).await {
            Ok(Some(pending)) => {
                self.emit_queue(None);
                if let Err(error) = self.start_turn(pending, None, Vec::new()).await {
                    tracing::error!(
                        session = %self.session_id,
                        %error,
                        "consumed queued prompt but could not start its turn"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(
                    session = %self.session_id,
                    %error,
                    "could not consume queued prompt; leaving it idle"
                );
            }
        }
    }

    async fn on_steering_response(
        &mut self,
        pending: PendingSteer,
        result: Option<Value>,
        error: Option<Value>,
    ) {
        let outcome = result
            .and_then(|v| serde_json::from_value::<wire::SteeringResult>(v).ok())
            .map(|r| r.outcome);
        match (outcome, error) {
            (Some(wire::SteeringOutcome::Injected), None) => {
                self.consume_promoted_queue(&pending).await;
                // A steer belongs to the live turn rather than opening another
                // item in the turn index. Journal it only after the adapter has
                // accepted it; a rejected extension falls back without leaving
                // a phantom message in the transcript.
                self.current_turn = pending.turn;
                let by = pending.by.map(Value::from).unwrap_or(Value::Null);
                self.journal_block(
                    kind::USER_MESSAGE,
                    json!({
                        "text": pending.text,
                        "by": by,
                        "steered": true,
                        "resources": pending.resources,
                    }),
                )
                .await;
                if let Some(reply) = pending.reply {
                    let _ = reply.send(Ok(PromptAck {
                        queued: false,
                        steered: true,
                        turn: Some(pending.turn),
                    }));
                }
                if !self.turn_live {
                    self.dispatch_pending_prompt().await;
                }
            }
            (Some(wire::SteeringOutcome::StartedNewTurn), None) => {
                self.consume_promoted_queue(&pending).await;
                if self.turn_live {
                    // codex-acp waits for its old prompt to settle before taking
                    // this branch. Keep the request until loom consumes that
                    // prompt response, then adopt the already-started turn.
                    self.pending_external = Some(pending);
                } else {
                    self.begin_external_turn(pending).await;
                }
            }
            (outcome, error) => {
                tracing::warn!(
                    session = %self.session_id,
                    ?outcome,
                    ?error,
                    "steering failed"
                );
                if pending.forced {
                    let detail = error
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| format!("adapter returned {outcome:?}"));
                    if let Some(reply) = pending.reply {
                        let _ = reply.send(Err(anyhow!("adapter rejected forced steer: {detail}")));
                    }
                } else {
                    self.fallback_prompt(pending).await;
                }
            }
        }
    }

    async fn consume_promoted_queue(&self, pending: &PendingSteer) {
        let Some(promoted) = pending.promoted_queue.as_deref() else {
            return;
        };
        if let Err(error) =
            session::consume_pending_prompt(&self.db, &self.session_id, promoted).await
        {
            // The adapter has already accepted the feedback, so continue the
            // turn and make the durability failure loud. A healthy database
            // preserves any messages appended behind the promoted prefix.
            tracing::error!(
                session = %self.session_id,
                %error,
                "steered queued feedback but could not consume its durable copy"
            );
        } else {
            match session::read_pending_prompt(&self.db, &self.session_id).await {
                Ok(remaining) => {
                    self.emit_queue((!remaining.trim().is_empty()).then_some(remaining.as_str()));
                }
                Err(error) => tracing::error!(
                    session = %self.session_id,
                    %error,
                    "consumed steered feedback but could not refresh the queue view"
                ),
            }
        }
    }

    async fn fallback_prompt(&mut self, pending: PendingSteer) {
        let ack = if pending.promoted_queue.is_some() {
            if self.turn_live {
                Ok(PromptAck {
                    queued: true,
                    steered: false,
                    turn: Some(self.current_turn),
                })
            } else {
                self.start_pending_prompt(pending.by).await
            }
        } else if self.turn_live {
            self.queue_prompt(&pending.text, &pending.resources).await
        } else {
            self.start_prompt(pending.text, pending.by, pending.resources)
                .await
        };
        if let Some(reply) = pending.reply {
            let _ = reply.send(ack);
        }
    }

    async fn queue_prompt(&self, text: &str, resources: &[Value]) -> Result<PromptAck> {
        self.queue_prompt_with_value(text, resources)
            .await
            .map(|(ack, _)| ack)
    }

    async fn queue_prompt_with_value(
        &self,
        text: &str,
        resources: &[Value],
    ) -> Result<(PromptAck, String)> {
        let queued = queued_prompt_text(text, resources);
        let pending = session::append_pending_prompt(&self.db, &self.session_id, &queued).await?;
        self.emit_queue(Some(&pending));
        Ok((
            PromptAck {
                queued: true,
                steered: false,
                turn: Some(self.current_turn),
            },
            pending,
        ))
    }

    async fn start_pending_prompt(&mut self, by: Option<String>) -> Result<PromptAck> {
        let pending = session::take_pending_prompt(&self.db, &self.session_id)
            .await?
            .ok_or_else(|| anyhow!("there is no queued feedback to send"))?;
        self.emit_queue(None);
        self.start_prompt(pending, by, Vec::new()).await
    }

    async fn start_prompt(
        &mut self,
        text: String,
        by: Option<String>,
        resources: Vec<Value>,
    ) -> Result<PromptAck> {
        self.start_turn(text, by, resources).await?;
        Ok(PromptAck {
            queued: false,
            steered: false,
            turn: Some(self.current_turn),
        })
    }

    async fn begin_external_turn(&mut self, pending: PendingSteer) {
        let turn = if self.turns_dispatched > 0 {
            self.current_turn + 1
        } else {
            self.current_turn
        };
        self.effective_mode = self.current_mode.clone();
        let inflight = json!({
            "turn": turn,
            "external": true,
            "mode": self.effective_mode,
        })
        .to_string();
        let persisted = session::set_inflight(&self.db, &self.session_id, Some(&inflight)).await;
        if let Err(e) = persisted {
            if let Some(reply) = pending.reply {
                let _ = reply.send(Err(e));
            }
            return;
        }
        self.current_turn = turn;
        self.next_seq = 0;
        self.turns_dispatched += 1;
        self.turn_live = true;
        self.external_turn = true;
        self.emit(
            "turn",
            json!({
                "turn": self.current_turn,
                "state": "started",
                "effective_mode": self.effective_mode,
            }),
        );
        let by = pending.by.map(Value::from).unwrap_or(Value::Null);
        self.journal_block(
            kind::USER_MESSAGE,
            json!({
                "text": pending.text,
                "by": by,
                "steered": false,
                "resources": pending.resources,
            }),
        )
        .await;
        crate::monitor::record_acp_lifecycle(&self.db, &self.bus, &self.session_id, "working")
            .await;
        if let Some(reply) = pending.reply {
            let _ = reply.send(Ok(PromptAck {
                queued: false,
                steered: false,
                turn: Some(self.current_turn),
            }));
        }
    }

    async fn start_turn(
        &mut self,
        text: String,
        by: Option<String>,
        resources: Vec<Value>,
    ) -> Result<()> {
        let by_v = by.map(Value::from).unwrap_or(Value::Null);
        self.start_turn_with_block(
            text.clone(),
            resources.clone(),
            kind::USER_MESSAGE,
            json!({ "text": text, "by": by_v, "resources": resources }),
        )
        .await
    }

    async fn start_handoff_turn(&mut self, text: String, payload: Value) -> Result<()> {
        self.start_turn_with_block(text, Vec::new(), kind::HANDOFF, payload)
            .await
    }

    async fn start_turn_with_block(
        &mut self,
        text: String,
        resources: Vec<Value>,
        opening_kind: &str,
        opening_payload: Value,
    ) -> Result<()> {
        if self.turns_dispatched > 0 {
            self.current_turn += 1;
            self.next_seq = 0;
        }
        self.turns_dispatched += 1;
        self.turn_live = true;
        self.effective_mode = self.current_mode.clone();

        self.emit(
            "turn",
            json!({
                "turn": self.current_turn,
                "state": "started",
                "effective_mode": self.effective_mode,
            }),
        );
        self.journal_block(opening_kind, opening_payload).await;

        let id = self.next_id();
        self.inflight_prompt = Some((id, self.current_turn));
        let inflight = json!({
            "prompt_id": id,
            "turn": self.current_turn,
            "mode": self.effective_mode,
        })
        .to_string();
        session::set_inflight(&self.db, &self.session_id, Some(&inflight)).await?;
        self.stream
            .write(&wire::request_line(
                id,
                method::SESSION_PROMPT,
                wire::prompt_params(&self.acp_session_id, &text, &resources),
            ))
            .await?;
        // Turn start ⇒ the `working` lifecycle edge: status `running`, the calm
        // `idle` mark and the agent's `attention` tag cleared. Mirrors what the
        // terminal path's `UserPromptSubmit` hook does (see `crate::monitor`).
        crate::monitor::record_acp_lifecycle(&self.db, &self.bus, &self.session_id, "working")
            .await;
        Ok(())
    }

    // -- permissions --------------------------------------------------------

    async fn handle_permission(&mut self, seq: u64, inc: Incoming) -> Result<()> {
        let jsonrpc_id = inc.id.clone().unwrap_or(Value::Null);
        let req_key = id_key(&jsonrpc_id);
        let params: RequestPermissionParams =
            serde_json::from_value(inc.params.unwrap_or(Value::Null))?;
        self.flush_buf().await;

        match chat::permission_outcome(&self.db, &self.session_id, &req_key).await? {
            chat::PermissionOutcome::Resolved(option_id) => {
                // Already answered before a crash: re-send the stored answer.
                let _ = self
                    .stream
                    .write(&wire::response_line(
                        &jsonrpc_id,
                        wire::permission_selected(&option_id),
                    ))
                    .await;
                return Ok(());
            }
            chat::PermissionOutcome::Open => {
                // Open block already journaled (replay): re-register the pending id.
                self.pending_perms.insert(
                    req_key.clone(),
                    PendingPerm {
                        jsonrpc_id: jsonrpc_id.clone(),
                        frame_seq: seq,
                    },
                );
            }
            chat::PermissionOutcome::Unknown => {
                let options = permission_options_json(&params.options);
                let payload = json!({
                    "request_id": req_key.clone(),
                    "tool_call_id": params.tool_call.tool_call_id.clone(),
                    "title": params.tool_call.title.clone().unwrap_or_default(),
                    "options": options,
                    "effective_mode": self.effective_mode,
                    "outcome": Value::Null,
                });
                self.journal_block(kind::PERMISSION_REQUEST, payload).await;
                self.pending_perms.insert(
                    req_key.clone(),
                    PendingPerm {
                        jsonrpc_id: jsonrpc_id.clone(),
                        frame_seq: seq,
                    },
                );
            }
        }

        // Policy follows the mode captured when this turn started. A config write
        // during the turn changes `current_mode` for the next prompt only; using it
        // here would auto-approve a request raised by an older, restricted turn.
        let bypass = self
            .effective_mode
            .as_deref()
            .is_some_and(crate::agent::is_full_access_mode);
        if bypass {
            if let Some(opt) = auto_choice(&params.options) {
                let _ = self.answer_permission(&req_key, &opt, "policy").await;
            }
        }
        Ok(())
    }

    async fn answer_permission(
        &mut self,
        request_id: &str,
        option_id: &str,
        by: &str,
    ) -> PermAnswer {
        if let Some(pp) = self.pending_perms.remove(request_id) {
            let _ = self
                .stream
                .write(&wire::response_line(
                    &pp.jsonrpc_id,
                    wire::permission_selected(option_id),
                ))
                .await;
            if let Ok(Some(view)) =
                chat::resolve_permission(&self.db, &self.session_id, request_id, option_id, by)
                    .await
            {
                self.emit("block", serde_json::to_value(&view).unwrap_or(Value::Null));
            }
            let _ = self.maybe_ack().await;
            PermAnswer::Ok
        } else {
            match chat::permission_outcome(&self.db, &self.session_id, request_id).await {
                Ok(chat::PermissionOutcome::Resolved(_)) => PermAnswer::AlreadyResolved,
                _ => PermAnswer::NotFound,
            }
        }
    }

    // -- commands -----------------------------------------------------------

    async fn cancel_live_turn(&mut self) -> Result<()> {
        let result = self
            .stream
            .write(&wire::notification_line(
                method::SESSION_CANCEL,
                wire::cancel_params(&self.acp_session_id),
            ))
            .await;
        if result.is_ok() && self.turn_live {
            for (_, pending) in self.pending_steers.drain() {
                if let Some(reply) = pending.reply {
                    let _ = reply.send(Err(anyhow!("turn was cancelled")));
                }
            }
            if let Some(pending) = self.pending_external.take() {
                if let Some(reply) = pending.reply {
                    let _ = reply.send(Err(anyhow!("turn was cancelled")));
                }
            }
            // Cancellation is a client-owned boundary. Some adapters do not
            // answer the cancelled prompt, so settle loom's journal and
            // lifecycle immediately after the notification lands.
            self.on_turn_end(
                self.current_turn,
                Some(json!({ "stopReason": "cancelled" })),
                None,
            )
            .await;
        }
        // ACP requires the client to answer every pending permission request
        // with the `cancelled` outcome when it cancels a turn.
        for (_, pp) in self.pending_perms.drain() {
            let _ = self
                .stream
                .write(&wire::response_line(
                    &pp.jsonrpc_id,
                    wire::permission_cancelled(),
                ))
                .await;
        }
        result
    }

    async fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::Prompt {
                text,
                by,
                force_steer,
                resources,
                reply,
            } => {
                if self.turn_live {
                    if self.steering_cap
                        && self.pending_steers.is_empty()
                        && self.pending_external.is_none()
                    {
                        let id = self.next_id();
                        let request = wire::request_line(
                            id,
                            method::SESSION_STEERING,
                            wire::steering_params(&self.acp_session_id, &text, &resources),
                        );
                        if force_steer {
                            // Explicit steering reports the adapter's real answer
                            // and does not also leave a queued copy behind if the
                            // private extension is rejected.
                            match self.stream.write(&request).await {
                                Ok(()) => {
                                    self.pending_steers.insert(
                                        id,
                                        PendingSteer {
                                            text,
                                            by,
                                            turn: self.current_turn,
                                            forced: true,
                                            promoted_queue: None,
                                            resources,
                                            reply: Some(reply),
                                        },
                                    );
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                }
                            }
                        } else {
                            // Durability and the HTTP acknowledgement come before
                            // the private steering RPC. If the adapter hangs,
                            // rejects, or disappears, the message remains visibly
                            // queued and the browser can keep accepting feedback.
                            match self.queue_prompt_with_value(&text, &resources).await {
                                Ok((queued_ack, promoted_queue)) => {
                                    match self.stream.write(&request).await {
                                        Ok(()) => {
                                            let _ = reply.send(Ok(queued_ack));
                                            self.pending_steers.insert(
                                                id,
                                                PendingSteer {
                                                    text,
                                                    by,
                                                    turn: self.current_turn,
                                                    forced: false,
                                                    promoted_queue: Some(promoted_queue),
                                                    resources,
                                                    reply: None,
                                                },
                                            );
                                        }
                                        Err(error) => {
                                            tracing::warn!(session = %self.session_id, %error,
                                                "steering write failed; feedback remains queued");
                                            let _ = reply.send(Ok(queued_ack));
                                        }
                                    }
                                }
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                    } else if force_steer {
                        let message = if self.steering_cap {
                            "another steer is still pending; retry when it settles"
                        } else {
                            "this agent does not support steering; queue the feedback or stop and send it"
                        };
                        let _ = reply.send(Err(anyhow!(message)));
                    } else {
                        // A failed queue write must surface — a 202 that silently
                        // dropped the prompt would be worse than an error.
                        let ack = self.queue_prompt(&text, &resources).await;
                        let _ = reply.send(ack);
                    }
                } else {
                    let ack = self.start_prompt(text, by, resources).await;
                    let _ = reply.send(ack);
                }
            }
            Command::ForcePending { by, reply } => {
                if !self.pending_steers.is_empty() || self.pending_external.is_some() {
                    let _ = reply.send(Err(anyhow!(
                        "another steer is still pending; retry when it settles"
                    )));
                } else {
                    let queued =
                        match session::read_pending_prompt(&self.db, &self.session_id).await {
                            Ok(queued) => queued,
                            Err(error) => {
                                let _ = reply.send(Err(error));
                                return;
                            }
                        };
                    if queued.trim().is_empty() {
                        let _ = reply.send(Err(anyhow!("there is no queued feedback to send")));
                    } else if !self.turn_live {
                        let result = self.start_pending_prompt(by).await;
                        let _ = reply.send(result);
                    } else if !self.steering_cap {
                        let result = match self.cancel_live_turn().await {
                            Ok(()) => self.start_pending_prompt(by).await,
                            Err(error) => Err(error),
                        };
                        let _ = reply.send(result);
                    } else {
                        let id = self.next_id();
                        let request = wire::request_line(
                            id,
                            method::SESSION_STEERING,
                            wire::steering_params(&self.acp_session_id, &queued, &[]),
                        );
                        match self.stream.write(&request).await {
                            Ok(()) => {
                                self.pending_steers.insert(
                                    id,
                                    PendingSteer {
                                        text: queued.clone(),
                                        by,
                                        turn: self.current_turn,
                                        forced: true,
                                        promoted_queue: Some(queued),
                                        resources: Vec::new(),
                                        reply: Some(reply),
                                    },
                                );
                            }
                            Err(error) => {
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                }
            }
            Command::RetractPending { reply } => {
                // A steering RPC may already have handed this text to the
                // adapter even though its durable fallback is still visible.
                // Once that request is in flight, claiming the text is editable
                // would be a lie: the eventual response can consume or run it.
                if !self.pending_steers.is_empty() || self.pending_external.is_some() {
                    let _ = reply.send(Err(anyhow!(
                        "queued feedback is already being steered; wait for it to settle"
                    )));
                } else {
                    let result = session::take_pending_prompt(&self.db, &self.session_id)
                        .await
                        .and_then(|pending| {
                            pending.ok_or_else(|| anyhow!("there is no queued feedback to edit"))
                        });
                    if result.is_ok() {
                        self.emit_queue(None);
                    }
                    let _ = reply.send(result);
                }
            }
            Command::Cancel { reply } => {
                let _ = reply.send(self.cancel_live_turn().await);
            }
            Command::AnswerPermission {
                request_id,
                option_id,
                by,
                reply,
            } => {
                let ans = self.answer_permission(&request_id, &option_id, &by).await;
                let _ = reply.send(ans);
            }
            Command::SetMode { mode_id, by, reply } => {
                let id = self.next_id();
                let write = self
                    .stream
                    .write(&wire::request_line(
                        id,
                        method::SESSION_SET_MODE,
                        wire::set_mode_params(&self.acp_session_id, &mode_id),
                    ))
                    .await;
                match write {
                    Ok(()) => {
                        self.pending_mode
                            .insert(id, PendingMode { mode_id, by, reply });
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Command::SetConfigOption {
                config_id,
                value,
                reply,
            } => {
                let id = self.next_id();
                let write = self
                    .stream
                    .write(&wire::request_line(
                        id,
                        method::SESSION_SET_CONFIG_OPTION,
                        wire::set_config_option_params(
                            self.acp_session_id.as_str(),
                            &config_id,
                            value,
                        ),
                    ))
                    .await;
                match write {
                    Ok(()) => {
                        self.pending_config.insert(id, reply);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            // The run loop intercepts this variant so it can acknowledge only
            // after registry removal. Keep the match exhaustive defensively.
            Command::PrepareHandoff { reply } => {
                let _ = reply.send(Err(anyhow!("handoff command reached the task dispatcher")));
            }
        }
    }

    // -- exit ---------------------------------------------------------------

    async fn on_exit(&mut self, status: Option<i32>) {
        tracing::warn!(session = %self.session_id, ?status, "acp agent exited");
        let ending_turn = self
            .inflight_prompt
            .take()
            .map(|(_, turn)| turn)
            .or_else(|| self.external_turn.then_some(self.current_turn));
        if let Some(turn) = ending_turn {
            self.flush_buf().await;
            if !chat::has_turn_end(&self.db, &self.session_id, turn)
                .await
                .unwrap_or(false)
            {
                self.journal_block(kind::TURN_END, json!({ "stop_reason": "error" }))
                    .await;
            }
            self.emit(
                "turn",
                json!({ "turn": turn, "state": "ended", "stop_reason": "error" }),
            );
            self.turn_live = false;
            self.external_turn = false;
            self.effective_mode = None;
            let _ = session::set_inflight(&self.db, &self.session_id, None).await;
        }
    }

    // -- journal + ack ------------------------------------------------------

    async fn journal_block(&mut self, kind: &str, payload: Value) -> ChatBlockView {
        let turn = self.current_turn;
        let seq = self.next_seq;
        // A `session/load` replay is already in the journal: don't rewrite it, and
        // leave the seq cursor where the seeded continuation expects it.
        if self.suppress_journal {
            return ChatBlockView {
                turn,
                seq,
                kind: kind.to_string(),
                payload,
                created_at: now_iso(),
            };
        }
        self.next_seq += 1;
        let inserted = chat::insert(&self.db, &self.session_id, turn, seq, kind, &payload).await;
        let view = ChatBlockView {
            turn,
            seq,
            kind: kind.to_string(),
            payload,
            created_at: now_iso(),
        };
        if let Err(e) = inserted {
            // The block never became durable: freeze the ack watermark (its
            // frames must replay after a restart) and don't announce it.
            tracing::error!(session = %self.session_id, turn, seq, kind, error = %e,
                "chat journal write failed; freezing ack watermark");
            self.journal_failed = true;
            return view;
        }
        self.emit("block", serde_json::to_value(&view).unwrap_or(Value::Null));
        view
    }

    /// The highest seq safe to ack: just before the earliest frame still feeding
    /// an open buffer, live tool, or unanswered permission — else the highest
    /// frame seen.
    fn safe_watermark(&self) -> u64 {
        let mut min_pending: Option<u64> = None;
        let mut track = |s: u64| {
            min_pending = Some(min_pending.map_or(s, |m| m.min(s)));
        };
        if let Some(b) = &self.buf {
            track(b.first_seq);
        }
        for t in self.tools.values() {
            track(t.first_seq);
        }
        for p in self.pending_perms.values() {
            track(p.frame_seq);
        }
        match min_pending {
            Some(m) => m.saturating_sub(1),
            None => self.highest_seq,
        }
    }

    async fn maybe_ack(&mut self) -> Result<()> {
        if self.journal_failed {
            return Ok(());
        }
        let w = self.safe_watermark();
        if w > self.acked {
            self.stream.ack(w).await?;
            session::set_ack_seq(&self.db, &self.session_id, w as i64).await?;
            self.acked = w;
        }
        Ok(())
    }
}

/// The one-shot grant an auto-answer may select, if the adapter offers one.
fn auto_choice(options: &[PermissionOption]) -> Option<String> {
    options
        .iter()
        // Never turn a no-prompt posture into a persisted policy mutation. If
        // the adapter offers no one-shot grant, surface the request for an
        // explicit answer instead of guessing.
        .find(|o| o.kind == "allow_once")
        .map(|o| o.option_id.clone())
}

/// Render permission options into the journal contract's `options` array.
fn permission_options_json(options: &[PermissionOption]) -> Vec<Value> {
    options
        .iter()
        .map(|o| {
            json!({
                "option_id": o.option_id.clone(),
                "name": o.name.clone(),
                "kind": o.kind.clone(),
            })
        })
        .collect()
}

/// A JSON-RPC id as a stable string key (numbers stringify, strings pass through).
fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Render ACP resources into the durable text-only pending queue. Prefer the
/// worktree-relative display name so queued prompts stay readable and do not
/// expose the server's absolute filesystem layout.
fn queued_prompt_text(text: &str, resources: &[Value]) -> String {
    let references: Vec<&str> = resources
        .iter()
        .filter_map(|resource| {
            resource
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .or_else(|| resource.get("uri").and_then(Value::as_str))
        })
        .collect();
    if references.is_empty() {
        return text.to_string();
    }

    let mut queued = format!("{text}\n\nReferenced files:\n");
    for reference in references {
        queued.push_str("- ");
        queued.push_str(reference);
        queued.push('\n');
    }
    queued
}

#[cfg(test)]
mod tests {
    use super::{auto_choice, queued_prompt_text, PermissionOption};
    use serde_json::json;

    #[test]
    fn queued_resources_prefer_relative_names_over_server_uris() {
        let resources = [
            json!({"name": "src/main.rs", "uri": "file:///server/worktree/src/main.rs"}),
            json!({"uri": "https://example.test/context"}),
        ];

        assert_eq!(
            queued_prompt_text("review", &resources),
            "review\n\nReferenced files:\n- src/main.rs\n- https://example.test/context\n"
        );
    }

    #[test]
    fn automatic_permission_choice_never_persists_policy() {
        let options = [
            PermissionOption {
                option_id: "persist".to_string(),
                name: "Always allow".to_string(),
                kind: "allow_always".to_string(),
            },
            PermissionOption {
                option_id: "once".to_string(),
                name: "Allow once".to_string(),
                kind: "allow_once".to_string(),
            },
        ];
        assert_eq!(auto_choice(&options).as_deref(), Some("once"));
        assert_eq!(auto_choice(&options[..1]), None);
    }
}
