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

use weaver_core::branch::Branch;
use weaver_core::transcript;

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

/// Whether `agent_kind` produces a conversation transcript worth looking for: a
/// builtin (claude/codex) or the concierge (which runs one of them). A custom
/// agent or bare shell has none, so its missing transcript is expected, not a
/// problem to warn about.
fn produces_transcript(agent_kind: &str) -> bool {
    crate::agent::builtin_agent_type(agent_kind).is_some()
        || agent_kind == crate::agent::CONCIERGE_KIND
}

/// Whether `agent_kind` could have a Codex transcript — the codex runtime itself
/// or the concierge (which may run codex). Only these are worth the `~/.codex`
/// directory walk; every other agent skips it.
fn maybe_codex(agent_kind: &str) -> bool {
    agent_kind == "codex" || agent_kind == crate::agent::CONCIERGE_KIND
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
/// dashboard viewer. Prefers the live transcript (always fresh); falls back to
/// the captured `chat.json` for an archived session whose transcript files have
/// since been cleaned up. `None` when neither is available.
pub async fn conversation(db: &Db, session: &Session, branch: &Branch) -> Option<transcript::Log> {
    let files = locate(session);
    if let Some(log) = transcript::parse_files(&files) {
        return Some(log);
    }
    let path = captured_json_path(db, branch).await?;
    let raw = tokio::fs::read_to_string(&path).await.ok()?;
    serde_json::from_str(&raw).ok()
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
