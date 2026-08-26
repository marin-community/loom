//! `loom status` and the `loom summary` catch-up it heads.

use anyhow::{bail, Result};
use clap::Subcommand;

use weaver_api::operations::issues as issue_ops;
use weaver_api::operations::{artifacts, branches, channels, sessions};
use weaver_api::render::sessions::{github_wiring, status_line};
use weaver_api::{BranchView, Client, IssueView, ThreadDto};
use weaver_core::tags;

use super::{branch_key, channel_key, client, render, truncate, working_branch_status};

// `sessions.status.get` and `.set` declare these two invocations and now print
// them — `weaver_api::render::sessions` holds the only copy of the text. What
// keeps the clap enum is the *group*: `status` is these two verbs and nothing
// else, so letting the declarations place them would leave `loom status` a
// registry-assembled group with no description in `loom --help` and clap's
// "the subcommand 'status' wasn't recognized" where it now says "a subcommand
// is required". Removing it also means editing `bin/loom.rs`, which is being
// split elsewhere.
//
// A doc comment here would replace the group's `about` in `--help`.
#[derive(Subcommand)]
pub enum StatusCmd {
    /// Print the current attention level and message.
    Get,
    /// Update the attention level and optional current-state message.
    Set {
        /// Attention level: `ok`, `attention`, or `blocked`.
        #[arg(long)]
        tag: String,
        /// Current-state message, e.g. "Wired up routes; tests pass".
        #[arg(long, default_value = "")]
        message: String,
    },
}

pub async fn run(cmd: StatusCmd) -> Result<()> {
    match cmd {
        StatusCmd::Get => cmd_status(None, String::new()).await,
        StatusCmd::Set { tag, message } => cmd_status(Some(tag), message).await,
    }
}

pub async fn run_summary() -> Result<()> {
    let client = client();
    let branch = client
        .invoke::<branches::get::Op>(&branches::get::Input {
            branch: (branch_key()?).to_string(),
        })
        .await?;
    print!("{}", render_summary(&client, &branch).await?);
    Ok(())
}

/// How many backlog items `loom summary` lists before collapsing the rest.
const SUMMARY_TASK_CAP: usize = 10;

/// Render the `loom summary` catch-up as a string. Kept
/// separate from the printing so the post-compaction hook can replay the same
/// text into the agent's context as `additionalContext` (see [`cmd_hook`]).
///
/// Not a `Render` on `sessions.summary.get`, though it is the obvious
/// candidate. `SessionCatchupView` carries the goal, the level and message, the
/// channel row, the artifacts and the issues — but not the `github` tag this
/// prints, not the last three channel messages, and not the open threads across
/// every artifact. Those are three more reads, one of them per artifact, and
/// each delegated sub-tree costs a fourth to fetch the working branch's live
/// status. It also *writes*: reading the catch-up advances this agent's own
/// read marker on its channel. A renderer is a pure function of one `Output`
/// and can do none of that. Widening the view to carry it all would make
/// `sessions.summary.get` fan out server-side for every caller, including the
/// dashboard, which needs none of it.
pub(super) async fn render_summary(client: &Client, b: &BranchView) -> Result<String> {
    use std::fmt::Write as _;
    let mut out = String::new();

    // Each section trails the command that drills into it, so the summary
    // doubles as a map of where to look next.
    let goal = if !b.goal.is_empty() {
        b.goal.clone()
    } else if !b.title.is_empty() {
        b.title.clone()
    } else {
        "(none set)".to_string()
    };
    let _ = writeln!(out, "Goal:    {goal}  (loom artifacts get goal)");

    let _ = writeln!(out, "Status:  {}  (loom status get)", status_line(b));
    if let Some(wiring) = github_wiring(b) {
        let _ = writeln!(
            out,
            "GitHub:  status messages mirror publicly to {wiring}  (loom sessions tags delete github stops it)"
        );
    }

    // The session channel is the durable inbox and delegation context. Reading
    // summary counts as reading this agent's inbox, so advance its own marker;
    // browser/user markers remain independent.
    if let Ok(channel_id) = channel_key(None) {
        if let Ok(channel) = client
            .invoke::<channels::get::Op>(&channels::get::Input {
                channel: channel_id.to_string(),
                branch: String::new(),
            })
            .await
        {
            let urgent = if channel.unread_urgent_count > 0 {
                format!(", {} urgent", channel.unread_urgent_count)
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "Channel: {} — {} unread{}  (loom channels read)",
                channel.id, channel.unread_count, urgent
            );
            if channel.unread_count > 0 {
                if let Ok(messages) = client.channel_messages(&channel_id, 0).await {
                    for message in messages
                        .iter()
                        .filter(|message| message.kind != "goal")
                        .rev()
                        .take(3)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                    {
                        let _ = writeln!(
                            out,
                            "  {}:{} [{}] {}",
                            message.author_kind,
                            message.author_id,
                            message.kind,
                            truncate(&message.body, 100)
                        );
                    }
                    if let Some(last) = messages.last() {
                        // A catch-up remains useful if acknowledgement races a
                        // channel lifecycle change; the next summary retries.
                        let _ = client
                            .invoke::<channels::read_marker::set::Op>(
                                &channels::read_marker::set::Input {
                                    channel: channel_id.to_string(),
                                    seq: Some(last.seq),
                                    branch: String::new(),
                                },
                            )
                            .await;
                    }
                }
            }
        }
    }

    // Artifacts visible from this branch (its own + repo-shared) — the documents
    // the agent has written to Loom (designs, reports, the `plan`).
    let artifacts = client
        .invoke::<artifacts::list::Op>(&artifacts::list::Input {
            repo: false,
            branch: b.id.to_string(),
        })
        .await
        .unwrap_or_default();
    match artifacts.as_slice() {
        [] => {
            let _ = writeln!(out, "Artifacts: none  (loom artifacts write <name> <file>)");
        }
        [a] => {
            let _ = writeln!(
                out,
                "Artifacts: {} [rev {}]  (loom artifacts get {})",
                a.name, a.rev, a.name
            );
        }
        many => {
            let names = many.iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
            let _ = writeln!(
                out,
                "Artifacts: {}  (loom artifacts list)",
                names.join(", ")
            );
        }
    }

    // Open discussion: unresolved comment threads across every artifact visible
    // from this branch — so a reviewer's feedback surfaces here even if the
    // agent never re-opens the artifact that carries it.
    let mut open_threads: Vec<(String, ThreadDto)> = Vec::new();
    for a in &artifacts {
        if let Ok(threads) = client
            .invoke::<artifacts::threads::list::Op>(&artifacts::threads::list::Input {
                name: a.name.to_string(),
                open_only: false,
                branch: b.id.to_string(),
            })
            .await
        {
            open_threads.extend(
                threads
                    .into_iter()
                    .filter(|t| t.status == "open")
                    .map(|t| (a.name.clone(), t)),
            );
        }
    }
    if !open_threads.is_empty() {
        open_threads.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at));
        out.push('\n');
        let _ = writeln!(out, "Open discussion ({}):", open_threads.len());
        for (name, t) in open_threads.iter().take(SUMMARY_TASK_CAP) {
            let _ = writeln!(
                out,
                "  #{} on {name}: \"{}\"  (loom artifacts threads {name})",
                t.id,
                truncate(&t.anchor.quote, 60)
            );
        }
        if open_threads.len() > SUMMARY_TASK_CAP {
            let _ = writeln!(
                out,
                "  (+{} more — loom artifacts threads <name>)",
                open_threads.len() - SUMMARY_TASK_CAP
            );
        }
    }

    // Intentional work items only: ordinary session delegation is represented
    // by child channels and never appears here.
    let issues = client
        .invoke::<issue_ops::list::Op>(&issue_ops::list::Input {
            repo_root: b.repo_root.clone(),
            all: false,
            backlog: false,
        })
        .await
        .unwrap_or_default();
    let open: Vec<&IssueView> = issues
        .iter()
        .filter(|i| i.claimed_branch.as_deref() == Some(b.branch.as_str()))
        .collect();
    let delegated: Vec<&IssueView> = issues
        .iter()
        .filter(|i| {
            i.source_branch.as_deref() == Some(b.branch.as_str())
                && i.claimed_branch.is_some()
                && i.claimed_branch.as_deref() != Some(b.branch.as_str())
        })
        .collect();
    out.push('\n');
    if open.is_empty() && delegated.is_empty() {
        let _ = writeln!(out, "Backlog: none  (loom issues list)");
    } else {
        let total = open.len() + delegated.len();
        let _ = writeln!(out, "Backlog ({total}):  (loom issues list)");
        // Cap the whole list (own issues first, then delegated sub-trees) so a
        // branch that delegated many sub-trees can't blow the summary up; the
        // overflow collapses into one trailing line.
        let mut shown = 0;
        for i in open.iter().take(SUMMARY_TASK_CAP) {
            let _ = writeln!(out, "  #{:<4} {}", i.id, i.title);
            shown += 1;
        }
        for i in delegated
            .iter()
            .take(SUMMARY_TASK_CAP.saturating_sub(shown))
        {
            let claimed = i.claimed_branch.as_deref().unwrap_or("?");
            let who = match working_branch_status(client, &i.repo_root, claimed).await {
                Some(s) => s,
                None => claimed.to_string(),
            };
            let _ = writeln!(out, "  #{:<4} {}  → {who} (delegated)", i.id, i.title);
            shown += 1;
        }
        if total > shown {
            let _ = writeln!(out, "  (+{} more — loom issues list)", total - shown);
        }
    }

    // Hint for the next step: a generated next-action drawn from the open work.
    // The current status (where work was left off) is already on the `Status:`
    // line above, sourced from the status-description trail.
    out.push('\n');
    let _ = writeln!(out, "Next steps:  (loom sessions events · loom status get)");
    let _ = writeln!(out, "  - {}", next_action_hint(&open, &delegated));
    Ok(out)
}

/// A single suggested next action for `loom summary`, derived from the open
/// work: pick up the first open task, else poll a delegated sub-tree, else
/// (nothing open) wrap up and open a PR.
fn next_action_hint(open: &[&IssueView], delegated: &[&IssueView]) -> String {
    if let Some(first) = open.first() {
        format!(
            "pick up #{} ({}); `loom issues list` for the rest",
            first.id,
            truncate(&first.title, 60)
        )
    } else if !delegated.is_empty() {
        format!(
            "{} delegated sub-tree(s) still open — `loom issues get <id>` to poll",
            delegated.len()
        )
    } else {
        "no explicit backlog items — continue the goal, or wrap up and open a PR (`gh pr create`)"
            .to_string()
    }
}

async fn cmd_status(level: Option<String>, message: String) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    if let Some(level) = level {
        return cmd_status_write(&client, &key, &level, &message).await;
    }
    let b = client
        .invoke::<branches::get::Op>(&branches::get::Input {
            branch: key.to_string(),
        })
        .await?;
    println!("{}", render::<sessions::status::get::Op>(&b));
    Ok(())
}

/// Report the agent's status: set the attention level and, when a message is
/// given, the accompanying current-state note. The level lives on the
/// `attention` tag — `ok` clears it (absence is the calm state), `attention`/
/// `blocked` set it. One call to loom (`POST /branches/{id}/status`), which
/// writes the description, sets or clears the tag, and records a single `tag`
/// event atomically server-side. An empty message leaves the previous message
/// in place — `loom status set --tag ok` just lowers the level without wiping
/// what the agent last said, and the reply says so, because what is printed is
/// the session's status as it now stands rather than the arguments sent.
async fn cmd_status_write(client: &Client, key: &str, level: &str, message: &str) -> Result<()> {
    let level = level.trim().to_ascii_lowercase();
    // `ok` is a valid *input* (return to calm) but is never stored — it clears
    // the tag. The two storable levels come from the tags registry. Checked
    // client-side too so a bad level fails fast, before any network round trip.
    if level != "ok" && !tags::is_valid_value(tags::ATTENTION_KEY, &level) {
        bail!("unknown status '{level}' — expected one of ok, attention, blocked");
    }
    let updated = client
        .invoke::<branches::status::set::Op>(&branches::status::set::Input {
            level: level.to_string(),
            message: (!message.is_empty()).then(|| message.to_string()),
            branch: key.to_string(),
        })
        .await?;
    println!("{}", render::<sessions::status::set::Op>(&updated));
    Ok(())
}
