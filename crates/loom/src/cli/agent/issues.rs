//! `loom issues` — work items, their tags, and the delegated sub-trees they track.

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;

use weaver_api::operations::branches;
use weaver_api::operations::issues as issue_ops;
use weaver_api::render::sessions::{attention, status_line};
use weaver_api::{Client, IssueAction, IssueView};
use weaver_core::tags;

use super::{branch_key, client, working_branch_status};

// - `add` joins its trailing words into a title and picks between two
//   operations — `issues.create` or `issues.backlog.create` — on `--repo`.
// - `ls`, `get` and `wait` poll the branch working each issue for its live
//   status: a round trip per delegated sub-tree, and a renderer has no client.
// - `tag set`/`delete` take a run of ids followed by a key (and a value), which
//   one declared positional cannot be; `issues.tags.*` act on a single id.
// - `tag ls` has no operation: nothing declared lists one item's tags.
#[derive(Subcommand)]
pub enum IssueCmd {
    /// Add an issue. By default it is claimed by the current branch; `--repo`
    /// creates an unclaimed repo-level backlog item instead.
    Add {
        title: Vec<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        github: Option<i64>,
        /// Create an unclaimed repo backlog item, not attached to this branch.
        #[arg(long)]
        repo: bool,
    },
    /// List issues. Default: this branch's work + the unclaimed repo backlog.
    Ls {
        /// Include closed issues.
        #[arg(long)]
        all: bool,
        /// Show every issue in the repo (all branches + backlog), uncapped.
        #[arg(long)]
        repo: bool,
        /// Show only this branch's claimed issues (suppress the backlog).
        #[arg(long)]
        mine: bool,
        /// Use a different branch as "this branch" (by name).
        #[arg(long)]
        branch: Option<String>,
    },
    /// Show one issue, including the live status of the branch working it.
    #[command(name = "get", visible_alias = "show")]
    Show { id: i64 },
    /// Block until an issue finishes or its sub-tree needs you.
    ///
    /// Polls the issue until it closes (the sub-agent's "done" signal) or — unless
    /// `--closed-only` — until the branch working it raises its attention to
    /// `attention`/`blocked` (it wants you). Prints why it woke. Exits non-zero
    /// if `--timeout` elapses first with the issue still open.
    Wait {
        id: i64,
        /// Give up after this many seconds (0 = wait indefinitely).
        #[arg(long, default_value = "1800")]
        timeout: u64,
        /// Seconds between polls.
        #[arg(long, default_value = "3")]
        interval: u64,
        /// Wake only when the issue closes; ignore the sub-agent's attention.
        #[arg(long)]
        closed_only: bool,
    },
    /// Label an issue with free-form `(key, value)` tags: set, rm, or ls.
    ///
    /// Issue tags are quiet annotations (priority, area, kind, …) rendered as
    /// pills in the loom Issues pane — there is no loud `attention`/`triage`
    /// ladder.
    Tag {
        #[command(subcommand)]
        cmd: IssueTagCmd,
    },
}

#[derive(Subcommand)]
pub enum IssueTagCmd {
    /// Set (insert or replace) a tag on an issue. The value must be non-empty;
    /// clear a label with `loom issues tag delete`.
    Set {
        /// One or more numeric issue ids followed by the tag key and value.
        #[arg(required = true, num_args = 3..)]
        args: Vec<String>,
        /// One-line reason accompanying the tag.
        #[arg(long, default_value = "")]
        note: String,
        /// Who is setting it (attribution); defaults to `agent`.
        #[arg(long, default_value = "agent")]
        by: String,
    },
    /// Clear an issue label — delete the `(key)` tag.
    #[command(name = "delete", visible_alias = "rm")]
    Rm {
        /// One or more numeric issue ids followed by the tag key.
        #[arg(required = true, num_args = 2..)]
        args: Vec<String>,
    },
    /// List an issue's tags.
    Ls { id: i64 },
}

pub async fn run(cmd: IssueCmd) -> Result<()> {
    cmd_issue(cmd).await
}

/// How many backlog items to print before collapsing the rest into a hint.
const BACKLOG_CAP: usize = 10;

async fn cmd_issue(cmd: IssueCmd) -> Result<()> {
    let client = client();
    let key = branch_key()?;
    let b = client
        .invoke::<branches::get::Op>(&branches::get::Input {
            branch: key.to_string(),
        })
        .await?;
    match cmd {
        IssueCmd::Add {
            title,
            body,
            github,
            repo,
        } => {
            let title = title.join(" ");
            if title.trim().is_empty() {
                bail!("issue title is required");
            }
            let i = if repo {
                client
                    .invoke::<issue_ops::backlog::create::Op>(&issue_ops::backlog::create::Input {
                        tags: Vec::new(),
                        title: title.clone(),
                        body: body.unwrap_or_default(),
                        github_issue: github,
                        repo_root: b.repo_root.clone(),
                        source_branch: Some(b.branch.clone()),
                    })
                    .await?
            } else {
                client
                    .invoke::<issue_ops::create::Op>(&issue_ops::create::Input {
                        title: title.clone(),
                        body: body.unwrap_or_default(),
                        github_issue: github,
                        branch: b.id.clone(),
                    })
                    .await?
            };
            println!("#{} {}", i.id, i.title);
        }
        IssueCmd::Ls {
            all,
            repo,
            mine,
            branch,
        } => {
            let target = branch.unwrap_or_else(|| b.branch.clone());
            let issues = client
                .invoke::<issue_ops::list::Op>(&issue_ops::list::Input {
                    repo_root: b.repo_root.clone(),
                    all,
                    backlog: false,
                })
                .await?;
            if repo {
                print_issue_ls_repo(&issues, &target);
            } else {
                print_issue_ls_default(&client, &issues, &target, mine).await;
            }
        }
        IssueCmd::Show { id } => {
            let i = client
                .invoke::<issue_ops::get::Op>(&issue_ops::get::Input {
                    id,
                    repo_root: b.repo_root.clone(),
                })
                .await?;
            ensure_issue_in_repo(&i, &b.repo_root)?;
            println!("#{} {}", i.id, i.title);
            println!("  status:  {}", i.status);
            println!(
                "  claimed: {}",
                i.claimed_branch.as_deref().unwrap_or("(backlog)")
            );
            // Surface the live status of the branch working this issue — what
            // makes `issue show` a poll of a delegated sub-tree, not just a
            // record lookup.
            if let Some(claimed) = &i.claimed_branch {
                if let Some(progress) = working_branch_status(&client, &i.repo_root, claimed).await
                {
                    println!("  working: {progress}");
                }
            }
            if let Some(src) = &i.source_branch {
                println!("  from:    {src}");
            }
            if let Some(n) = i.github_issue {
                let slug = i.github_repo.as_deref().unwrap_or_default();
                match &i.github_state {
                    // The live thread, as GitHub reports it right now — catches
                    // "closed / re-titled while you worked". `gh` has the rest.
                    Some(gh) => {
                        let renamed = if gh.title != i.title && !gh.title.is_empty() {
                            format!(" — {:?}", gh.title)
                        } else {
                            String::new()
                        };
                        println!(
                            "  github:  {slug}#{n} {}{renamed} (updated {})",
                            gh.state,
                            age_of(&gh.updated_at)
                        );
                    }
                    None if slug.is_empty() => println!("  github:  #{n}"),
                    None => println!("  github:  {slug}#{n}"),
                }
            }
            if !i.tags.is_empty() {
                let rendered = i
                    .tags
                    .iter()
                    .map(|t| format!("{}={}", t.key, t.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  tags:    {rendered}");
            }
            println!("  created: {}", i.created_at);
            if let Some(c) = &i.closed_at {
                println!("  closed:  {c}");
            }
            if !i.body.is_empty() {
                println!();
                println!("{}", i.body);
            }
        }
        IssueCmd::Wait {
            id,
            timeout,
            interval,
            closed_only,
        } => {
            cmd_issue_wait(
                &client,
                &b.repo_root,
                id,
                timeout,
                interval.max(1),
                closed_only,
            )
            .await?;
        }
        IssueCmd::Tag { cmd } => cmd_issue_tag(&client, &b.repo_root, cmd).await?,
    }
    Ok(())
}

/// Set, clear, or list a free-form tag on an issue (`loom issues tag …`).
async fn cmd_issue_tag(client: &Client, repo_root: &str, cmd: IssueTagCmd) -> Result<()> {
    match cmd {
        IssueTagCmd::Set { args, note, by } => {
            let (ids, key, value) = parse_issue_tag_set_args(args)?;
            let key = key.trim();
            let value = value.trim();
            let note = note.trim();
            let by = by.trim();
            if key.is_empty() {
                bail!("a tag key is required");
            }
            if value.is_empty() {
                bail!(
                    "a tag value cannot be empty — use `loom issues tag delete <ids...> {key}` to clear it"
                );
            }
            let result = client
                .invoke::<issue_ops::actions::Op>(&issue_ops::actions::Input {
                    ids,
                    action: IssueAction::Tag {
                        key: key.to_string(),
                        value: value.to_string(),
                        note: note.to_string(),
                        by: Some(by.to_string()),
                    },
                    repo_root: repo_root.to_string(),
                })
                .await?;
            if note.is_empty() {
                println!(
                    "tagged {} issue(s): {key} = {value} (by {by})",
                    result.issues.len()
                );
            } else {
                println!(
                    "tagged {} issue(s): {key} = {value} (by {by}) — {note}",
                    result.issues.len()
                );
            }
        }
        IssueTagCmd::Rm { args } => {
            let (ids, key) = parse_issue_tag_delete_args(args)?;
            let result = client
                .invoke::<issue_ops::actions::Op>(&issue_ops::actions::Input {
                    ids,
                    action: IssueAction::Untag { key: key.clone() },
                    repo_root: repo_root.to_string(),
                })
                .await?;
            println!("cleared {key} from {} issue(s)", result.issues.len());
        }
        IssueTagCmd::Ls { id } => {
            let i = client
                .invoke::<issue_ops::get::Op>(&issue_ops::get::Input {
                    id,
                    repo_root: repo_root.to_string(),
                })
                .await?;
            ensure_issue_in_repo(&i, repo_root)?;
            if i.tags.is_empty() {
                println!("(no tags)");
                return Ok(());
            }
            for t in &i.tags {
                let note = if t.note.is_empty() {
                    String::new()
                } else {
                    format!("  — {}", t.note)
                };
                println!("{} = {}{note}", t.key, t.value);
            }
        }
    }
    Ok(())
}

fn parse_issue_ids(values: &[String]) -> Result<Vec<i64>> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| anyhow!("'{value}' is not a positive issue id"))
        })
        .collect()
}

fn parse_issue_tag_set_args(mut args: Vec<String>) -> Result<(Vec<i64>, String, String)> {
    if args.len() < 3 {
        bail!("tag set requires issue id(s), key, and value");
    }
    let value = args.pop().expect("length checked");
    let key = args.pop().expect("length checked");
    Ok((parse_issue_ids(&args)?, key, value))
}

fn parse_issue_tag_delete_args(mut args: Vec<String>) -> Result<(Vec<i64>, String)> {
    if args.len() < 2 {
        bail!("tag delete requires issue id(s) and key");
    }
    let key = args.pop().expect("length checked");
    Ok((parse_issue_ids(&args)?, key))
}

fn issue_line(i: &IssueView) -> String {
    let marker = if i.status == "open" { "[ ]" } else { "[x]" };
    let gh = i
        .github_issue
        .map(|n| format!(" (gh #{n})"))
        .unwrap_or_default();
    format!("#{:<4} {} {}{}", i.id, marker, i.title, gh)
}

/// Default `ls`: this branch's working set, plus the unclaimed repo backlog
/// (capped). `--mine` drops the backlog section. `issues` is one repo-wide
/// fetch (`all` already applied server-side); every section here is a
/// client-side partition of it.
async fn print_issue_ls_default(client: &Client, issues: &[IssueView], target: &str, mine: bool) {
    let working: Vec<&IssueView> = issues
        .iter()
        .filter(|i| i.claimed_branch.as_deref() == Some(target))
        .collect();
    let mut printed = false;
    if !working.is_empty() {
        println!("On this branch ({}):", working.len());
        for i in &working {
            println!("  {}", issue_line(i));
        }
        printed = true;
    }
    // Sub-trees this branch launched: tracking issues it sourced but another
    // branch is working. Each carries its sub-agent's live status.
    let delegated: Vec<&IssueView> = issues
        .iter()
        .filter(|i| {
            i.source_branch.as_deref() == Some(target)
                && i.claimed_branch.is_some()
                && i.claimed_branch.as_deref() != Some(target)
        })
        .collect();
    if !delegated.is_empty() {
        println!("Delegated by this branch ({}):", delegated.len());
        for i in &delegated {
            let claimed = i.claimed_branch.as_deref().unwrap_or("?");
            let status = match working_branch_status(client, &i.repo_root, claimed).await {
                Some(s) => s,
                None => claimed.to_string(),
            };
            println!("  {}  → {status}", issue_line(i));
        }
        printed = true;
    }
    if !mine {
        let backlog: Vec<&IssueView> = issues
            .iter()
            .filter(|i| i.claimed_branch.is_none())
            .collect();
        if !backlog.is_empty() {
            let shown = backlog.len().min(BACKLOG_CAP);
            println!(
                "Repo backlog ({} unclaimed, showing {}):",
                backlog.len(),
                shown
            );
            for i in backlog.iter().take(BACKLOG_CAP) {
                println!("  {}", issue_line(i));
            }
            if backlog.len() > BACKLOG_CAP {
                println!(
                    "  (+{} more — loom issues list --repo)",
                    backlog.len() - BACKLOG_CAP
                );
            }
            printed = true;
        }
    }
    if !printed {
        println!("(no issues)");
    }
}

/// `ls --repo`: every open (or, with `--all`, every) issue in the repo, grouped
/// into this branch / unclaimed backlog / other branches.
fn print_issue_ls_repo(issues: &[IssueView], target: &str) {
    if issues.is_empty() {
        println!("(no issues)");
        return;
    }
    let mut mine = Vec::new();
    let mut backlog = Vec::new();
    let mut others = Vec::new();
    for i in issues {
        match i.claimed_branch.as_deref() {
            Some(b) if b == target => mine.push(i),
            Some(_) => others.push(i),
            None => backlog.push(i),
        }
    }
    let section = |title: String, items: &[&IssueView]| {
        if items.is_empty() {
            return;
        }
        println!("{title}");
        for i in items {
            // Annotate cross-branch items with who holds them.
            let who = i
                .claimed_branch
                .as_deref()
                .filter(|b| *b != target)
                .map(|b| format!("  ← {b}"))
                .unwrap_or_default();
            println!("  {}{}", issue_line(i), who);
        }
    };
    section(format!("On this branch ({}):", mine.len()), &mine);
    section(
        format!("Repo backlog ({} unclaimed):", backlog.len()),
        &backlog,
    );
    section(format!("Other branches ({}):", others.len()), &others);
}

/// Confirm an issue exists and lives in `repo_root`. Cross-*repo* access is the
/// real mistake to guard; within a repo, claimed and backlog items are all fair
/// game.
fn ensure_issue_in_repo(i: &IssueView, repo_root: &str) -> Result<()> {
    if i.repo_root != repo_root {
        bail!("issue #{} belongs to a different repo", i.id);
    }
    Ok(())
}

/// Block until issue `id` finishes (closes) or — unless `closed_only` — its
/// claiming branch raises attention above `ok`. Polls every `interval` seconds;
/// exits the process non-zero if `timeout` (when non-zero) elapses first.
async fn cmd_issue_wait(
    client: &Client,
    repo_root: &str,
    id: i64,
    timeout: u64,
    interval: u64,
    closed_only: bool,
) -> Result<()> {
    let issue = client
        .invoke::<issue_ops::get::Op>(&issue_ops::get::Input {
            id,
            repo_root: repo_root.to_string(),
        })
        .await?;
    ensure_issue_in_repo(&issue, repo_root)?;
    if issue.status != "open" {
        println!("issue #{id} is {} — nothing to wait for", issue.status);
        return Ok(());
    }
    match issue.claimed_branch.as_deref() {
        Some(claimed) => match working_branch_status(client, repo_root, claimed).await {
            Some(s) => println!("waiting on #{id} ({}) — {s}", issue.title),
            None => println!("waiting on #{id} ({})", issue.title),
        },
        None => println!("waiting on #{id} ({})", issue.title),
    }

    let interval = std::time::Duration::from_secs(interval);
    let deadline =
        (timeout > 0).then(|| std::time::Instant::now() + std::time::Duration::from_secs(timeout));
    loop {
        // Never nap past the deadline: a long `--interval` must not stretch a
        // short `--timeout`.
        let nap = match deadline {
            Some(d) => interval.min(d.saturating_duration_since(std::time::Instant::now())),
            None => interval,
        };
        tokio::time::sleep(nap).await;
        let cur = client
            .invoke::<issue_ops::get::Op>(&issue_ops::get::Input {
                id,
                repo_root: repo_root.to_string(),
            })
            .await?;
        if cur.status != "open" {
            println!("issue #{id} closed — sub-tree finished");
            return Ok(());
        }
        if !closed_only {
            if let Some(name) = cur.claimed_branch.as_deref() {
                let key = format!("{repo_root}:{name}");
                if let Ok(row) = client
                    .invoke::<branches::get::Op>(&branches::get::Input {
                        branch: key.to_string(),
                    })
                    .await
                {
                    // The sub-agent wants the user when its `attention` tag is
                    // present with a loud value (`attention`/`blocked`); absence
                    // is the calm `ok` state.
                    if tags::ATTENTION_VALUES.contains(&attention(&row).as_str()) {
                        println!("issue #{id} needs you — {name} is {}", status_line(&row));
                        return Ok(());
                    }
                }
            }
        }
        // Timing out is a real "not done" outcome: report it as an error so the
        // process exits non-zero (callers branch on it) without an ad-hoc
        // `process::exit` that bypasses normal error handling.
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            let progress = match cur.claimed_branch.as_deref() {
                Some(c) => working_branch_status(client, repo_root, c)
                    .await
                    .unwrap_or_else(|| "open".to_string()),
                None => "open".to_string(),
            };
            bail!("timed out after {timeout}s — #{id} still open ({progress})");
        }
    }
}

/// A compact "how long ago" for an ISO-8601 timestamp: `3m ago`, `2h ago`,
/// `5d ago`. Unparseable input (or the future, from clock skew) renders as
/// `just now` — this is orientation, not arithmetic anyone acts on.
fn age_of(iso: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return "just now".to_string();
    };
    let mins = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_minutes();
    match mins {
        i64::MIN..=1 => "just now".to_string(),
        2..=119 => format!("{mins}m ago"),
        120..=2879 => format!("{}h ago", mins / 60),
        _ => format!("{}d ago", mins / 1440),
    }
}
