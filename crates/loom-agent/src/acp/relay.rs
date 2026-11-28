//! The tapestry relay as the agent-client-protocol crate's `Channel` transport.
//!
//! The relay spools every adapter-stdout frame with a sequence number and
//! replays from the last acked watermark after a restart. One pump task joins
//! the two halves of a [`Channel::duplex`] pair to the relay: adapter frames
//! become [`TransportFrame`]s for the connection engine, and the engine's
//! outgoing frames become relay writes. Because the crate's dispatch loop is
//! serial — it does not poll the transport for the next message until the
//! current one is handled — the pump's ack mode decides how far the engine
//! may read ahead:
//!
//! - [`AckMode::Auto`] (transient clients) acks each frame as it is
//!   delivered: nothing durable hangs on those sessions.
//! - [`AckMode::Gated`] (the live session task) withholds the next frame
//!   until the task marked the previous one processed, and the task acks
//!   through [`RelayHandle::ack`] with its own safe watermark — the engine
//!   can never outrun the journal.

use std::sync::{Arc, Mutex as StdMutex};

use agent_client_protocol::{Channel, TransportFrame};
use anyhow::Result;
use tokio::sync::{watch, Mutex as AsyncMutex};

use tapestry::{RelayEvent, RelayStream};

/// The live session's read-ahead gate: at most one delivered frame may sit
/// unprocessed in front of the task.
///
/// Everything gated here is exercised by the live session task's migration
/// onto the crate, which lands directly after the transient-client step.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct RelayGate {
    state: Arc<StdMutex<GateState>>,
    free_tx: watch::Sender<bool>,
}

#[derive(Default)]
struct GateState {
    /// The seq of the frame delivered to the engine but not yet marked
    /// processed by the session task.
    delivered: Option<u64>,
}

impl RelayGate {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        let (free_tx, _) = watch::channel(true);
        Self {
            state: Arc::default(),
            free_tx,
        }
    }

    /// Block until no delivered frame is waiting on the session task.
    pub(crate) async fn await_free(&self) {
        let mut free = self.free_tx.subscribe();
        loop {
            if self.state.lock().unwrap().delivered.is_none() {
                return;
            }
            let _ = free.changed().await;
        }
    }

    fn record_delivered(&self, seq: u64) {
        self.state.lock().unwrap().delivered = Some(seq);
    }

    /// The session task finished handling the frame it was delivered.
    #[allow(dead_code)]
    pub(crate) fn mark_processed(&self) {
        self.state.lock().unwrap().delivered = None;
        let _ = self.free_tx.send(true);
    }
}

/// How the pump advances the relay's retention watermark.
pub(crate) enum AckMode {
    /// Ack every frame as it is delivered.
    Auto,
    /// Withhold the next frame until the task processed the previous one;
    /// the task acks via [`RelayHandle::ack`]. Used by the live session
    /// task's crate migration.
    #[allow(dead_code)]
    Gated(RelayGate),
}

/// The task-side face of a connected relay transport: spool acks and the
/// adapter's exit status, which travel outside the JSON-RPC stream.
pub(crate) struct RelayHandle {
    writer: Arc<AsyncMutex<tapestry::RelayWriter>>,
    exit: Arc<StdMutex<Option<Option<i32>>>>,
}

impl RelayHandle {
    /// Advance the retention watermark: everything up to and including `seq`
    /// is durably processed.
    #[allow(dead_code)]
    pub(crate) async fn ack(&self, seq: u64) -> Result<()> {
        self.writer.lock().await.ack(seq).await
    }

    /// The adapter's exit status once the relay reported it, `None` while it
    /// runs. `Some(None)` means the exit status is unknown.
    pub(crate) fn exit_status(&self) -> Option<Option<i32>> {
        *self.exit.lock().unwrap()
    }
}

/// Connect a relay subscription as the crate's channel transport. The pump
/// task runs until the relay ends (adapter exit, supervisor drop) or the
/// connection closes, whichever comes first.
pub(crate) fn connect(stream: RelayStream, mode: AckMode) -> (Channel, RelayHandle) {
    let (engine_side, pump_side) = Channel::duplex();
    let (writer, mut events) = stream.split();
    let writer = Arc::new(AsyncMutex::new(writer));
    let exit: Arc<StdMutex<Option<Option<i32>>>> = Arc::default();
    let pump_exit = exit.clone();
    let handle_writer = writer.clone();

    let gate = match mode {
        AckMode::Auto => None,
        AckMode::Gated(gate) => Some(gate),
    };
    let auto_ack = gate.is_none();

    tokio::spawn(async move {
        use futures::StreamExt;
        let (mut to_pump, pump_tx) = pump_side.into_parts();
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(RelayEvent::Frame { seq, payload }) => {
                        if let Some(gate) = &gate {
                            gate.await_free().await;
                            gate.record_delivered(seq);
                        }
                        if auto_ack {
                            if let Err(error) = writer.lock().await.ack(seq).await {
                                tracing::warn!(%error, "relay ack failed");
                            }
                        }
                        let line = String::from_utf8_lossy(&payload);
                        if pump_tx.unbounded_send(TransportFrame::parse_json(&line)).is_err() {
                            break;
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
    });

    (
        engine_side,
        RelayHandle {
            writer: handle_writer,
            exit,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gate stays closed while the delivered frame is unprocessed, and
    /// opens the moment the task marks it processed.
    #[tokio::test]
    async fn the_gate_serializes_delivery_to_the_task() {
        let gate = RelayGate::new();
        gate.record_delivered(7);
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
        assert!(
            tokio::time::timeout(Duration::from_millis(50), gate.await_free())
                .await
                .is_err(),
            "the gate must close for the next frame"
        );
    }
}
