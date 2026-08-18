//! Reading a coding agent's conversation transcript and rendering it for review.
//!
//! Two parts, deliberately separated:
//!
//! 1. **raw → iris** — per-agent converters ([`claude::to_iris`],
//!    [`codex::to_iris`]) flatten an agent's own transcript shape into the one
//!    normalized [`iris`] format. [`parse`] sniffs which agent produced a log and
//!    dispatches.
//! 2. **iris → markdown** — [`iris::render_markdown`] turns that normalized log
//!    into a readable, markdown-like conversation, agent-agnostic.
//!
//! Plus the filesystem glue to *find* a worktree's transcript: Claude Code keys
//! its `~/.claude/projects/<munged-cwd>/` dir off the working directory, so a
//! worktree maps to its transcripts deterministically ([`claude_transcripts_for`])
//! even after it's archived; Codex stores `~/.codex/sessions/.../rollout-*.jsonl`
//! and records the cwd inside, so we match on that ([`codex_transcripts_for`]).
//! This module is pure model + filesystem reads — no process spawning. loom calls
//! it at archive time to capture the log; the `loom sessions transcript` CLI renders on
//! demand.

pub mod claude;
pub mod codex;
pub mod iris;

use std::path::{Path, PathBuf};

pub use iris::{Block, Log, Message, Role};

/// Which agent produced a raw transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Claude,
    Codex,
}

/// Sniff which agent produced a raw JSONL transcript. Codex records are tagged
/// `{type: session_meta|response_item|event_msg|turn_context, payload}`; Claude
/// records are `{type: user|assistant, message}`. Returns `None` when neither
/// shape is recognised in the first handful of lines.
pub fn detect(jsonl: &str) -> Option<Source> {
    for line in jsonl
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(40)
    {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if v.get("payload").is_some()
            && matches!(
                t,
                "session_meta" | "response_item" | "event_msg" | "turn_context"
            )
        {
            return Some(Source::Codex);
        }
        if matches!(t, "user" | "assistant") && v.get("message").is_some() {
            return Some(Source::Claude);
        }
    }
    None
}

/// Parse a raw transcript into the iris format, auto-detecting the agent. Returns
/// `None` when the format isn't recognised.
pub fn parse(jsonl: &str) -> Option<Log> {
    match detect(jsonl)? {
        Source::Claude => Some(claude::to_iris(jsonl)),
        Source::Codex => Some(codex::to_iris(jsonl)),
    }
}

/// Read a set of transcript files (in order), concatenate, and parse to iris.
/// Unreadable files are skipped. Returns `None` when nothing parses.
pub fn parse_files(paths: &[PathBuf]) -> Option<Log> {
    let mut jsonl = String::new();
    for p in paths {
        if let Ok(s) = std::fs::read_to_string(p) {
            jsonl.push_str(&s);
            if !s.ends_with('\n') {
                jsonl.push('\n');
            }
        }
    }
    if jsonl.trim().is_empty() {
        return None;
    }
    parse(&jsonl)
}

/// Read and render a worktree's transcript to markdown — locating it across both
/// agents ([`locate`]). `None` when no transcript is found or it can't be parsed.
pub fn render_worktree(work_dir: &Path) -> Option<String> {
    let (_, files) = locate(work_dir)?;
    parse_files(&files).map(|log| log.render_markdown())
}

/// Find a worktree's transcript files, trying Claude first (its layout is an
/// exact path lookup) then Codex (a bounded scan). Returns the source and the
/// files oldest-first, or `None` when neither agent has a transcript for it.
pub fn locate(work_dir: &Path) -> Option<(Source, Vec<PathBuf>)> {
    let claude = claude_transcripts_for(work_dir);
    if !claude.is_empty() {
        return Some((Source::Claude, claude));
    }
    let codex = codex_transcripts_for(work_dir);
    if !codex.is_empty() {
        return Some((Source::Codex, codex));
    }
    None
}

// --- Claude Code transcript location -----------------------------------------

/// `~/.claude/projects` for the current user, honouring `$HOME`.
pub fn claude_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("projects"))
}

/// Claude Code's project-directory name for a working directory: every `/` and
/// `.` becomes `-`. e.g. `/home/a/code/x/.worktrees/y` →
/// `-home-a-code-x--worktrees-y`. Mirrors Claude Code's own munging.
pub fn claude_project_dir_name(work_dir: &Path) -> String {
    work_dir
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// Every Claude transcript (`*.jsonl`) for a working directory, oldest first by
/// mtime. A worktree accumulates one file per Claude session, so a resumed
/// (`--continue`) session leaves several; ordering them reconstructs the whole
/// conversation. Empty when the project dir is absent.
pub fn claude_transcripts_for(work_dir: &Path) -> Vec<PathBuf> {
    let Some(projects) = claude_projects_dir() else {
        return Vec::new();
    };
    jsonl_files_sorted(&projects.join(claude_project_dir_name(work_dir)))
}

// --- Codex rollout transcript location ---------------------------------------

/// `~/.codex/sessions` for the current user, honouring `$HOME`.
pub fn codex_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
}

/// Every Codex rollout transcript whose recorded cwd is `work_dir`, oldest first
/// by mtime. Codex doesn't key its session files by path, so this walks the
/// `~/.codex/sessions/YYYY/MM/DD/` tree and matches each file's `session_meta`
/// cwd. Best-effort and bounded to the `.jsonl` rollouts; empty when none match.
pub fn codex_transcripts_for(work_dir: &Path) -> Vec<PathBuf> {
    let Some(root) = codex_sessions_dir() else {
        return Vec::new();
    };
    let want = work_dir.to_string_lossy();
    let mut matched: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for path in walk_jsonl(&root) {
        if rollout_cwd(&path).as_deref() == Some(want.as_ref()) {
            let mtime = mtime_of(&path);
            matched.push((mtime, path));
        }
    }
    matched.sort_by_key(|(mtime, _)| *mtime);
    matched.into_iter().map(|(_, p)| p).collect()
}

/// The cwd a Codex rollout was recorded in, read from its `session_meta` line
/// (the first line). `None` when the file is unreadable or has no cwd.
fn rollout_cwd(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    // The cwd lives in session_meta, which is the first record; scan a few lines
    // in case anything precedes it.
    for line in BufReader::new(file).lines().take(5).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(cwd) = v
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .and_then(|c| c.as_str())
        {
            return Some(cwd.to_string());
        }
    }
    None
}

// --- shared filesystem helpers -----------------------------------------------

fn mtime_of(path: &Path) -> std::time::SystemTime {
    path.metadata()
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// `*.jsonl` files directly in `dir`, oldest first by mtime. Empty when absent.
fn jsonl_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .map(|p| (mtime_of(&p), p))
        .collect();
    files.sort_by_key(|(mtime, _)| *mtime);
    files.into_iter().map(|(_, p)| p).collect()
}

/// All `*.jsonl` files anywhere under `root` (a shallow recursive walk).
fn walk_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "jsonl") {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_claude_vs_codex() {
        let claude =
            json!({"type": "user", "message": {"role": "user", "content": "hi"}}).to_string();
        let codex = json!({"type": "session_meta", "payload": {"id": "x"}}).to_string();
        assert_eq!(detect(&claude), Some(Source::Claude));
        assert_eq!(detect(&codex), Some(Source::Codex));
        assert_eq!(detect("not json\n{}"), None);
    }

    #[test]
    fn parse_dispatches_by_detected_source() {
        let claude =
            json!({"type": "user", "message": {"role": "user", "content": "hi"}}).to_string();
        let log = parse(&claude).unwrap();
        assert_eq!(log.source, "claude");

        let codex = [
            json!({"type": "session_meta", "payload": {"id": "x", "cwd": "/w"}}).to_string(),
            json!({"type": "event_msg", "payload": {"type": "user_message", "message": "hey"}})
                .to_string(),
        ]
        .join("\n");
        let log = parse(&codex).unwrap();
        assert_eq!(log.source, "codex");
        assert_eq!(log.messages.len(), 1);
    }

    #[test]
    fn munges_cwd_into_claude_project_dir_name() {
        assert_eq!(
            claude_project_dir_name(Path::new("/home/a/code/x/.worktrees/y")),
            "-home-a-code-x--worktrees-y"
        );
    }
}
