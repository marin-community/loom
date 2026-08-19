use std::path::PathBuf;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::operations::issues as issue_operations;
use weaver_api::{
    IssueAction, IssueActionProblem as IssueActionProblemView, IssueActionsResult, IssueView,
    PatchIssueReq,
};
use weaver_core::branch as branch_mod;
use weaver_core::issue::{BulkIssueAction, Issue};

use crate::db::Db;
use crate::events;
use crate::git;
use crate::{auth::Grant, auth::Principal};

use super::operations::{register, Bound, OperationContext};
use super::{author_or_manual, require_branch};
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

/// Every `issues.*` operation, bound to the code that serves it.
///
/// One line per operation, in the order the ids sort. `assert_registry_is_complete`
/// fails startup if a declared operation is missing from this list, so the
/// bundle cannot drift back into the state where `issues.actions` was declared
/// and served while three other operations were declared and were not.
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![
        register::<issue_operations::actions::Actions, _, _>(issue_actions_operation),
        register::<issue_operations::backlog::create::Create, _, _>(create_repo_issue_operation),
        register::<issue_operations::close::Close, _, _>(close_issue_operation),
        register::<issue_operations::create::Create, _, _>(create_branch_issue_operation),
        register::<issue_operations::delete::Delete, _, _>(delete_issue_operation),
        register::<issue_operations::get::Get, _, _>(get_issue_operation),
        register::<issue_operations::list::List, _, _>(list_repo_issues_operation),
        register::<issue_operations::reopen::Reopen, _, _>(reopen_issue_operation),
        register::<issue_operations::tags::delete::Delete, _, _>(clear_issue_tag_operation),
        register::<issue_operations::tags::set::Set, _, _>(set_issue_tag_operation),
    ]
}

#[derive(Debug, Deserialize)]
pub(super) struct IssueListQuery {
    #[serde(default)]
    all: bool,
}

/// Query for the cross-repo board: `all` as above, plus the automation opt-in
/// mirroring `GET /api/sessions?automation=true`.
#[derive(Debug, Deserialize)]
pub(super) struct AllIssuesQuery {
    #[serde(default)]
    all: bool,
    /// Include issues claimed by an automation-class session's branch. Defaults
    /// to `false` — the board shows the work of the interactive fleet, not the
    /// trackers its machinery opens for itself.
    #[serde(default)]
    automation: bool,
}

/// Build an [`IssueView`] for an issue, gathering its tags (a separate query).
async fn issue_view(db: &Db, issue: Issue) -> ApiResult<IssueView> {
    let tags = weaver_core::issue::list_tags(db, issue.id).await?;
    Ok(IssueView::from_parts(issue, &tags))
}

/// Build views for a batch of issues, each with its tags joined.
pub(super) async fn issue_views(db: &Db, issues: Vec<Issue>) -> ApiResult<Vec<IssueView>> {
    let mut out = Vec::with_capacity(issues.len());
    for i in issues {
        out.push(issue_view(db, i).await?);
    }
    Ok(out)
}

/// Every issue across every repo — the loom dashboard's cross-repo issue board.
pub(super) async fn list_all_issues(
    State(st): State<AppState>,
    Query(q): Query<AllIssuesQuery>,
) -> ApiResult<Json<Vec<IssueView>>> {
    let mut issues = weaver_core::issue::list_all(&st.db, q.all).await?;
    if !q.automation {
        // Branches whose current claim-holder is an automation-class session,
        // as (repo_root, branch) pairs — issues key their claim by branch name,
        // not branch id. Archived sessions never own work, including historical
        // rows whose claims predate archive cleanup.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT b.repo_root, b.branch FROM sessions s
             JOIN branches b ON b.id = s.branch_id
             WHERE s.class = 'automation'
               AND s.status != 'archived'
               AND s.id = (
                   SELECT s2.id FROM sessions s2
                   WHERE s2.branch_id = s.branch_id
                     AND s2.status != 'archived'
                   ORDER BY (s2.status NOT IN ('done', 'error')) DESC,
                            s2.created_at DESC
                   LIMIT 1
               )",
        )
        .fetch_all(&st.db)
        .await?;
        let hidden: std::collections::HashSet<(String, String)> = rows.into_iter().collect();
        issues.retain(|i| match &i.claimed_branch {
            Some(claimed) => !hidden.contains(&(i.repo_root.clone(), claimed.clone())),
            None => true,
        });
    }
    Ok(Json(issue_views(&st.db, issues).await?))
}

/// Issues claimed by this branch — the session's working set.
pub(super) async fn list_branch_issues(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<IssueListQuery>,
) -> ApiResult<Json<Vec<IssueView>>> {
    let branch = require_branch(&st.db, &key).await?;
    let issues =
        weaver_core::issue::list_for_branch(&st.db, &branch.repo_root, &branch.branch, q.all)
            .await?;
    Ok(Json(issue_views(&st.db, issues).await?))
}

/// Create an issue claimed by this branch.
pub(super) async fn create_branch_issue_operation(
    context: OperationContext,
    input: issue_operations::create::Input,
) -> ApiResult<IssueView> {
    let st = context.state;
    if input.title.trim().is_empty() {
        return Err(AppError::bad_request("issue title is required"));
    }
    // The old request carried a `tags` list that both callers always sent
    // empty; a new issue starts untagged and `issues.tags.set` labels it.
    let tags = Vec::new();
    let branch = require_branch(&st.db, &input.branch).await?;
    let issue = weaver_core::issue::add_with_tags(
        &st.db,
        &weaver_core::issue::NewIssue {
            repo_root: branch.repo_root.clone(),
            source_branch: Some(branch.branch.clone()),
            claimed_branch: Some(branch.branch.clone()),
            title: input.title.trim().to_string(),
            body: input.body,
            github_issue: input.github_issue,
            ..Default::default()
        },
        &tags,
    )
    .await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "issue_added",
        json!({ "id": issue.id, "title": issue.title }),
    )
    .await
    .ok();
    issue_view(&st.db, issue).await
}

/// Resolve the branch row an issue event should be attributed to: the branch
/// currently working it, else the branch it came from. `None` for a pure
/// repo-level backlog item (no session feed to notify).
async fn issue_event_branch(db: &Db, issue: &Issue) -> Option<String> {
    let name = issue
        .claimed_branch
        .as_deref()
        .or(issue.source_branch.as_deref())?;
    let branch = branch_mod::find_by_repo_branch(db, &issue.repo_root, name)
        .await
        .ok()
        .flatten()?;
    Some(branch.id)
}

async fn record_issue_event(st: &AppState, issue: &Issue, kind: &str, payload: Value) {
    if let Some(branch_id) = issue_event_branch(&st.db, issue).await {
        events::record(&st.db, &st.bus, &branch_id, kind, payload)
            .await
            .ok();
    }
}

async fn change_issue_status(st: &AppState, issue: &Issue, status: &str) -> ApiResult<()> {
    let kind = match status {
        "open" => {
            weaver_core::issue::reopen(&st.db, issue.id).await?;
            "issue_reopened"
        }
        "closed" => {
            weaver_core::issue::close(&st.db, issue.id).await?;
            "issue_closed"
        }
        other => {
            return Err(AppError::bad_request(format!(
                "invalid status '{other}' (expected 'open' or 'closed')"
            )))
        }
    };
    tracing::info!(issue = issue.id, status, "issue status changed");
    record_issue_event(st, issue, kind, json!({ "id": issue.id })).await;
    Ok(())
}

pub(super) async fn get_issue_operation(
    context: OperationContext,
    input: issue_operations::get::Input,
) -> ApiResult<IssueView> {
    let st = context.state;
    let id = input.id;
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    // The declared scope is `Repository(repo_root)`, but this operation is
    // addressed by *issue id* — so the repository that matters is the issue's,
    // not the one the caller named. Checking only the latter would let a session
    // read another repository's work item by asking for it with its own
    // `repo_root`. `issues.actions` already checks the same way.
    require_repo_access(&st, &context.principal, &issue.repo_root).await?;
    let mut view = issue_view(&st.db, issue).await?;
    // Best-effort live snapshot of the linked GitHub thread, so `loom issues
    // get` surfaces "closed / re-titled while you worked". Single-issue reads
    // only (lists would fan out), bounded so a slow GitHub can't hang the CLI,
    // and a failure just leaves the field absent — the ledger still stands.
    if let (Some(repo), Some(number)) = (view.github_repo.clone(), view.github_issue) {
        view.github_state = tokio::time::timeout(
            std::time::Duration::from_secs(4),
            st.trigger.gh().issue_state(&repo, number),
        )
        .await
        .ok()
        .and_then(|r| {
            r.map_err(|e| tracing::debug!(repo, number, error = %e, "live issue state unavailable"))
                .ok()
        })
        .map(|s| weaver_api::GithubThreadState {
            state: s.state,
            title: s.title,
            updated_at: s.updated_at,
        });
    }
    Ok(view)
}

pub(super) async fn patch_issue(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PatchIssueReq>,
) -> ApiResult<Json<IssueView>> {
    let existing = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    if req
        .claimed_branch
        .as_ref()
        .and_then(|branch| branch.as_deref())
        .is_some_and(|branch| !branch.trim().is_empty())
    {
        return Err(AppError::bad_request(
            "claimed_branch can only be cleared; launch a session to claim an issue",
        ));
    }
    if let Some(status) = req.status.as_deref() {
        change_issue_status(&st, &existing, status).await?;
    }
    if req.title.is_some() || req.body.is_some() {
        let new_title = req.title.as_deref().unwrap_or(&existing.title);
        let new_body = req.body.as_deref().unwrap_or(&existing.body);
        sqlx::query("UPDATE issues SET title = ?, body = ?, updated_at = ? WHERE id = ?")
            .bind(new_title)
            .bind(new_body)
            .bind(weaver_core::db::now_iso())
            .bind(id)
            .execute(&st.db)
            .await?;
        tracing::info!(issue = id, "issue updated");
    }
    if let Some(mapping) = req.github.as_deref() {
        let mapping = mapping.trim();
        let parsed = if mapping.is_empty() {
            None
        } else {
            Some(crate::github::parse_wiring(mapping).ok_or_else(|| {
                AppError::bad_request(format!(
                    "invalid GitHub issue mapping '{mapping}' — expected owner/name#number"
                ))
            })?)
        };
        let (repo, number) = parsed
            .map(|(repo, number)| (Some(repo), Some(number)))
            .unwrap_or((None, None));
        sqlx::query(
            "UPDATE issues SET github_repo = ?, github_issue = ?, updated_at = ? WHERE id = ?",
        )
        .bind(repo)
        .bind(number)
        .bind(weaver_core::db::now_iso())
        .bind(id)
        .execute(&st.db)
        .await?;
        tracing::info!(issue = id, github = mapping, "issue GitHub mapping changed");
    }
    if req.claimed_branch.is_some() {
        weaver_core::issue::set_claim(&st.db, id, None).await?;
    }
    let issue = weaver_core::issue::get(&st.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("issue"))?;
    Ok(Json(issue_view(&st.db, issue).await?))
}

/// Set (upsert) a free-form label on an issue. Issue tags carry no loud
/// `attention`/`triage` ladder — every key is a quiet annotation, so the only
/// rule is a non-empty value (clear the tag with `DELETE` to remove a label). A
/// `tag` event is recorded on the branch working the issue, when there is one,
/// so its session feed refreshes.
pub(super) async fn set_issue_tag_operation(
    context: OperationContext,
    input: issue_operations::tags::set::Input,
) -> ApiResult<IssueView> {
    let id = input.id;
    let tag_key = input.key.trim().to_string();
    // `by` is derived from the credential, not taken from the body. The old
    // request let a caller name whoever it liked as the setter, and the tag is
    // shown in the dashboard as provenance. The two values are the ones both
    // callers already wrote: the dashboard sent `manual`, the MCP tool `agent`.
    let by = Some(
        if context.principal.is_human() {
            "manual"
        } else {
            "agent"
        }
        .to_string(),
    );
    let repo_root = input.repo_root.clone();
    let result = issue_actions_operation(
        context,
        issue_operations::actions::Input {
            ids: vec![id],
            action: IssueAction::Tag {
                key: tag_key,
                value: input.value,
                note: input.note,
                by,
            },
            repo_root,
        },
    )
    .await?;
    result
        .issues
        .into_iter()
        .next()
        .ok_or_else(|| AppError::not_found("issue"))
}

/// Clear a label on an issue through the same validated semantic operation as
/// MCP and bulk CLI calls. A missing tag is reported as a conflict.
pub(super) async fn clear_issue_tag_operation(
    context: OperationContext,
    input: issue_operations::tags::delete::Input,
) -> ApiResult<IssueView> {
    let id = input.id;
    let repo_root = input.repo_root.clone();
    let result = issue_actions_operation(
        context,
        issue_operations::actions::Input {
            ids: vec![id],
            action: IssueAction::Untag {
                key: input.key.trim().to_string(),
            },
            repo_root,
        },
    )
    .await?;
    result
        .issues
        .into_iter()
        .next()
        .ok_or_else(|| AppError::not_found("issue"))
}

pub(super) async fn delete_issue_operation(
    context: OperationContext,
    input: issue_operations::delete::Input,
) -> ApiResult<IssueActionsResult> {
    issue_actions_operation(
        context,
        issue_operations::actions::Input {
            ids: input.ids,
            action: IssueAction::Delete,
            repo_root: input.repo_root,
        },
    )
    .await
}

fn validate_issue_action(action: &IssueAction) -> ApiResult<()> {
    let (key, value) = match action {
        IssueAction::Tag { key, value, .. } => (Some(key.trim()), Some(value.trim())),
        IssueAction::Untag { key } => (Some(key.trim()), None),
        _ => (None, None),
    };
    if key.is_some_and(str::is_empty) {
        return Err(AppError::bad_request("tag key is required"));
    }
    if value.is_some_and(str::is_empty) {
        return Err(AppError::bad_request("tag value must be non-empty"));
    }
    Ok(())
}

fn domain_issue_action(action: &IssueAction) -> BulkIssueAction {
    match action {
        IssueAction::Close => BulkIssueAction::Close,
        IssueAction::Reopen => BulkIssueAction::Reopen,
        IssueAction::Tag {
            key,
            value,
            note,
            by,
        } => BulkIssueAction::Tag {
            key: key.trim().to_string(),
            value: value.trim().to_string(),
            note: note.trim().to_string(),
            set_by: author_or_manual(by.as_deref()),
        },
        IssueAction::Untag { key } => BulkIssueAction::Untag {
            key: key.trim().to_string(),
        },
        IssueAction::Delete => BulkIssueAction::Delete,
    }
}

/// Validate an issue command and every target before applying the whole batch
/// in one transaction. Invalid IDs and preconditions are returned together in
/// structured error details; no requested issue changes.
pub(super) async fn issue_actions_operation(
    context: OperationContext,
    req: issue_operations::actions::Input,
) -> ApiResult<IssueActionsResult> {
    let st = context.state;
    let principal = context.principal;
    if req.ids.is_empty() {
        return Err(AppError::bad_request("at least one issue id is required"));
    }
    let mut seen = std::collections::HashSet::new();
    if req.ids.iter().any(|id| !seen.insert(*id)) {
        return Err(AppError::bad_request("issue ids must be unique"));
    }
    validate_issue_action(&req.action)?;
    if matches!(principal.grant, Grant::Session { .. }) {
        for id in &req.ids {
            if let Some(issue) = weaver_core::issue::get(&st.db, *id).await? {
                require_repo_access(&st, &principal, &issue.repo_root).await?;
            }
        }
    }

    let action = domain_issue_action(&req.action);
    let result = match weaver_core::issue::apply_bulk_action(&st.db, &req.ids, &action).await {
        Ok(result) => result,
        Err(error) => {
            if let Some(validation) =
                error.downcast_ref::<weaver_core::issue::IssueActionValidationError>()
            {
                let problems: Vec<IssueActionProblemView> = validation
                    .problems
                    .iter()
                    .map(|problem| IssueActionProblemView {
                        id: problem.id,
                        code: problem.code.clone(),
                        error: problem.error.clone(),
                    })
                    .collect();
                let summary = problems
                    .iter()
                    .map(|problem| format!("#{} {}", problem.id, problem.error))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(AppError::conflict(format!("{} — {summary}", validation))
                    .with_details(json!({
                        "problems": problems,
                    })));
            }
            return Err(error.into());
        }
    };

    let event = match &req.action {
        IssueAction::Close => Some(("issue_closed", json!({}))),
        IssueAction::Reopen => Some(("issue_reopened", json!({}))),
        IssueAction::Tag { key, value, .. } => Some((
            "issue_tagged",
            json!({ "key": key.trim(), "value": value.trim() }),
        )),
        IssueAction::Untag { key } => {
            Some(("issue_tagged", json!({ "key": key.trim(), "value": "" })))
        }
        IssueAction::Delete => None,
    };
    if let Some((kind, fields)) = event {
        for issue in &result.issues {
            let mut payload = fields.clone();
            payload["id"] = json!(issue.id);
            record_issue_event(&st, issue, kind, payload).await;
        }
    }
    Ok(IssueActionsResult {
        issues: issue_views(&st.db, result.issues).await?,
        deleted_ids: result.deleted_ids,
    })
}

pub(super) async fn close_issue_operation(
    context: OperationContext,
    input: issue_operations::close::Input,
) -> ApiResult<IssueActionsResult> {
    issue_actions_operation(
        context,
        issue_operations::actions::Input {
            ids: input.ids,
            action: IssueAction::Close,
            repo_root: input.repo_root,
        },
    )
    .await
}

pub(super) async fn reopen_issue_operation(
    context: OperationContext,
    input: issue_operations::reopen::Input,
) -> ApiResult<IssueActionsResult> {
    issue_actions_operation(
        context,
        issue_operations::actions::Input {
            ids: input.ids,
            action: IssueAction::Reopen,
            repo_root: input.repo_root,
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// Repo-scoped issues (the backlog / board surface)
// ---------------------------------------------------------------------------

/// Resolve a repo identity from an explicit `repo_root` or, failing that, a
/// `cwd` — canonicalized to match how issues are keyed.
pub(crate) async fn resolve_repo_root(
    repo_root: Option<&str>,
    cwd: Option<&str>,
) -> ApiResult<String> {
    if let Some(rr) = repo_root.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(rr.to_string());
    }
    let cwd = cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("repo_root or cwd is required"))?;
    let root = git::repo_root(&PathBuf::from(cwd))
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(root.canonicalize().unwrap_or(root).display().to_string())
}

async fn require_repo_access(
    st: &AppState,
    principal: &Principal,
    repo_root: &str,
) -> ApiResult<()> {
    let Grant::Session { branch_id, .. } = &principal.grant else {
        return Ok(());
    };
    let own_repo: Option<String> =
        sqlx::query_scalar("SELECT repo_root FROM branches WHERE id = ?")
            .bind(branch_id)
            .fetch_optional(&st.db)
            .await?;
    if own_repo.as_deref() == Some(repo_root) {
        Ok(())
    } else {
        Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "session credentials are limited to their repository",
        ))
    }
}

/// The repo-wide issue board, or just the unclaimed backlog with `backlog: true`.
///
/// The repo-access check the old handler ran inline is gone: `authorize()` does
/// it once from `Scoped`, before this is ever called.
pub(super) async fn list_repo_issues_operation(
    context: OperationContext,
    input: issue_operations::list::Input,
) -> ApiResult<Vec<IssueView>> {
    let st = context.state;
    let repo_root = resolve_repo_root(Some(&input.repo_root), None).await?;
    let issues = if input.backlog {
        weaver_core::issue::list_backlog(&st.db, &repo_root, input.all).await?
    } else {
        weaver_core::issue::list_for_repo(&st.db, &repo_root, input.all).await?
    };
    issue_views(&st.db, issues).await
}

/// Create an unclaimed repo-level backlog item.
pub(super) async fn create_repo_issue_operation(
    context: OperationContext,
    req: issue_operations::backlog::create::Input,
) -> ApiResult<IssueView> {
    let st = context.state;
    if req.title.trim().is_empty() {
        return Err(AppError::bad_request("issue title is required"));
    }
    if req.repo_root.trim().is_empty() {
        return Err(AppError::bad_request("repo_root is required"));
    }
    // No inline repo-access check: `authorize()` already ran it from `Scoped`.
    let tags = Vec::new();
    let issue = weaver_core::issue::add_with_tags(
        &st.db,
        &weaver_core::issue::NewIssue {
            repo_root: req.repo_root.clone(),
            source_branch: req.source_branch.clone(),
            title: req.title.trim().to_string(),
            body: req.body.clone(),
            github_issue: req.github_issue,
            ..Default::default()
        },
        &tags,
    )
    .await?;
    // Attribute the add to the filing branch, when there is one, so its
    // session feed refreshes — the same notification a claimed issue's
    // `create_branch_issue` already gives.
    if let Some(source) = req.source_branch.as_deref() {
        if let Some(branch) = branch_mod::find_by_repo_branch(&st.db, &req.repo_root, source)
            .await
            .ok()
            .flatten()
        {
            events::record(
                &st.db,
                &st.bus,
                &branch.id,
                "issue_added",
                json!({ "id": issue.id, "title": issue.title }),
            )
            .await
            .ok();
        }
    }
    issue_view(&st.db, issue).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn test_state(db: Db) -> AppState {
        AppState {
            ctx: crate::Ctx {
                db: db.clone(),
                bus: crate::events::EventBus::new(),
                addr: "127.0.0.1:0".to_string(),
            },
            ide: std::sync::Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::production(db),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        }
    }

    fn admin_principal() -> Principal {
        Principal {
            username: "test".to_string(),
            github_login: None,
            via: crate::auth::AuthVia::Loopback,
            grant: Grant::Admin,
            automation_context: None,
        }
    }

    #[tokio::test]
    async fn create_repo_issue_attributes_source_branch_and_records_an_event() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let branch = branch_mod::upsert(&db, "/r", "weaver/a", "main")
            .await
            .unwrap();

        let issue = create_repo_issue_operation(
            OperationContext::new(st, admin_principal()),
            issue_operations::backlog::create::Input {
                repo_root: "/r".to_string(),
                title: "backlog item".to_string(),
                body: String::new(),
                github_issue: None,
                // The branch NAME, which is what `source_branch` stores and what
                // the CLI compares against — see `ContextSource::BranchName`.
                source_branch: Some("weaver/a".to_string()),
            },
        )
        .await
        .unwrap();
        assert_eq!(issue.claimed_branch, None, "still unclaimed");
        assert_eq!(issue.source_branch.as_deref(), Some("weaver/a"));

        let events = events::history(&db, &branch.id, 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "issue_added");
    }

    #[tokio::test]
    async fn patch_issue_changes_and_clears_github_mapping() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let issue = weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: "/r".to_string(),
                title: "mapped".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let mapped = patch_issue(
            State(st.clone()),
            Path(issue.id),
            Json(PatchIssueReq {
                github: Some("acme/widgets#17".to_string()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(mapped.github_repo.as_deref(), Some("acme/widgets"));
        assert_eq!(mapped.github_issue, Some(17));

        let cleared = patch_issue(
            State(st),
            Path(issue.id),
            Json(PatchIssueReq {
                github: Some(String::new()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(cleared.github_repo, None);
        assert_eq!(cleared.github_issue, None);
    }

    #[tokio::test]
    async fn patch_issue_clears_claim_but_cannot_assign_one() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let issue = weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: "/r".to_string(),
                claimed_branch: Some("weaver/worker".to_string()),
                title: "claimed".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let cleared = patch_issue(
            State(st.clone()),
            Path(issue.id),
            Json(PatchIssueReq {
                claimed_branch: Some(None),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(cleared.claimed_branch, None);

        let err = patch_issue(
            State(st),
            Path(issue.id),
            Json(PatchIssueReq {
                claimed_branch: Some(Some("weaver/other".to_string())),
                ..Default::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn issue_actions_returns_one_aggregate_success() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let first = weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: "/r".to_string(),
                title: "first".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let second = weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: "/r".to_string(),
                title: "second".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let result = issue_actions_operation(
            OperationContext::new(st, admin_principal()),
            issue_operations::actions::Input {
                ids: vec![first.id, second.id],
                action: IssueAction::Close,
                repo_root: "/r".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.issues.len(), 2);
        assert!(result.issues.iter().all(|issue| issue.status == "closed"));
        assert!(result.deleted_ids.is_empty());
    }

    #[tokio::test]
    async fn scalar_status_operations_share_atomic_validation() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let issue = weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: "/r".to_string(),
                title: "one item".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let closed = close_issue_operation(
            OperationContext::new(st.clone(), admin_principal()),
            issue_operations::close::Input {
                ids: vec![issue.id],
                repo_root: "/r".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(closed.issues[0].status, "closed");

        let duplicate = close_issue_operation(
            OperationContext::new(st.clone(), admin_principal()),
            issue_operations::close::Input {
                ids: vec![issue.id],
                repo_root: "/r".to_string(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(duplicate.status(), axum::http::StatusCode::CONFLICT);

        let reopened = reopen_issue_operation(
            OperationContext::new(st, admin_principal()),
            issue_operations::reopen::Input {
                ids: vec![issue.id],
                repo_root: "/r".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(reopened.issues[0].status, "open");
    }

    #[tokio::test]
    async fn issue_actions_reports_invalid_ids_and_commits_nothing() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let st = test_state(db.clone());
        let issue = weaver_core::issue::add(
            &db,
            &weaver_core::issue::NewIssue {
                repo_root: "/r".to_string(),
                title: "must stay open".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let error = issue_actions_operation(
            OperationContext::new(st, admin_principal()),
            issue_operations::actions::Input {
                ids: vec![issue.id, 999_999],
                action: IssueAction::Close,
                repo_root: "/r".to_string(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
        assert_eq!(
            error.details.as_ref().unwrap()["problems"][0],
            json!({
                "id": 999_999,
                "code": "not_found",
                "error": "issue not found",
            })
        );
        assert_eq!(
            weaver_core::issue::get(&db, issue.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "open"
        );
    }
}
