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

/// Redacts credentials from log lines before they are shown to a non-admin
/// user. Admins retain the raw operator log; callers construct this with the
/// deployment's known secret values for the user-facing view.
#[derive(Debug, Clone)]
pub struct LogRedactor {
    known_secrets: Vec<String>,
}

impl LogRedactor {
    /// Build a redactor from secret values already resolved by the caller.
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut known_secrets: Vec<String> = secrets
            .into_iter()
            .filter(|secret| secret.trim().len() >= 4)
            .collect();
        known_secrets.sort();
        known_secrets.dedup();
        known_secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        Self { known_secrets }
    }

    /// Return the same structured line with only its free-form message scrubbed.
    pub fn redact(&self, mut line: LogLine) -> LogLine {
        line.message = self.redact_message(&line.message);
        line
    }

    fn redact_message(&self, message: &str) -> String {
        let mut redacted = message.to_string();
        for secret in &self.known_secrets {
            redacted = redacted.replace(secret, "<redacted>");
        }
        redact_labeled_values(redact_prefixed_tokens(redacted))
    }
}

fn redact_prefixed_tokens(mut text: String) -> String {
    const PREFIXES: [&str; 13] = [
        "github_pat_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "ghp_",
        "loom_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxs-",
        "sk-ant-",
        "sk-proj-",
    ];
    for prefix in PREFIXES {
        let mut from = 0;
        while let Some(relative) = text[from..].find(prefix) {
            let start = from + relative;
            let token_end = start
                + text[start..]
                    .find(|character: char| {
                        !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-' | '.')
                    })
                    .unwrap_or(text.len() - start);
            text.replace_range(start..token_end, "<redacted-token>");
            from = start + "<redacted-token>".len();
        }
    }
    text
}

fn redact_labeled_values(mut text: String) -> String {
    const LABELS: [&str; 13] = [
        "authorization",
        "access_token",
        "refresh_token",
        "client_secret",
        "webhook_secret",
        "private_key",
        "api_key",
        "password",
        "passwd",
        "credential",
        "token",
        "secret",
        "cookie",
    ];
    for label in LABELS {
        let mut from = 0;
        loop {
            let lower = text[from..].to_ascii_lowercase();
            let Some(relative) = lower.find(label) else {
                break;
            };
            let start = from + relative;
            let before = text[..start].chars().next_back();
            let after = text[start + label.len()..].chars().next();
            if before.is_some_and(|character| character.is_ascii_alphanumeric())
                || after.is_some_and(|character| character.is_ascii_alphanumeric())
            {
                from = start + label.len();
                continue;
            }

            let mut cursor = start + label.len();
            if text[cursor..].starts_with(['\'', '"']) {
                cursor += 1;
            }
            cursor += text[cursor..]
                .find(|character: char| !character.is_ascii_whitespace())
                .unwrap_or(text.len() - cursor);
            if !text[cursor..].starts_with(['=', ':']) {
                from = start + label.len();
                continue;
            }
            cursor += 1;
            cursor += text[cursor..]
                .find(|character: char| !character.is_ascii_whitespace())
                .unwrap_or(text.len() - cursor);
            if cursor == text.len() {
                break;
            }

            let quote = text[cursor..]
                .chars()
                .next()
                .filter(|character| matches!(character, '\'' | '"'));
            let value_start = cursor + quote.map_or(0, char::len_utf8);
            let value_end = if let Some(quote) = quote {
                text[value_start..]
                    .find(quote)
                    .map_or(text.len(), |relative| value_start + relative)
            } else if text[value_start..]
                .to_ascii_lowercase()
                .starts_with("bearer ")
            {
                let token_start = value_start + "bearer ".len();
                token_start
                    + text[token_start..]
                        .find(is_unquoted_value_delimiter)
                        .unwrap_or(text.len() - token_start)
            } else {
                value_start
                    + text[value_start..]
                        .find(is_unquoted_value_delimiter)
                        .unwrap_or(text.len() - value_start)
            };
            text.replace_range(value_start..value_end, "<redacted>");
            from = value_start + "<redacted>".len();
        }
    }
    text
}

fn is_unquoted_value_delimiter(character: char) -> bool {
    character.is_ascii_whitespace() || matches!(character, ',' | ';' | '&' | '}' | ']')
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

    #[test]
    fn user_log_redaction_masks_known_and_structural_secrets() {
        let redactor = LogRedactor::new(["arbitrary-deployment-credential".to_string()]);
        let line = LogLine {
            seq: 7,
            ts: "t".into(),
            level: "WARN".into(),
            target: "test".into(),
            message: concat!(
                "known=arbitrary-deployment-credential ",
                "github=github_pat_abc123 slack=xoxb-secret-value ",
                "authorization=Bearer opaque-bearer ",
                "body={\"access_token\":\"opaque-json-token\"} ",
                "MY_SECRET=new-secret-after-stream-open"
            )
            .into(),
        };

        let redacted = redactor.redact(line);
        for secret in [
            "arbitrary-deployment-credential",
            "github_pat_abc123",
            "xoxb-secret-value",
            "opaque-bearer",
            "opaque-json-token",
            "new-secret-after-stream-open",
        ] {
            assert!(!redacted.message.contains(secret), "leaked {secret}");
        }
        assert_eq!(redacted.seq, 7);
        assert_eq!(redacted.target, "test");
    }

    #[test]
    fn user_log_redaction_preserves_benign_diagnostics() {
        let message = "secret rotation succeeded method=GET path=/api/status";
        assert_eq!(LogRedactor::new([]).redact_message(message), message);
    }
}
