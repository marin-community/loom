//! Capturing and serving a session's agent conversation.
//!
//! Archiving tears down a session's worktree, but its agent's conversation
//! transcript lives outside the worktree (`~/.claude/projects/…`,
//! `~/.codex/sessions/…`) and survives. At archive time [`capture`] locates that
//! transcript, normalizes it to the [iris format](weaver_core::transcript::iris),
//! and writes two files under `<log_dir>/<branch>/`:
//!
//! * `chat.json` — the normalized iris log (machine-readable, re-renderable).
//! * `chat.md` — a readable markdown render for review.
//!
//! [`conversation`] loads the same iris log for the dashboard's Conversation
//! view, working for both a live session (parse the transcript fresh) and an
//! archived one (the live transcript usually still exists; the captured
//! `chat.json` is the durable fallback if it's been cleaned up).
//!
//! `log_dir` is the `session.log_dir` setting, defaulting to
//! `~/.iris/logs/sessions`. Capture is best-effort: a failure returns a warning
//! and never blocks the archive.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use weaver_core::branch::Branch;
use weaver_core::transcript;
use weaver_core::transcript::iris::{Block, Log, Message, Role};

use crate::chat::kind;
use crate::db::Db;
use crate::session::Session;

/// Resolve the directory conversation logs are captured under: the
/// `session.log_dir` setting, or `~/.iris/logs/sessions` when unset. `None` only
/// when the default is needed but `$HOME` is unset.
pub async fn log_dir(db: &Db) -> Option<PathBuf> {
    let configured = weaver_core::config::get(db, "session.log_dir")
        .await
        .filter(|s| !s.trim().is_empty());
    match configured {
        Some(dir) => Some(PathBuf::from(dir.trim())),
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".iris").join("logs").join("sessions")),
    }
}

/// A filesystem-safe single-component name for a branch: `weaver/fix-thing` →
/// `weaver-fix-thing`. Mirrors the session-socket sanitizer — alphanumerics and
/// `-`/`_`/`.` survive, everything else becomes `-`.
fn branch_slug(branch: &Branch) -> String {
    let s: String = branch
        .branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.is_empty() {
        branch.id.clone()
    } else {
        s
    }
}

/// Whether `agent_kind` produces a conversation transcript worth looking for.
/// A custom agent or bare shell has none, so its missing transcript is expected,
/// not a problem to warn about.
fn produces_transcript(agent_kind: &str) -> bool {
    crate::agent::builtin_agent_type(agent_kind).is_some()
}

/// Whether `agent_kind` could have a Codex transcript. Only Codex is worth the
/// `~/.codex` directory walk; every other agent skips it.
fn maybe_codex(agent_kind: &str) -> bool {
    agent_kind == "codex"
}

/// Locate a session's live agent transcript files (oldest first). Claude's are a
/// cheap path lookup, so always try them; Codex needs a `~/.codex` directory
/// walk, so only fall back to it for an agent that could be Codex ([`maybe_codex`]).
/// Empty when none are found.
fn locate(session: &Session) -> Vec<PathBuf> {
    let work_dir = PathBuf::from(&session.work_dir);
    let files = transcript::claude_transcripts_for(&work_dir);
    if !files.is_empty() {
        return files;
    }
    if !maybe_codex(&session.agent_kind) {
        return Vec::new();
    }
    transcript::codex_transcripts_for(&work_dir)
}

/// The captured iris-JSON path for a session's branch (`<log_dir>/<branch>/
/// chat.json`), whether or not it exists yet. `None` only when the log dir can't
/// be resolved.
pub async fn captured_json_path(db: &Db, branch: &Branch) -> Option<PathBuf> {
    Some(
        log_dir(db)
            .await?
            .join(branch_slug(branch))
            .join("chat.json"),
    )
}

/// The session's conversation as an iris [`Log`](transcript::Log), for the
/// dashboard viewer. For an ACP session the source of truth is loom's own chat
/// journal, mapped to iris ([`journal_to_log`]) — served live so the existing
/// Conversation tab keeps working before the SPA rewires to `/chat`. For a
/// terminal session, prefer the live transcript (always fresh), falling back to
/// the captured `chat.json` for an archived session whose transcript files have
/// since been cleaned up. `None` when neither is available.
pub async fn conversation(db: &Db, session: &Session, branch: &Branch) -> Option<transcript::Log> {
    if session.protocol == "acp" {
        if let Some(log) = journal_to_log(db, session).await {
            return Some(log);
        }
        // An archived ACP session whose journal is gone falls back to the captured
        // iris JSON, same as a terminal session.
        let path = captured_json_path(db, branch).await?;
        let raw = tokio::fs::read_to_string(&path).await.ok()?;
        return serde_json::from_str(&raw).ok();
    }
    let files = locate(session);
    if let Some(log) = transcript::parse_files(&files) {
        return Some(log);
    }
    let path = captured_json_path(db, branch).await?;
    let raw = tokio::fs::read_to_string(&path).await.ok()?;
    serde_json::from_str(&raw).ok()
}

/// Map an ACP session's chat journal ([`crate::chat`]) to an iris [`Log`]: the
/// same block model, flattened into the agent-agnostic transcript the archive
/// export and the Conversation viewer speak. `user_message` → User/Text,
/// `agent_message` → Assistant/Text, `thought` → Assistant/Thinking, `tool_call`
/// → Assistant ToolUse+ToolResult; the ACP-only kinds (`plan`, `permission_
/// request`, `mode_change`, `usage`, `handoff`) flatten to Context notes; `turn_end` is a
/// boundary marker with no conversational content, so it is dropped. `None` when
/// the journal is empty.
pub async fn journal_to_log(db: &Db, session: &Session) -> Option<Log> {
    let blocks = crate::chat::list(db, &session.id).await.ok()?;
    if blocks.is_empty() {
        return None;
    }
    let mut messages: Vec<Message> = Vec::with_capacity(blocks.len());
    for b in &blocks {
        let ts = Some(b.created_at.clone());
        let p = &b.payload;
        let text = |key: &str| {
            p.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match b.kind.as_str() {
            kind::USER_MESSAGE => messages.push(Message::new(
                Role::User,
                ts,
                vec![Block::text(text("text"))],
            )),
            kind::AGENT_MESSAGE => messages.push(Message::new(
                Role::Assistant,
                ts,
                vec![Block::text(text("text"))],
            )),
            kind::THOUGHT => messages.push(Message::new(
                Role::Assistant,
                ts,
                vec![Block::thinking(text("text"))],
            )),
            kind::TOOL_CALL => {
                let name = match p.get("title").and_then(Value::as_str) {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => p
                        .get("tool_kind")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                };
                let input = json!({
                    "tool_kind": p.get("tool_kind").cloned().unwrap_or(Value::Null),
                    "content": p.get("content").cloned().unwrap_or(Value::Null),
                    "locations": p.get("locations").cloned().unwrap_or(Value::Null),
                });
                let is_error = p.get("status").and_then(Value::as_str) == Some("failed");
                let output = tool_content_text(p.get("content"));
                messages.push(Message::new(
                    Role::Assistant,
                    ts,
                    vec![
                        Block::ToolUse { name, input },
                        Block::tool_result(output, is_error),
                    ],
                ));
            }
            kind::PLAN
            | kind::PERMISSION_REQUEST
            | kind::MODE_CHANGE
            | kind::USAGE
            | kind::HANDOFF => {
                messages.push(Message::new(
                    Role::Context,
                    ts,
                    vec![Block::text(context_note(&b.kind, p))],
                ));
            }
            // turn_end: a boundary, not content.
            _ => {}
        }
    }
    Some(Log {
        source: "acp".to_string(),
        session_id: session.acp_session_id.clone(),
        model: None,
        cwd: Some(session.work_dir.clone()),
        messages,
    })
}

/// Flatten a `tool_call` block's `content` array into readable result text: text
/// parts verbatim, diff parts as a compact `path` + `- old` / `+ new` snippet.
fn tool_content_text(content: Option<&Value>) -> String {
    let Some(arr) = content.and_then(Value::as_array) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for c in arr {
        match c.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = c.get("text").and_then(Value::as_str) {
                    parts.push(t.to_string());
                }
            }
            Some("diff") => {
                let path = c.get("path").and_then(Value::as_str).unwrap_or("");
                let old = c.get("old").and_then(Value::as_str).unwrap_or("");
                let new = c.get("new").and_then(Value::as_str).unwrap_or("");
                parts.push(format!("{path}\n- {old}\n+ {new}"));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// A one-line context note for an ACP-only block kind, flattened into the iris
/// export as injected context.
fn context_note(kind: &str, p: &Value) -> String {
    match kind {
        kind::PLAN => {
            let entries = p.get("entries").and_then(Value::as_array);
            let lines: Vec<String> = entries
                .into_iter()
                .flatten()
                .map(|e| {
                    let status = e.get("status").and_then(Value::as_str).unwrap_or("");
                    let content = e.get("content").and_then(Value::as_str).unwrap_or("");
                    format!("- [{status}] {content}")
                })
                .collect();
            format!("plan:\n{}", lines.join("\n"))
        }
        kind::PERMISSION_REQUEST => {
            let title = p.get("title").and_then(Value::as_str).unwrap_or("");
            let outcome = p
                .get("outcome")
                .and_then(|o| o.get("option_id"))
                .and_then(Value::as_str)
                .unwrap_or("pending");
            format!("permission: {title} ({outcome})")
        }
        kind::MODE_CHANGE => {
            let mode = p.get("mode_id").and_then(Value::as_str).unwrap_or("");
            format!("mode changed to {mode}")
        }
        kind::USAGE => {
            let used = p.get("used").and_then(Value::as_i64).unwrap_or(0);
            let size = p.get("size").and_then(Value::as_i64).unwrap_or(0);
            format!("context usage: {used}/{size}")
        }
        kind::HANDOFF => {
            let from = p.get("from").and_then(Value::as_str).unwrap_or("agent");
            let to = p.get("to").and_then(Value::as_str).unwrap_or("agent");
            format!("agent handoff: {from} -> {to}")
        }
        _ => kind.to_string(),
    }
}

/// Capture `session`'s conversation log to `<log_dir>/<branch>/`. Returns any
/// warnings (no transcript found, write failure) — never an error, so a capture
/// problem can't abort the archive. Returns the written markdown path on success
/// for logging.
pub async fn capture(
    db: &Db,
    session: &Session,
    branch: &Branch,
) -> (Option<PathBuf>, Vec<String>) {
    // An ACP session's transcript is loom's own chat journal, not an agent JSONL:
    // map it to iris and write the same `chat.json`/`chat.md` pair.
    if session.protocol == "acp" {
        return capture_acp(db, session, branch).await;
    }
    let mut warnings = Vec::new();
    let files = locate(session);
    if files.is_empty() {
        // Missing transcript for an agent that produces one is worth a warning;
        // for a custom agent or bare shell it's expected, so stay quiet.
        if produces_transcript(&session.agent_kind) {
            warnings.push(format!(
                "no agent transcript found for {}",
                session.work_dir
            ));
        }
        return (None, warnings);
    }
    let Some(log) = transcript::parse_files(&files) else {
        warnings.push(format!(
            "found {} transcript file(s) but none parsed",
            files.len()
        ));
        return (None, warnings);
    };

    let Some(dir) = log_dir(db).await else {
        warnings.push("cannot resolve session log dir (HOME unset)".to_string());
        return (None, warnings);
    };
    let dest = dir.join(branch_slug(branch));
    match write_log(&dest, &log).await {
        Ok(md_path) => {
            tracing::info!(
                branch = %branch.branch, source = %log.source,
                messages = log.messages.len(), path = %md_path.display(),
                "captured conversation log"
            );
            (Some(md_path), warnings)
        }
        Err(e) => {
            warnings.push(format!("writing session log to {}: {e}", dest.display()));
            (None, warnings)
        }
    }
}

/// Capture an ACP session's conversation from its chat journal ([`journal_to_log`]).
/// Best-effort like [`capture`]: an empty journal is a quiet no-op (nothing to
/// warn about), a write failure a warning.
async fn capture_acp(
    db: &Db,
    session: &Session,
    branch: &Branch,
) -> (Option<PathBuf>, Vec<String>) {
    let mut warnings = Vec::new();
    let Some(log) = journal_to_log(db, session).await else {
        // No journal yet (an ACP session archived before it took a turn) — nothing
        // to capture, and nothing worth warning about.
        return (None, warnings);
    };
    let Some(dir) = log_dir(db).await else {
        warnings.push("cannot resolve session log dir (HOME unset)".to_string());
        return (None, warnings);
    };
    let dest = dir.join(branch_slug(branch));
    match write_log(&dest, &log).await {
        Ok(md_path) => {
            tracing::info!(
                branch = %branch.branch, source = %log.source,
                messages = log.messages.len(), path = %md_path.display(),
                "captured acp conversation journal"
            );
            (Some(md_path), warnings)
        }
        Err(e) => {
            warnings.push(format!("writing session log to {}: {e}", dest.display()));
            (None, warnings)
        }
    }
}

/// Write the iris JSON and rendered markdown into `dest`, returning the markdown
/// path.
async fn write_log(dest: &Path, log: &transcript::Log) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(dest).await?;
    tokio::fs::write(dest.join("chat.json"), log.to_json()).await?;
    let md_path = dest.join("chat.md");
    tokio::fs::write(&md_path, log.render_markdown()).await?;
    Ok(md_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str) -> Branch {
        Branch {
            id: "abc12345".to_string(),
            repo_root: "/repo".to_string(),
            branch: name.to_string(),
            base_branch: "main".to_string(),
            goal: String::new(),
            title: String::new(),
            description: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn branch_slug_is_a_safe_single_component() {
        assert_eq!(branch_slug(&branch("weaver/fix-thing")), "weaver-fix-thing");
        assert_eq!(branch_slug(&branch("feature/a_b.c")), "feature-a_b.c");
    }

    #[tokio::test]
    async fn log_dir_honors_the_setting_and_defaults_under_iris() {
        let db = crate::db::connect_in_memory().await.unwrap();
        // Unset → default under $HOME/.iris.
        let def = log_dir(&db).await.unwrap();
        assert!(def.ends_with("logs/sessions"), "default: {}", def.display());
        assert!(def.to_string_lossy().contains(".iris"));

        // Set → used verbatim.
        weaver_core::config::apply(&db, &[("session.log_dir".into(), Some("/tmp/logs".into()))])
            .await
            .unwrap();
        assert_eq!(log_dir(&db).await.unwrap(), PathBuf::from("/tmp/logs"));
    }

    #[tokio::test]
    async fn write_log_produces_both_files() {
        let tmp = std::env::temp_dir().join(format!("chatlog-test-{}", std::process::id()));
        let log = transcript::Log {
            source: "claude".into(),
            session_id: Some("s1".into()),
            model: None,
            cwd: None,
            messages: vec![transcript::Message::new(
                transcript::Role::User,
                None,
                vec![transcript::Block::text("hi")],
            )],
        };
        let md = write_log(&tmp, &log).await.unwrap();
        assert!(md.ends_with("chat.md"));
        let md_body = tokio::fs::read_to_string(&md).await.unwrap();
        assert!(md_body.contains("# Conversation log"));
        let json = tokio::fs::read_to_string(tmp.join("chat.json"))
            .await
            .unwrap();
        assert!(json.contains("\"source\": \"claude\""));
        tokio::fs::remove_dir_all(&tmp).await.ok();
    }
}
