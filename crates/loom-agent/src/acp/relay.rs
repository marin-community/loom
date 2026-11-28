//! The tapestry relay as the agent-client-protocol crate's `Channel` transport.
//!
//! The relay spools every adapter-stdout frame with a sequence number and
//! replays from the last acked watermark after a restart. One pump task joins
//! the two halves of a [`Channel::duplex`] pair to the relay: adapter frames
//! become [`TransportFrame`]s for the connection engine, and the engine's
//! outgoing frames become relay writes.
//!
//! Two frame-tracking pieces ride along:
//!
//! - [`RelayGate`] hands each dispatched frame's spool sequence to the
//!   connection's handlers ([`RelayGate::frame_seq`]) and is released by the
//!   handlers once a frame's dispatch is complete. The pump waits for that
//!   before sending the next frame — with a generous watchdog, so an
//!   unexpected engine path can slow the session but never freeze it — which
//!   is what makes `frame_seq` exact: at most one frame is ever in flight
//!   between the relay and the engine's handlers.
//! - [`RelayHandle`] carries the writer the session task acks with (its own
//!   safe watermark — the engine never acks on its own) and the adapter's
//!   exit status, which travels outside the JSON-RPC stream.
//!
//! The transient clients instead use [`AckMode::Auto`]: the pump acks every
//! frame as it delivers it, because nothing durable hangs on those sessions.

use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use agent_client_protocol::{
    Channel, ErrorCode, RawJsonRpcId, RawJsonRpcMessage, RawJsonRpcResponse, TransportFrame,
};
use anyhow::Result;
use tokio::sync::mpsc;
use tokio::sync::{watch, Mutex as AsyncMutex};

use super::{EngineEvent, ResponseKind};
use tapestry::{RelayEvent, RelayStream};

/// How long the pump waits for a dispatched frame to be marked processed
/// before warning and moving on. Frame dispatch completes in microseconds; a
/// miss this long means an engine path loom does not hook, which must never
/// wedge a live session.
const FRAME_DISPATCH_WATCHDOG: Duration = Duration::from_secs(30);

/// The frame tracker shared by the pump and the connection's handlers.
#[derive(Clone)]
pub(crate) struct RelayGate {
    state: Arc<StdMutex<GateState>>,
    free_tx: watch::Sender<bool>,
}

#[derive(Default)]
struct GateState {
    /// The seq of the frame delivered to the engine but not yet marked
    /// processed by its handlers.
    delivered: Option<u64>,
}

impl RelayGate {
    pub(crate) fn new() -> Self {
        let (free_tx, _) = watch::channel(true);
        Self {
            state: Arc::default(),
            free_tx,
        }
    }

    /// The spool seq of the frame the engine is dispatching right now. Exact
    /// while a handler runs: the pump cannot deliver the next frame until
    /// this one is marked processed.
    pub(crate) fn frame_seq(&self) -> u64 {
        self.state.lock().unwrap().delivered.unwrap_or(0)
    }

    /// The handlers finished with the frame they were delivered.
    pub(crate) fn mark_processed(&self) {
        self.state.lock().unwrap().delivered = None;
        let _ = self.free_tx.send(true);
    }

    fn record_delivered(&self, seq: u64) {
        self.state.lock().unwrap().delivered = Some(seq);
    }

    /// Block until no delivered frame is waiting on the handlers.
    pub(crate) async fn await_free(&self) {
        let mut free = self.free_tx.subscribe();
        loop {
            if self.state.lock().unwrap().delivered.is_none() {
                return;
            }
            let _ = free.changed().await;
        }
    }
}

/// How the relay's retention watermark advances.
pub(crate) enum AckMode {
    /// The pump acks every frame as it delivers it.
    Auto,
    /// The session task acks through [`RelayHandle::ack`] with its own safe
    /// watermark; the pump never acks. The gate tracks per-frame dispatch so
    /// handlers can stamp spool seqs onto the events they forward.
    Gated(RelayGate),
}

/// The task-side face of a connected relay transport: spool acks, the
/// adapter's exit status, and the pump's end — all of which travel outside
/// the JSON-RPC stream. The session task selects on [`RelayHandle::closed`]
/// so a relay eviction or supervisor loss ends it cleanly even while the
/// connection engine still holds its event senders.
pub(crate) struct RelayHandle {
    writer: Arc<AsyncMutex<tapestry::RelayWriter>>,
    exit: Arc<StdMutex<Option<Option<i32>>>>,
    closed_rx: watch::Receiver<bool>,
}

impl RelayHandle {
    /// Advance the retention watermark: everything up to and including `seq`
    /// is durably processed.
    pub(crate) async fn ack(&self, seq: u64) -> Result<()> {
        self.writer.lock().await.ack(seq).await
    }

    /// The adapter's exit status once the relay reported it, `None` while it
    /// runs. `Some(None)` means the exit status is unknown.
    pub(crate) fn exit_status(&self) -> Option<Option<i32>> {
        *self.exit.lock().unwrap()
    }

    /// Whether the pump has ended. A request the engine fails *after* this
    /// is a teardown artifact — the relay died, not the adapter answering —
    /// so its durable state must stay for the successor task, which will see
    /// any real response the adapter did send in the spool replay.
    pub(crate) fn is_closed(&self) -> bool {
        *self.closed_rx.borrow()
    }

    /// Resolves when the pump ends: the relay reported the adapter's exit,
    /// the supervisor dropped the stream, or a successor's subscription
    /// evicted this one.
    pub(crate) async fn closed(&mut self) {
        let mut closed = self.closed_rx.clone();
        loop {
            if *closed.borrow() {
                return;
            }
            if closed.changed().await.is_err() {
                return;
            }
        }
    }
}

/// The ids of responses the engine cannot know about: requests sent by a
/// *previous* session task, carried across the restart by durable state. The
/// engine would drop the replayed response as "unknown id", so the pump adopts
/// it — forwarding it straight to the session task, which routes by id.
pub(crate) type AdoptedRequests = std::collections::HashSet<String>;

/// The request id exactly as it appears on the wire (integers and strings
/// rendered bare), which is what durable records store.
fn request_id_string(id: &RawJsonRpcId) -> String {
    id.to_string()
}

/// The id a raw response answers, in either of its shapes.
fn raw_response_id(response: &RawJsonRpcResponse) -> &RawJsonRpcId {
    match response {
        RawJsonRpcResponse::Result { id, .. } => id,
        RawJsonRpcResponse::Error { id, .. } => id,
    }
}

/// A raw response as the engine's request outcomes carry it.
fn raw_response_outcome(response: RawJsonRpcResponse) -> super::RequestOutcome {
    match response {
        RawJsonRpcResponse::Result { result, .. } => Ok(result),
        RawJsonRpcResponse::Error { error, .. } => {
            let data = match error.data {
                agent_client_protocol::schema::MaybeUndefined::Value(value) => Some(value),
                _ => None,
            };
            Err(
                agent_client_protocol::Error::new(ErrorCode::new(error.code), error.message)
                    .data(data),
            )
        }
    }
}

/// Connect a relay subscription as the crate's channel transport. The pump
/// task runs until the relay ends (adapter exit, supervisor drop) or the
/// connection closes, whichever comes first.
pub(crate) fn connect(
    stream: RelayStream,
    mode: AckMode,
    task_events: mpsc::UnboundedSender<EngineEvent>,
    mut adopted: AdoptedRequests,
) -> (Channel, RelayHandle, Option<RelayGate>) {
    let (engine_side, pump_side) = Channel::duplex();
    let (writer, mut events) = stream.split();
    let writer = Arc::new(AsyncMutex::new(writer));
    let exit: Arc<StdMutex<Option<Option<i32>>>> = Arc::default();
    let pump_exit = exit.clone();
    let handle_writer = writer.clone();
    let (closed_tx, closed_rx) = watch::channel(false);

    let returned_gate = match &mode {
        AckMode::Auto => None,
        AckMode::Gated(gate) => Some(gate.clone()),
    };
    let gate = match mode {
        AckMode::Auto => None,
        AckMode::Gated(gate) => Some(gate),
    };
    let auto_ack = gate.is_none();

    tokio::spawn(async move {
        use futures::StreamExt;
        let (mut to_pump, pump_tx) = pump_side.into_parts();
        // The ids of requests this connection wrote. A response to one of
        // them has a subscriber in the engine; anything else is a replay of a
        // request a previous task sent — the engine would drop it as an
        // unknown id, so the pump settles it itself (adopted ones go to the
        // task, the rest are dead replays no state waits on).
        let mut live_requests: HashSet<String> = HashSet::new();
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(RelayEvent::Frame { seq, payload }) => {
                        if let Some(gate) = &gate {
                            // Wait out the previous frame before overwriting
                            // the seq the handlers read. The watchdog turns an
                            // unhooked engine path into a warning, not a wedge.
                            if tokio::time::timeout(
                                FRAME_DISPATCH_WATCHDOG,
                                gate.await_free(),
                            )
                            .await
                            .is_err()
                            {
                                tracing::warn!(
                                    seq,
                                    "a relay frame was not marked processed; continuing"
                                );
                            }
                            gate.record_delivered(seq);
                        }
                        if auto_ack {
                            if let Err(error) = writer.lock().await.ack(seq).await {
                                tracing::warn!(%error, "relay ack failed");
                            }
                        }
                        let line = String::from_utf8_lossy(&payload);
                        match TransportFrame::parse_json(&line) {
                            // A malformed frame never reaches the engine's
                            // handlers, so nothing would release the gate.
                            // Adapters do not act on error replies to it, and
                            // the durable task drops it too — drop here.
                            frame @ TransportFrame::Malformed { .. } => {
                                if let Some(gate) = &gate {
                                    gate.mark_processed();
                                }
                                let _ = frame;
                                tracing::warn!(session_frame = %line, "unparseable acp frame");
                            }
                            TransportFrame::Single(RawJsonRpcMessage::Response(response)) => {
                                let id = request_id_string(raw_response_id(&response));
                                if adopted.remove(&id) {
                                    // A replayed response the durable state
                                    // carries: forward it to the task, which
                                    // routes it by id.
                                    let outcome = raw_response_outcome(response);
                                    let delivered = task_events
                                        .send(EngineEvent::Response {
                                            seq,
                                            id,
                                            kind: ResponseKind::Replayed,
                                            outcome,
                                        })
                                        .is_ok();
                                    if let Some(gate) = &gate {
                                        gate.mark_processed();
                                    }
                                    if !delivered {
                                        break;
                                    }
                                } else if live_requests.remove(&id) {
                                    // This connection asked; the engine has a
                                    // subscriber for the reply.
                                    if pump_tx
                                        .unbounded_send(TransportFrame::Single(
                                            RawJsonRpcMessage::Response(response),
                                        ))
                                        .is_err()
                                    {
                                        break;
                                    }
                                } else {
                                    // A dead replay the engine would drop as
                                    // an unknown id. No durable state waits on
                                    // it (the task's reply maps die with it),
                                    // so drop it here and release the frame.
                                    tracing::debug!(
                                        id = %id,
                                        "dropping replayed response to a dead request"
                                    );
                                    if let Some(gate) = &gate {
                                        gate.mark_processed();
                                    }
                                }
                            }
                            frame => {
                                if pump_tx.unbounded_send(frame).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(RelayEvent::Exit { status }) => {
                        *pump_exit.lock().unwrap() = Some(status);
                        break;
                    }
                    None => break,
                },
                frame = to_pump.next() => match frame {
                    Some(frame) => {
                        if let TransportFrame::Single(agent_client_protocol::RawJsonRpcMessage::Request(request)) =
                            &frame
                        {
                            live_requests.insert(request_id_string(&request.id));
                        }
                        let mut line = match frame.to_json() {
                            Ok(line) => line,
                            Err(error) => {
                                tracing::warn!(%error, "serializing outgoing ACP frame");
                                continue;
                            }
                        };
                        line.push('\n');
                        if let Err(error) = writer.lock().await.write(line.as_bytes()).await {
                            tracing::warn!(%error, "relay write failed");
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
        // Dropping pump_tx closes the engine's transport; the connection ends
        // with it. Dropping to_pump ends our side when the engine went first.
        let _ = closed_tx.send(true);
    });

    (
        engine_side,
        RelayHandle {
            writer: handle_writer,
            exit,
            closed_rx,
        },
        returned_gate,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gate stays closed while the delivered frame is unprocessed, and
    /// opens the moment the handlers mark it processed.
    #[tokio::test]
    async fn the_gate_serializes_delivery_to_the_handlers() {
        let gate = RelayGate::new();
        gate.record_delivered(7);
        assert_eq!(gate.frame_seq(), 7);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), gate.await_free())
                .await
                .is_err(),
            "the gate must stay closed while the frame is unprocessed"
        );
        gate.mark_processed();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), gate.await_free())
                .await
                .is_ok(),
            "the gate must open once the frame is processed"
        );

        // A second delivery closes it again.
        gate.record_delivered(8);
        assert_eq!(gate.frame_seq(), 8);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), gate.await_free())
                .await
                .is_err(),
            "the gate must close for the next frame"
        );
    }
}
