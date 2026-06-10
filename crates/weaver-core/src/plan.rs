//! Structured project plans.
//!
//! A plan is a single markdown **file** (default `docs/plans/<slug>.md`) holding
//! the design — a problem statement, a `mermaid` architecture diagram — and a
//! task breakdown with **stable task IDs** (`T1`, `T2`, …). The file owns
//! *structure*; the [`crate::issue`] ledger owns *state*. The two are joined by
//! a task's `plan_task` key (`"<slug>#T3"`), and reconciliation
//! ([`diff`] / [`apply`]) keeps them in step. See `docs/structured-projects.md`.
//!
//! This module is pure model: a tolerant hand-written parser (no markdown
//! dependency — the format is line-oriented), a scaffold generator, and the
//! reconcile diff/apply. It never writes status back into the file; task status
//! is always projected from the issue ledger at read time.

use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

use crate::branch::Branch;
use crate::db::Db;
use crate::issue::{self, Issue, NewIssue};

/// A parsed plan: the structure, never the live status.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    /// Filename stem — the canonical identity, used to key linked issues.
    pub slug: String,
    /// First `# ` heading, or the slug if none.
    pub title: String,
    /// Frontmatter `status:` (e.g. `draft`, `active`, `done`); `draft` default.
    pub status: String,
    pub tasks: Vec<PlanTask>,
}

/// One task in a plan. The heading carries the stable id, the title, and inline
/// `` `key: value` `` annotations.
#[derive(Debug, Clone, Serialize)]
pub struct PlanTask {
    /// Stable id, `T<n>`. Assigned once, never reused — the join key's tail.
    pub id: String,
    pub title: String,
    /// How the task runs: `session`/`issue` materialize into the ledger;
    /// `inline`/`workflow` are execution detail and never become issues.
    /// Defaults to `session` when unannotated.
    pub exec: String,
    /// Freeform priority hint (`high`/`med`/`low`/…); drives dashboard sorting.
    pub value: String,
    /// Ids of tasks this one depends on, for the dependency graph.
    pub deps: Vec<String>,
    /// Body text under the heading (acceptance criteria, notes).
    pub body: String,
}

impl PlanTask {
    /// Whether this task becomes a tracked issue. Only `session`/`issue` tasks
    /// hit the ledger; `inline`/`workflow` are execution detail.
    pub fn materializes(&self) -> bool {
        matches!(self.exec.as_str(), "session" | "issue")
    }

    /// The `plan_task` link key for this task within `slug`.
    pub fn key(&self, slug: &str) -> String {
        format!("{slug}#{}", self.id)
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a plan file. Tolerant by design — it never errors; malformed regions
/// are skipped so a half-written plan still yields whatever structure is there.
/// `slug` (the filename stem) is the canonical identity regardless of any
/// frontmatter `plan:` value.
pub fn parse(slug: &str, src: &str) -> Plan {
    let lines: Vec<&str> = src.lines().collect();
    let mut status = "draft".to_string();
    let mut title = String::new();
    let mut tasks = Vec::new();

    let mut i = 0;

    // Optional YAML-ish frontmatter delimited by `---` lines.
    if lines.first().map(|l| l.trim()) == Some("---") {
        i = 1;
        while i < lines.len() && lines[i].trim() != "---" {
            if let Some((k, v)) = lines[i].split_once(':') {
                if k.trim() == "status" {
                    status = v.trim().to_string();
                }
            }
            i += 1;
        }
        if i < lines.len() {
            i += 1; // skip the closing `---`
        }
    }

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // First level-1 heading is the title.
        if title.is_empty() && is_h1(trimmed) {
            title = trimmed[2..].trim().to_string();
            i += 1;
            continue;
        }

        // A `### T<n> — …` heading starts a task; its body runs to the next
        // heading of any level.
        if let Some((id, rest)) = task_heading(trimmed) {
            let (ttitle, exec, value, deps) = parse_task_heading(rest);
            let mut body = String::new();
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with('#') {
                body.push_str(lines[j]);
                body.push('\n');
                j += 1;
            }
            tasks.push(PlanTask {
                id,
                title: ttitle,
                exec,
                value,
                deps,
                body: body.trim().to_string(),
            });
            i = j;
            continue;
        }

        i += 1;
    }

    if title.is_empty() {
        title = slug.to_string();
    }
    Plan {
        slug: slug.to_string(),
        title,
        status,
        tasks,
    }
}

/// True for a level-1 (`# `) heading, excluding deeper ones.
fn is_h1(t: &str) -> bool {
    t.starts_with("# ") && !t.starts_with("##")
}

/// If `t` is a `### T<n> …` task heading, return `(id, rest_after_id)`.
fn task_heading(t: &str) -> Option<(String, &str)> {
    let rest = t.strip_prefix("### ")?;
    let rest = rest.trim_start();
    let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let token = &rest[..token_end];
    if token.len() >= 2 && token.starts_with('T') && token[1..].chars().all(|c| c.is_ascii_digit())
    {
        Some((token.to_string(), &rest[token_end..]))
    } else {
        None
    }
}

/// Parse the part of a task heading after the id into
/// `(title, exec, value, deps)`. The title is the text before the first
/// `` ` ``; annotations are `` `key: value` `` spans anywhere on the line.
fn parse_task_heading(after_id: &str) -> (String, String, String, Vec<String>) {
    let parts: Vec<&str> = after_id.split('`').collect();
    let title = strip_separators(parts.first().copied().unwrap_or(""));

    let mut exec = "session".to_string();
    let mut value = String::new();
    let mut deps = Vec::new();

    // Odd-indexed segments are inside backticks.
    for (k, span) in parts.iter().enumerate() {
        if k % 2 == 0 {
            continue;
        }
        if let Some((key, val)) = span.split_once(':') {
            match key.trim().to_ascii_lowercase().as_str() {
                "exec" => exec = val.trim().to_ascii_lowercase(),
                "value" => value = val.trim().to_string(),
                "deps" => deps = parse_deps(val),
                _ => {}
            }
        }
    }
    (title, exec, value, deps)
}

/// Trim, then drop a leading run of title separators (em/en dash, hyphen, colon).
fn strip_separators(raw: &str) -> String {
    raw.trim()
        .trim_start_matches(['—', '–', '-', ':', ' '])
        .trim()
        .to_string()
}

/// Parse a `deps:` value (`T1, T2` or `—`/`none`) into task ids.
fn parse_deps(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !matches!(*s, "—" | "–" | "-" | "none" | "None" | "NONE"))
        .map(str::to_string)
        .collect()
}

/// A starter plan file: frontmatter, the four standard sections, and one
/// example task showing the heading-annotation format.
///
/// `goal` seeds the *Problem & goal* section — a plan is the branch goal grown
/// up, so the launch instruction becomes the starting problem statement rather
/// than an empty prompt. An empty goal falls back to the prompt.
pub fn scaffold(slug: &str, title: &str, goal: &str) -> String {
    let problem = if goal.trim().is_empty() {
        "_What are we building, and why? What does \"done\" look like?_".to_string()
    } else {
        format!(
            "{}\n\n_Refine the launch goal above into the problem and what \"done\" looks like._",
            goal.trim()
        )
    };
    format!(
        r#"---
plan: {slug}
status: draft
---

# {title}

## Problem & goal

{problem}

## Architecture

```mermaid
flowchart TD
    a["Component A"] --> b["Component B"]
```

## Tasks

Each task has a stable id (`T1`, `T2`, …). `exec: session|issue` tasks become
weaver issues on `weaver plan sync`; `inline`/`workflow` tasks do not. Status is
projected from the issue ledger — never hand-edit it here.

### T1 — First task  `exec: session`  `value: high`  `deps: —`

_What this task delivers; acceptance criteria._

## Open questions

- ...
"#
    )
}

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// One reconciliation step between the plan file and the issue ledger.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncAction {
    /// A new materializing task with no issue yet → create an open backlog issue.
    Create { task: String, title: String },
    /// A task removed from the plan whose issue is open & unclaimed → close it.
    Close { task: String, issue_id: i64 },
    /// A task whose (unclaimed) issue's title drifted from the plan → update it.
    UpdateTitle {
        task: String,
        issue_id: i64,
        title: String,
    },
    /// A task whose issue is **in-flight** (claimed by a session) diverged from
    /// the plan. Never touched — flagged for a human to decide.
    Flag {
        task: String,
        issue_id: i64,
        branch: String,
        reason: String,
    },
}

/// The full delta from a reconcile: the ordered actions plus the flag count
/// (the bit that needs a human).
#[derive(Debug, Clone, Serialize)]
pub struct SyncPlan {
    pub actions: Vec<SyncAction>,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Count of in-flight divergences a human must resolve.
    pub fn flags(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, SyncAction::Flag { .. }))
            .count()
    }
}

/// Diff a plan's tasks against the issues already linked to it (pass *all*
/// linked issues, open and closed). Pure — computes the delta without touching
/// the database, so a dry run and an apply share the exact same plan.
///
/// The rules (docs/structured-projects.md): a new materializing task → Create;
/// a removed task with an open, unclaimed issue → Close; a title that drifted on
/// an unclaimed issue → UpdateTitle; **any divergence on a claimed (in-flight)
/// issue → Flag, never a silent rewrite**; closed issues are left alone.
pub fn diff(slug: &str, tasks: &[PlanTask], issues: &[Issue]) -> SyncPlan {
    let mut actions = Vec::new();

    let by_key: HashMap<&str, &Issue> = issues
        .iter()
        .filter_map(|i| i.plan_task.as_deref().map(|k| (k, i)))
        .collect();
    let task_keys: HashSet<String> = tasks.iter().map(|t| t.key(slug)).collect();

    // Tasks → issues. A plan with a duplicated task id (two `### T1` headings)
    // must not emit the same action twice — the first occurrence wins, so a
    // dup can never spawn two issues sharing one `plan_task` key.
    let mut handled: HashSet<String> = HashSet::new();
    for t in tasks {
        if !t.materializes() {
            continue;
        }
        let key = t.key(slug);
        if !handled.insert(key.clone()) {
            continue;
        }
        match by_key.get(key.as_str()) {
            None => actions.push(SyncAction::Create {
                task: t.id.clone(),
                title: t.title.clone(),
            }),
            Some(i) => {
                // A closed issue is done; leave it (and don't recreate it).
                if i.status != "open" {
                    continue;
                }
                if i.title != t.title {
                    match i.claimed_branch.as_deref() {
                        Some(branch) => actions.push(SyncAction::Flag {
                            task: t.id.clone(),
                            issue_id: i.id,
                            branch: branch.to_string(),
                            reason: "title changed in the plan while a session is working it"
                                .to_string(),
                        }),
                        None => actions.push(SyncAction::UpdateTitle {
                            task: t.id.clone(),
                            issue_id: i.id,
                            title: t.title.clone(),
                        }),
                    }
                }
            }
        }
    }

    // Issues whose task is gone from the plan.
    for i in issues {
        let Some(key) = i.plan_task.as_deref() else {
            continue;
        };
        if task_keys.contains(key) || i.status != "open" {
            continue;
        }
        let task = key.rsplit('#').next().unwrap_or(key).to_string();
        match i.claimed_branch.as_deref() {
            Some(branch) => actions.push(SyncAction::Flag {
                task,
                issue_id: i.id,
                branch: branch.to_string(),
                reason: "removed from the plan while a session is working it".to_string(),
            }),
            None => actions.push(SyncAction::Close {
                task,
                issue_id: i.id,
            }),
        }
    }

    SyncPlan { actions }
}

/// Apply a [`SyncPlan`] to the issue ledger. Creates backlog issues
/// (`source_branch` = the syncing branch, unclaimed), closes removed ones,
/// updates drifted titles, and — when any task is flagged — raises the branch's
/// attention so the human is pulled in. Flags themselves change nothing on the
/// in-flight issue.
pub async fn apply(db: &Db, branch: &Branch, slug: &str, plan: &SyncPlan) -> Result<()> {
    for action in &plan.actions {
        match action {
            SyncAction::Create { task, title } => {
                issue::add(
                    db,
                    &NewIssue {
                        repo_root: branch.repo_root.clone(),
                        source_branch: Some(branch.branch.clone()),
                        claimed_branch: None,
                        title: title.clone(),
                        plan_task: Some(format!("{slug}#{task}")),
                        ..Default::default()
                    },
                )
                .await?;
            }
            SyncAction::Close { issue_id, .. } => issue::close(db, *issue_id).await?,
            SyncAction::UpdateTitle {
                issue_id, title, ..
            } => issue::set_title(db, *issue_id, title).await?,
            SyncAction::Flag { .. } => {}
        }
    }

    let flags = plan.flags();
    if flags > 0 {
        crate::tags::set(
            db,
            &branch.id,
            crate::tags::ATTENTION_KEY,
            "attention",
            "",
            "agent",
        )
        .await?;
        crate::branch::set_description(
            db,
            &branch.id,
            &format!("plan {slug}: {flags} in-flight task(s) diverged from the plan"),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
plan: search-rewrite
status: active
---

# Search rewrite

## Problem & goal
Make search fast.

## Architecture
```mermaid
flowchart TD
    a --> b
```

## Tasks

### T1 — Index layer  `exec: session`  `value: high`  `deps: —`
The storage + read path.

### T2 — Executor  `exec: session`  `value: med`  `deps: T1`
Runs the plan.

### T3 — Local scaffolding  `exec: inline`  `value: low`  `deps: T1, T2`
Just do it now.

## Open questions
- single node?
"#;

    #[test]
    fn parses_frontmatter_title_and_tasks() {
        let p = parse("search-rewrite", SAMPLE);
        assert_eq!(p.slug, "search-rewrite");
        assert_eq!(p.title, "Search rewrite");
        assert_eq!(p.status, "active");
        assert_eq!(p.tasks.len(), 3);

        let t1 = &p.tasks[0];
        assert_eq!(t1.id, "T1");
        assert_eq!(t1.title, "Index layer");
        assert_eq!(t1.exec, "session");
        assert_eq!(t1.value, "high");
        assert!(t1.deps.is_empty());
        assert_eq!(t1.body, "The storage + read path.");
        assert!(t1.materializes());

        let t2 = &p.tasks[1];
        assert_eq!(t2.deps, vec!["T1"]);
        assert_eq!(t2.value, "med");

        let t3 = &p.tasks[2];
        assert_eq!(t3.exec, "inline");
        assert_eq!(t3.deps, vec!["T1", "T2"]);
        assert!(!t3.materializes(), "inline tasks do not materialize");
    }

    #[test]
    fn parse_is_tolerant_of_a_bare_file() {
        let p = parse("bare", "just some text\nno structure here");
        assert_eq!(p.title, "bare"); // falls back to slug
        assert_eq!(p.status, "draft");
        assert!(p.tasks.is_empty());
    }

    #[test]
    fn scaffold_round_trips_through_the_parser() {
        let src = scaffold("my-plan", "My Plan", "");
        let p = parse("my-plan", &src);
        assert_eq!(p.title, "My Plan");
        assert_eq!(p.status, "draft");
        assert_eq!(p.tasks.len(), 1);
        assert_eq!(p.tasks[0].id, "T1");
        assert_eq!(p.tasks[0].exec, "session");
    }

    #[test]
    fn scaffold_seeds_problem_from_goal() {
        let with = scaffold("p", "P", "  Rewrite search to use the new index  ");
        assert!(with.contains("Rewrite search to use the new index"));
        assert!(!with.contains("What are we building")); // prompt replaced

        // An empty goal keeps the prompt.
        assert!(scaffold("p", "P", "   ").contains("What are we building"));
    }

    /// An issue linked to `slug#task`, optionally claimed/closed.
    fn linked(id: i64, slug: &str, task: &str, title: &str) -> Issue {
        Issue {
            id,
            repo_root: "/r".into(),
            github_repo: None,
            source_branch: Some("plan".into()),
            claimed_branch: None,
            title: title.into(),
            body: String::new(),
            status: "open".into(),
            github_issue: None,
            plan_task: Some(format!("{slug}#{task}")),
            created_at: String::new(),
            updated_at: String::new(),
            closed_at: None,
        }
    }

    #[test]
    fn diff_creates_closes_and_updates() {
        let p = parse("pl", SAMPLE);
        // T1 already materialized (title matches), T2 title drifted, plus an
        // orphan issue for a deleted T9.
        let mut t2 = linked(2, "pl", "T2", "Old executor name");
        t2.title = "Old executor name".into();
        let issues = vec![
            linked(1, "pl", "T1", "Index layer"),
            t2,
            linked(9, "pl", "T9", "Deleted task"),
        ];
        let d = diff("pl", &p.tasks, &issues);

        // T1: unchanged → no action. T2: title drift, unclaimed → UpdateTitle.
        // T9: gone, unclaimed open → Close. T3 is inline → never created.
        assert!(d.actions.contains(&SyncAction::UpdateTitle {
            task: "T2".into(),
            issue_id: 2,
            title: "Executor".into(),
        }));
        assert!(d.actions.contains(&SyncAction::Close {
            task: "T9".into(),
            issue_id: 9,
        }));
        assert!(!d
            .actions
            .iter()
            .any(|a| matches!(a, SyncAction::Create { task, .. } if task == "T3")));
        assert_eq!(d.flags(), 0);
    }

    #[test]
    fn diff_creates_a_brand_new_materializing_task() {
        let p = parse("pl", SAMPLE);
        let d = diff("pl", &p.tasks, &[]); // nothing materialized yet
        let created: Vec<&str> = d
            .actions
            .iter()
            .filter_map(|a| match a {
                SyncAction::Create { task, .. } => Some(task.as_str()),
                _ => None,
            })
            .collect();
        // T1 and T2 (session), but not T3 (inline).
        assert_eq!(created, vec!["T1", "T2"]);
    }

    #[test]
    fn diff_dedups_a_duplicated_task_id() {
        // A malformed plan with two `T1` headings must not spawn two issues
        // sharing one plan_task key — the first occurrence wins.
        let dup = PlanTask {
            id: "T1".into(),
            title: "First".into(),
            exec: "session".into(),
            value: String::new(),
            deps: vec![],
            body: String::new(),
        };
        let mut dup2 = dup.clone();
        dup2.title = "Second".into();
        let d = diff("pl", &[dup, dup2], &[]);
        let creates = d
            .actions
            .iter()
            .filter(|a| matches!(a, SyncAction::Create { task, .. } if task == "T1"))
            .count();
        assert_eq!(creates, 1, "duplicate id must create exactly one issue");
    }

    #[test]
    fn diff_flags_but_never_rewrites_in_flight_work() {
        let p = parse("pl", SAMPLE);
        // T2's issue is claimed by a live session and its title drifted; a
        // separate claimed issue for a now-deleted task.
        let mut t2 = linked(2, "pl", "T2", "Renamed under the agent");
        t2.claimed_branch = Some("weaver/exec".into());
        let mut gone = linked(7, "pl", "T7", "In-flight orphan");
        gone.claimed_branch = Some("weaver/orphan".into());
        let issues = vec![linked(1, "pl", "T1", "Index layer"), t2, gone];

        let d = diff("pl", &p.tasks, &issues);
        assert_eq!(d.flags(), 2);
        // Neither claimed issue gets an UpdateTitle/Close.
        assert!(!d.actions.iter().any(|a| matches!(
            a,
            SyncAction::UpdateTitle { issue_id, .. } | SyncAction::Close { issue_id, .. }
            if *issue_id == 2 || *issue_id == 7
        )));
    }

    #[tokio::test]
    async fn apply_materializes_and_is_idempotent() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = crate::branch::upsert(&db, "/r", "weaver/plan", "main")
            .await
            .unwrap();
        let p = parse("pl", SAMPLE);

        // First sync: create the two materializing tasks.
        let issues = issue::list_for_plan(&db, "/r", "pl", true).await.unwrap();
        let d = diff("pl", &p.tasks, &issues);
        apply(&db, &branch, "pl", &d).await.unwrap();
        let after = issue::list_for_plan(&db, "/r", "pl", true).await.unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|i| i.claimed_branch.is_none())); // backlog

        // Second sync over the now-materialized ledger: no further actions.
        let d2 = diff("pl", &p.tasks, &after);
        assert!(d2.is_empty(), "reconcile should converge");
    }

    #[tokio::test]
    async fn apply_raises_attention_on_flags() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = crate::branch::upsert(&db, "/r", "weaver/plan", "main")
            .await
            .unwrap();
        // A claimed issue for a task whose title drifted → a flag on apply.
        let i = issue::add(
            &db,
            &NewIssue {
                repo_root: "/r".into(),
                claimed_branch: Some("weaver/exec".into()),
                title: "stale title".into(),
                plan_task: Some("pl#T1".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let p = parse("pl", SAMPLE);
        let issues = vec![i];
        let d = diff("pl", &p.tasks, &issues);
        assert_eq!(d.flags(), 1);
        apply(&db, &branch, "pl", &d).await.unwrap();

        // Flagging raises the agent's attention tag and records the reason in the
        // branch description.
        let tag = crate::tags::get(&db, &branch.id, crate::tags::ATTENTION_KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tag.value, "attention");
        let refreshed = crate::branch::get(&db, &branch.id).await.unwrap().unwrap();
        assert!(refreshed.description.contains("diverged from the plan"));
    }
}
