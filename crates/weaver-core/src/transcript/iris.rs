//! The **iris log format**: one normalized, agent-agnostic representation of a
//! coding-agent conversation, plus its markdown renderer.
//!
//! Every agent (Claude Code, Codex, …) records its conversation in its own raw
//! format. The per-agent converters ([`super::claude`], [`super::codex`]) flatten
//! those into this single [`Log`] — a list of [`Message`]s, each a role and an
//! ordered list of [`Block`]s. Everything downstream (the markdown renderer
//! here, anything that wants to read a captured log) speaks only iris, so it
//! never has to know which agent produced it.
//!
//! [`Log`] is `serde`-serializable, so a captured conversation persists as iris
//! JSON and re-renders later without re-parsing the agent's raw transcript.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A whole conversation, normalized. `source` names the agent it came from
/// (`"claude"`, `"codex"`); the rest is optional context the renderer banners.
#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
pub struct Log {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub messages: Vec<Message>,
}

/// Who a [`Message`] is from. `Context` is injected, non-conversational material
/// (a session primer, system/permissions instructions) — kept for completeness
/// but rendered out of the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Context,
}

/// One message: who said it, when, and its ordered content blocks.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub blocks: Vec<Block>,
}

impl Message {
    pub fn new(role: Role, timestamp: Option<String>, blocks: Vec<Block>) -> Self {
        Self {
            role,
            timestamp,
            blocks,
        }
    }
}

/// A unit of message content. Tool input is kept as raw JSON so the renderer
/// owns all formatting decisions (a shell command fenced as `sh`, a patch as
/// text, anything else as pretty JSON).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Text { text: String },
    Thinking { text: String },
    ToolUse { name: String, input: Value },
    ToolResult { output: String, is_error: bool },
    Image,
}

impl Block {
    pub fn text(s: impl Into<String>) -> Self {
        Block::Text { text: s.into() }
    }
    pub fn thinking(s: impl Into<String>) -> Self {
        Block::Thinking { text: s.into() }
    }
    pub fn tool_result(output: impl Into<String>, is_error: bool) -> Self {
        Block::ToolResult {
            output: output.into(),
            is_error,
        }
    }
}

impl Log {
    /// Serialize to pretty iris JSON — the persisted capture format.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Render this conversation as a markdown-like log (see module-level docs of
    /// [`super`]).
    pub fn render_markdown(&self) -> String {
        render_markdown(self)
    }
}

/// A tool result longer than this many characters is truncated in the rendered
/// log, with a marker noting how much was dropped — one noisy command (a big
/// `cat`, a test run) shouldn't drown the conversation.
const MAX_RESULT_CHARS: usize = 3000;

/// Render an iris [`Log`] as a markdown-like conversation: a metadata banner,
/// then each turn under a role heading — user prompts, assistant replies,
/// collapsible thinking, and each tool call with its (truncated) result.
/// Consecutive assistant messages share one heading so an agent that emits each
/// reply, reasoning, and tool call as a separate record still reads as one turn.
pub fn render_markdown(log: &Log) -> String {
    let mut out = String::new();
    out.push_str("# Conversation log\n\n");
    out.push_str(&banner(log));
    out.push_str("\n---\n");

    let mut prev_role: Option<Role> = None;
    for msg in &log.messages {
        let time = msg.timestamp.as_deref().map(short_time).unwrap_or_default();
        match msg.role {
            Role::Context => render_context(&mut out, msg),
            Role::User => {
                push_heading(&mut out, "🧑 User", &time);
                render_blocks(&mut out, &msg.blocks);
            }
            Role::Assistant => {
                // One heading per contiguous assistant run.
                if prev_role != Some(Role::Assistant) {
                    push_heading(&mut out, "🤖 Assistant", &time);
                }
                render_blocks(&mut out, &msg.blocks);
            }
        }
        prev_role = Some(msg.role);
    }
    out
}

/// The one-line metadata banner under the title: source agent, message count,
/// model, and the time span covered.
fn banner(log: &Log) -> String {
    let mut parts = Vec::new();
    if !log.source.is_empty() {
        parts.push(log.source.clone());
    }
    parts.push(format!("{} messages", log.messages.len()));
    if let Some(m) = &log.model {
        parts.push(m.clone());
    }
    let times: Vec<&str> = log
        .messages
        .iter()
        .filter_map(|m| m.timestamp.as_deref())
        .collect();
    if let (Some(first), Some(last)) = (times.first(), times.last()) {
        parts.push(format!("{first} – {last}"));
    }
    format!("_{}_\n", parts.join(" · "))
}

fn push_heading(out: &mut String, label: &str, time: &str) {
    if time.is_empty() {
        out.push_str(&format!("\n## {label}\n\n"));
    } else {
        out.push_str(&format!("\n## {label} · {time}\n\n"));
    }
}

/// Injected context (a primer, system instructions) — collapsed out of the way
/// so it's available but doesn't dominate the log.
fn render_context(out: &mut String, msg: &Message) {
    let text: String = msg
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } | Block::Thinking { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    out.push_str("\n<details><summary>📎 Context</summary>\n\n");
    out.push_str(truncate(text, MAX_RESULT_CHARS).trim_end());
    out.push_str("\n\n</details>\n\n");
}

fn render_blocks(out: &mut String, blocks: &[Block]) {
    for block in blocks {
        match block {
            Block::Text { text } => {
                let t = text.trim();
                if !t.is_empty() {
                    out.push_str(t);
                    out.push_str("\n\n");
                }
            }
            Block::Thinking { text } => {
                let t = text.trim();
                if !t.is_empty() {
                    out.push_str("<details><summary>💭 Thinking</summary>\n\n");
                    out.push_str(t);
                    out.push_str("\n\n</details>\n\n");
                }
            }
            Block::ToolUse { name, input } => render_tool_use(out, name, input),
            Block::ToolResult { output, is_error } => render_tool_result(out, output, *is_error),
            Block::Image => out.push_str("_[image]_\n\n"),
        }
    }
}

/// `🔧 **Tool**` with its input fenced: a shell command as `sh`, a bare string
/// (e.g. a patch) as a plain fence, anything structured as pretty JSON.
fn render_tool_use(out: &mut String, name: &str, input: &Value) {
    out.push_str(&format!("🔧 **{name}**\n\n"));
    let command = input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(Value::as_str);
    if let Some(cmd) = command {
        fence(out, "sh", cmd);
    } else if let Some(s) = input.as_str() {
        if !s.trim().is_empty() {
            fence(out, "", s);
        }
    } else if let Some(obj) = input.as_object() {
        if !obj.is_empty() {
            let pretty = serde_json::to_string_pretty(obj).unwrap_or_default();
            fence(out, "json", &pretty);
        }
    }
}

/// A tool's output, collapsed and truncated; errors flagged in the summary.
fn render_tool_result(out: &mut String, output: &str, is_error: bool) {
    let text = output.trim();
    if text.is_empty() {
        return;
    }
    let label = if is_error {
        "↳ result (error)"
    } else {
        "↳ result"
    };
    out.push_str(&format!("<details><summary>{label}</summary>\n\n```\n"));
    out.push_str(truncate(text, MAX_RESULT_CHARS).trim_end());
    out.push_str("\n```\n\n</details>\n\n");
}

/// Append a fenced code block, truncated to [`MAX_RESULT_CHARS`].
fn fence(out: &mut String, lang: &str, body: &str) {
    out.push_str("```");
    out.push_str(lang);
    out.push('\n');
    out.push_str(truncate(body, MAX_RESULT_CHARS).trim_end());
    out.push_str("\n```\n\n");
}

/// The `HH:MM:SS` of an ISO-8601 timestamp (`2026-06-17T18:30:00.000Z`), or the
/// whole string when it isn't in that format.
fn short_time(ts: &str) -> String {
    let bytes = ts.as_bytes();
    if bytes.len() >= 19 && bytes[10] == b'T' {
        ts[11..19].to_string()
    } else {
        ts.to_string()
    }
}

/// Truncate to `max` characters on a char boundary, appending a marker that says
/// how many characters were dropped.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    let dropped = s.chars().count() - max;
    format!("{kept}\n… (truncated, {dropped} more characters)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn log_with(messages: Vec<Message>) -> Log {
        Log {
            source: "claude".into(),
            session_id: Some("s1".into()),
            model: Some("claude-opus-4-8".into()),
            cwd: None,
            messages,
        }
    }

    #[test]
    fn groups_consecutive_assistant_messages_under_one_heading() {
        let log = log_with(vec![
            Message::new(
                Role::User,
                Some("2026-06-17T10:00:00.000Z".into()),
                vec![Block::text("Fix it")],
            ),
            Message::new(
                Role::Assistant,
                Some("2026-06-17T10:00:01.000Z".into()),
                vec![
                    Block::thinking("hmm"),
                    Block::text("On it."),
                    Block::ToolUse {
                        name: "Bash".into(),
                        input: json!({"command": "ls -la"}),
                    },
                ],
            ),
            // The tool result arrives as its own assistant-role message; it must
            // NOT start a second "Assistant" heading.
            Message::new(
                Role::Assistant,
                Some("2026-06-17T10:00:02.000Z".into()),
                vec![Block::tool_result("total 0", false)],
            ),
        ]);
        let md = render_markdown(&log);
        assert!(md.contains("# Conversation log"));
        assert!(md.contains("claude · 3 messages · claude-opus-4-8"));
        assert_eq!(
            md.matches("## 🤖 Assistant").count(),
            1,
            "one heading: {md}"
        );
        assert!(md.contains("## 🧑 User · 10:00:00"));
        assert!(md.contains("<details><summary>💭 Thinking</summary>"));
        assert!(md.contains("🔧 **Bash**"));
        assert!(md.contains("```sh\nls -la\n```"));
        assert!(md.contains("<details><summary>↳ result</summary>"));
        assert!(md.contains("total 0"));
    }

    #[test]
    fn renders_string_input_and_error_result_and_context() {
        let log = log_with(vec![
            Message::new(Role::Context, None, vec![Block::text("primer")]),
            Message::new(
                Role::Assistant,
                None,
                vec![
                    Block::ToolUse {
                        name: "apply_patch".into(),
                        input: json!("*** Begin Patch"),
                    },
                    Block::tool_result("boom", true),
                ],
            ),
        ]);
        let md = render_markdown(&log);
        assert!(md.contains("<details><summary>📎 Context</summary>"));
        assert!(md.contains("🔧 **apply_patch**"));
        assert!(md.contains("*** Begin Patch"));
        assert!(md.contains("↳ result (error)"));
    }

    #[test]
    fn iris_log_round_trips_through_json() {
        let log = log_with(vec![Message::new(
            Role::User,
            None,
            vec![Block::text("hi")],
        )]);
        let json = log.to_json();
        let back: Log = serde_json::from_str(&json).unwrap();
        assert_eq!(back.messages.len(), 1);
        assert_eq!(back.source, "claude");
        assert!(matches!(back.messages[0].role, Role::User));
    }
}
