//! In-process server-log capture: a bounded ring buffer plus a live broadcast
//! that a `tracing` layer tees every event into, so an operator can read recent
//! server logs from the web UI (Settings → Logs) without shelling into the box —
//! the difference between a local dev server (logs in your terminal) and the
//! Docker deploy (logs behind `docker compose logs`).
//!
//! The buffer is a process global ([`buffer`]) so the tracing layer — installed
//! at startup, long before the web server exists — and the HTTP handlers share
//! one instance. It only *tees*: the stdout `fmt` layer is untouched, so
//! `docker compose logs` still gets everything.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// How many recent lines the snapshot buffer retains. A few thousand is plenty to
/// debug a just-happened failure and stays cheap in memory (~a few hundred KB).
const CAPACITY: usize = 2000;
/// Bound on the live broadcast channel; a slow subscriber that falls this far
/// behind is dropped (it can re-fetch the snapshot).
const BROADCAST_CAPACITY: usize = 256;

/// One captured log line, as the UI renders it.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    /// Monotonic sequence number, so the UI can dedupe the snapshot against the
    /// live stream (and detect drops) without comparing timestamps.
    pub seq: u64,
    /// RFC3339 UTC timestamp.
    pub ts: String,
    /// `ERROR` | `WARN` | `INFO` | `DEBUG` | `TRACE`.
    pub level: String,
    /// The event's target (module path, e.g. `loom::web::repos`).
    pub target: String,
    /// The rendered message plus any structured fields.
    pub message: String,
}

/// The shared log store: a bounded ring buffer for the snapshot plus a broadcast
/// channel for live subscribers. Mirrors [`weaver_core::events::EventBus`].
pub struct LogBuffer {
    ring: Mutex<VecDeque<LogLine>>,
    tx: broadcast::Sender<LogLine>,
    seq: AtomicU64,
    /// When this process started capturing (≈ process start), for the status line.
    started_at: String,
}

impl LogBuffer {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            ring: Mutex::new(VecDeque::with_capacity(CAPACITY)),
            tx,
            seq: AtomicU64::new(0),
            started_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Append a line: stamp a sequence number, push to the ring (evicting the
    /// oldest past capacity), and fan out to live subscribers. Never blocks on a
    /// subscriber. Holds the ring lock only for the push — and emits no `tracing`
    /// event itself, so it can never re-enter this layer and deadlock.
    fn push(&self, mut line: LogLine) {
        line.seq = self.seq.fetch_add(1, Ordering::Relaxed);
        {
            let mut ring = self.ring.lock().expect("log ring poisoned");
            if ring.len() == CAPACITY {
                ring.pop_front();
            }
            ring.push_back(line.clone());
        }
        // Err only means there are no live subscribers; that is fine.
        let _ = self.tx.send(line);
    }

    /// The most recent `limit` lines, oldest first.
    pub fn snapshot(&self, limit: usize) -> Vec<LogLine> {
        let ring = self.ring.lock().expect("log ring poisoned");
        let start = ring.len().saturating_sub(limit);
        ring.iter().skip(start).cloned().collect()
    }

    /// Subscribe to lines appended from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.tx.subscribe()
    }

    /// When capture began (≈ process start), RFC3339.
    pub fn started_at(&self) -> &str {
        &self.started_at
    }
}

/// The process-global log buffer, created on first access. Both the tracing layer
/// and the HTTP handlers resolve the same instance through this.
pub fn buffer() -> &'static Arc<LogBuffer> {
    static BUFFER: OnceLock<Arc<LogBuffer>> = OnceLock::new();
    BUFFER.get_or_init(|| Arc::new(LogBuffer::new()))
}

/// A `tracing` [`Layer`] that tees each event into the global [`buffer`]. Add it
/// to the subscriber registry alongside the stdout `fmt` layer.
pub fn layer() -> CaptureLayer {
    CaptureLayer
}

/// The layer type returned by [`layer`].
pub struct CaptureLayer;

/// A span's rendered fields (e.g. `" method=GET path=/api/tasks"`), stashed in the
/// span's extensions by [`CaptureLayer::on_new_span`] and folded into every event
/// logged within that span — so a line carries the request it belongs to even when
/// the event itself never named it.
struct SpanFields(String);

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor::default();
        attrs.record(&mut visitor);
        if !visitor.fields.is_empty() {
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(SpanFields(visitor.fields));
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        // Fold in the enclosing span scope's fields (root → leaf), so a line logged
        // while handling a request carries its `method`/`path` even though the event
        // never named them — e.g. `authentication required status=401 method=GET
        // path=/api/tasks` instead of a context-free `status=401`.
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<SpanFields>() {
                    visitor.fields.push_str(&fields.0);
                }
            }
        }
        buffer().push(LogLine {
            seq: 0, // assigned in push()
            ts: chrono::Utc::now().to_rfc3339(),
            level: level_str(*meta.level()).to_string(),
            target: meta.target().to_string(),
            message: visitor.finish(),
        });
    }
}

fn level_str(level: Level) -> &'static str {
    match level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

/// Renders an event's `message` plus structured fields into one string, e.g.
/// `github webhook: launched session session=abc repo=acme/widgets`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl MessageVisitor {
    fn finish(self) -> String {
        match (self.message.is_empty(), self.fields.is_empty()) {
            (false, true) => self.message,
            (true, false) => self.fields.trim_start().to_string(),
            (true, true) => String::new(),
            (false, false) => format!("{}{}", self.message, self.fields),
        }
    }
}

impl Visit for MessageVisitor {
    /// String fields render without the `Debug` quotes (`repo=acme/widgets`).
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            use std::fmt::Write;
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    /// Everything else — including the `message` (recorded as `format_args!`,
    /// whose `Debug` is the plain text) and non-string fields.
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            use std::fmt::Write;
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_caps_and_orders() {
        let buf = LogBuffer::new();
        for i in 0..(CAPACITY + 50) {
            buf.push(LogLine {
                seq: 0,
                ts: "t".into(),
                level: "INFO".into(),
                target: "test".into(),
                message: format!("line {i}"),
            });
        }
        let snap = buf.snapshot(CAPACITY);
        assert_eq!(snap.len(), CAPACITY, "capped at CAPACITY");
        assert_eq!(snap.first().unwrap().message, "line 50", "oldest evicted");
        assert_eq!(
            snap.last().unwrap().message,
            format!("line {}", CAPACITY + 49),
            "newest kept, oldest-first order"
        );
        // Sequence numbers are monotonic across the whole run, not just the window.
        assert!(snap.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    }

    #[test]
    fn snapshot_limit_returns_most_recent() {
        let buf = LogBuffer::new();
        for i in 0..10 {
            buf.push(LogLine {
                seq: 0,
                ts: "t".into(),
                level: "INFO".into(),
                target: "test".into(),
                message: i.to_string(),
            });
        }
        let snap = buf.snapshot(3);
        let msgs: Vec<&str> = snap.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(msgs, ["7", "8", "9"]);
    }

    #[test]
    fn folds_enclosing_span_fields_into_event() {
        use tracing_subscriber::prelude::*;

        // Install a registry + our capture layer for this thread, emit a warn
        // inside a `method`/`path` span, then find our line in the global buffer by
        // a unique marker (other tests may share the buffer).
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("http", method = "GET", path = "/api/tasks");
            let _g = span.enter();
            tracing::warn!("mark-9f3c authentication required");
        });

        let snap = buffer().snapshot(2000);
        let line = snap
            .iter()
            .rev()
            .find(|l| l.message.contains("mark-9f3c"))
            .expect("the event was captured");
        assert!(
            line.message.contains("method=GET"),
            "line carries the span's method: {}",
            line.message
        );
        assert!(
            line.message.contains("path=/api/tasks"),
            "line carries the span's path: {}",
            line.message
        );
    }

    #[test]
    fn visitor_joins_message_and_fields() {
        // Simulate what record_str produces for message + fields.
        let v = MessageVisitor {
            message: "launched session".into(),
            fields: " session=abc repo=acme/widgets".into(),
        };
        assert_eq!(v.finish(), "launched session session=abc repo=acme/widgets");
    }
}
