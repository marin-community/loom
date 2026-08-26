//! The session's own context: where it runs, what it logged, and the agent
//! lifecycle hooks loom installs into it.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use weaver_api::operations::{branches, permissions};
use weaver_api::BranchView;
use weaver_core::tags;

use super::status::render_summary;
use super::{branch_key, client, truncate};

pub async fn run_self() -> Result<()> {
    cmd_where().await
}

pub async fn run_events(limit: i64) -> Result<()> {
    cmd_log(limit).await
}

pub async fn run_hook(event: String) -> Result<()> {
    cmd_hook(event).await
}

pub async fn run_github_token(repository: Option<String>) -> Result<()> {
    cmd_github_token(repository).await
}

pub fn run_chatlog(file: Option<String>, as_json: bool) -> Result<()> {
    cmd_chatlog(file, as_json)
}

// ---------------------------------------------------------------------------
// The loom client and "current branch" resolution
// ---------------------------------------------------------------------------

async fn cmd_github_token(repository: Option<String>) -> Result<()> {
    let session_id =
        std::env::var("LOOM_SESSION_ID").map_err(|_| anyhow!("$LOOM_SESSION_ID is not set"))?;
    let credential = client()
        .invoke::<permissions::github::token::Op>(&permissions::github::token::Input {
            session: session_id.to_string(),
            repository,
        })
        .await?;
    println!("{}", credential.token);
    Ok(())
}

async fn cmd_where() -> Result<()> {
    let client = client();
    let key = branch_key()?;
    let b = client
        .invoke::<branches::get::Op>(&branches::get::Input {
            branch: key.to_string(),
        })
        .await?;
    println!("repo:      {}", b.repo_root);
    println!("branch:    {}", b.branch);
    println!("base:      {}", b.base_branch);
    println!("branch-id: {}", b.id);
    Ok(())
}

async fn cmd_log(limit: i64) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    let mut history = client
        .invoke::<branches::events::list::Op>(&branches::events::list::Input {
            branch: key.to_string(),
        })
        .await?;
    history.truncate(limit.max(0) as usize);
    if history.is_empty() {
        println!("(no events)");
        return Ok(());
    }
    for ev in history {
        let detail = if let Some(s) = ev.data.get("text").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(s) = ev.data.get("status").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(key) = ev.data.get("key").and_then(Value::as_str) {
            // A tag event. For the agent's own `attention` reports this is the
            // status trail: `level — message`; an empty value is the calm `ok`
            // (any other key cleared reads as "cleared").
            let value = ev.data.get("value").and_then(Value::as_str).unwrap_or("");
            let note = ev.data.get("note").and_then(Value::as_str).unwrap_or("");
            let shown = match (key, value) {
                (k, "") if k == tags::ATTENTION_KEY => "ok".to_string(),
                (k, "") => format!("{k} cleared"),
                (k, v) if k == tags::ATTENTION_KEY => v.to_string(),
                (k, v) => format!("{k}: {v}"),
            };
            if note.is_empty() {
                shown
            } else {
                format!("{shown} — {note}")
            }
        } else if let Some(level) = ev.data.get("level").and_then(Value::as_str) {
            match ev.data.get("note").and_then(Value::as_str) {
                Some(n) if !n.is_empty() => format!("{level} — {n}"),
                _ => level.to_string(),
            }
        } else if let Some(s) = ev.data.get("event").and_then(Value::as_str) {
            s.to_string()
        } else if let Some(s) = ev.data.get("goal").and_then(Value::as_str) {
            truncate(s, 60)
        } else {
            ev.data.to_string()
        };
        println!(
            "{}  {:<10}  {}",
            ev.created_at,
            ev.kind,
            truncate(&detail, 100)
        );
    }
    Ok(())
}

/// Ascend from `start` to the enclosing git worktree root (the directory holding
/// a `.git` entry — a dir in a normal clone, a file in a linked worktree).
/// Falls back to `start` when none is found, so a non-repo path still resolves.
fn worktree_root(start: &std::path::Path) -> std::path::PathBuf {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return start.to_path_buf(),
        }
    }
}

/// Render the current worktree's (or a named file's) agent transcript. No
/// network access — pure filesystem, so it works whether or not loom is
/// reachable (the agent and its own transcript file are always co-located,
/// regardless of the isolation model).
fn cmd_chatlog(file: Option<String>, as_json: bool) -> Result<()> {
    use weaver_core::transcript;
    let log = match file {
        Some(path) => {
            let raw = std::fs::read_to_string(&path).map_err(|e| anyhow!("reading {path}: {e}"))?;
            transcript::parse(&raw)
                .ok_or_else(|| anyhow!("{path}: unrecognized transcript format"))?
        }
        None => {
            // Agents key their transcript off the worktree root (where the agent
            // was launched), so resolve that rather than the possibly-deeper cwd.
            let cwd = std::env::current_dir()?;
            let root = worktree_root(&cwd);
            let (_, files) = transcript::locate(&root)
                .ok_or_else(|| anyhow!("no agent transcript found for {}", root.display()))?;
            transcript::parse_files(&files)
                .ok_or_else(|| anyhow!("transcript found but could not be parsed"))?
        }
    };
    if as_json {
        println!("{}", log.to_json());
    } else {
        print!("{}", log.render_markdown());
    }
    Ok(())
}

/// The WEAVER.md to inject at session start: the repo's own copy when it ships
/// one, else the builtin. We look in the worktree the hook is actually running
/// in (its cwd at launch is the worktree root) and then in the primary checkout,
/// so a `WEAVER.md` committed on the base branch is picked up either way.
fn weaver_md_for_branch(branch: &BranchView) -> String {
    let candidates = std::env::current_dir()
        .ok()
        .into_iter()
        .chain(std::iter::once(std::path::PathBuf::from(&branch.repo_root)));
    for dir in candidates {
        if let Ok(md) = std::fs::read_to_string(dir.join("WEAVER.md")) {
            if !md.trim().is_empty() {
                return md;
            }
        }
    }
    weaver_core::agent::builtin_weaver_md().to_string()
}

/// Read the `source` field a SessionStart hook receives as JSON on stdin
/// (`startup` | `resume` | `clear` | `compact`). Returns `None` when stdin is a
/// terminal (a human running the hook by hand), empty, or unparseable — callers
/// then fall back to the full-primer behaviour, which is always safe.
fn read_hook_source() -> Option<String> {
    use std::io::{IsTerminal, Read};
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    stdin.read_to_string(&mut buf).ok()?;
    let v: Value = serde_json::from_str(buf.trim()).ok()?;
    v.get("source")?.as_str().map(str::to_owned)
}

/// The concise Loom re-orientation replayed after a context compaction: a short
/// reminder that this is still a Loom session, the supplied catch-up summary,
/// and the load-bearing rules an agent must not lose (status, no blocking TUI
/// prompts, PR-not-merge, and typed result delivery). The command surface is
/// discoverable through `loom help`.
fn compact_replay(b: &BranchView, summary: &str) -> String {
    let summary = summary.trim_end();
    format!(
        "Context was just compacted — you are still in a **Loom session** on branch `{branch}` (a detached agent workstream in a git worktree; the user reviews asynchronously via the Loom dashboard, not this terminal). Re-orientation:\n\n{summary}\n\nReminders: keep your status honest with `loom status set --tag <ok|attention|blocked> --message \"<message>\"`; state questions as plain text and raise attention; finish by opening a PR rather than merging; append a typed result with `loom channels send --kind result \"<outcome>\"`. Run `loom help` to rediscover the command surface.\n",
        branch = b.branch,
    )
}

async fn cmd_hook(event: String) -> Result<()> {
    // A nested, isolated agent — a headless `claude -p` review, lint, or one-shot
    // spawned from inside a session — carries no `$WEAVER_BRANCH` (the spawner
    // strips it precisely so the child doesn't impersonate the parent). It reads
    // the worktree's `.claude/settings.local.json` all the same and fires these
    // lifecycle hooks; with no branch to key on, the hook is intentionally inert.
    // Return quietly — writing nothing, printing nothing — rather than surfacing
    // the "not in a loom session" error `branch_key` would raise for a real
    // command. This is the server-side half of the fix: even a nested agent that
    // still fires the hook cannot stamp the parent branch's lifecycle.
    if std::env::var("WEAVER_BRANCH")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Ok(());
    }
    // Hooks must never break the agent: best-effort, swallow errors.
    let result: Result<()> = (async {
        let client = client();
        let key = branch_key()?;
        // SessionStart carries a `source` on stdin (startup|resume|clear|compact);
        // we only read it for that event so other hooks don't touch stdin.
        let is_session_start = event == "session-start";
        let source = if is_session_start {
            read_hook_source()
        } else {
            None
        };
        let is_compact = source.as_deref() == Some("compact");
        client
            .record_branch_event(&key, "hook", json!({ "event": event, "source": source }))
            .await?;
        if is_session_start {
            // After a compaction the agent has lost its working context but the
            // session is unchanged — replay a concise re-orientation (the
            // `loom summary` catch-up) rather than the full repository primer. On a
            // genuine start/resume/clear, inject the full primer.
            let b = client
                .invoke::<branches::get::Op>(&branches::get::Input {
                    branch: key.to_string(),
                })
                .await?;
            let context = if is_compact {
                let summary = render_summary(&client, &b).await.unwrap_or_default();
                compact_replay(&b, &summary)
            } else {
                weaver_md_for_branch(&b)
            };
            print!("{}", weaver_core::agent::session_primer(&context));
        }
        Ok(())
    })
    .await;
    if let Err(e) = result {
        eprintln!("loom hook: {e}");
    }
    Ok(())
}
