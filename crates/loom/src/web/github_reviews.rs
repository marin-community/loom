//! Dispatch automatic reviews from signed GitHub App deliveries.

use std::{sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tokio::task::JoinSet;
use weaver_api::{operations::runs::create::Input, RunView};
use weaver_core::{
    config::{
        self, GithubReviewPolicy, GithubReviewRoute, DEFAULT_GITHUB_REVIEW_ROUTES,
        GITHUB_REVIEW_ROUTES_KEY,
    },
    db::Db,
};

use crate::{
    automation::{federation_get, github_automation_subject},
    github_app::{GithubApp, ReviewHead, ReviewLabel, ReviewPullRequest, ReviewRepository},
    repo::{parse_slug, RepoSlug},
};

use super::{
    automation::{create_run_core, RunCaller},
    AppState,
};

const LINT_LABEL: &str = "agentic-lint";
const MAX_IN_FLIGHT: usize = 8;
const MAX_ATTEMPTS: i64 = 5;
// Exceed the automation reservation's five-minute stale interval so a
// reclaimed delivery can resume a run interrupted by a daemon restart.
const LEASE_SECONDS: i64 = 330;

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    Opened,
    ReadyForReview,
    Reopened,
    Synchronize,
    Unlabeled,
    #[serde(other)]
    Other,
}

impl Action {
    fn initial(self) -> bool {
        matches!(self, Self::Opened | Self::ReadyForReview | Self::Reopened)
    }
}

#[derive(Deserialize, Serialize)]
struct Delivery {
    repository_id: i64,
    repository: String,
    number: i64,
    action: Action,
    removed_label: Option<String>,
}

impl Delivery {
    fn matches(&self, route: &GithubReviewRoute) -> bool {
        route.repository_id == self.repository_id
            && (self.action.initial()
                || match route.policy {
                    GithubReviewPolicy::Correctness => false,
                    GithubReviewPolicy::Holistic { .. } => self.action == Action::Synchronize,
                    GithubReviewPolicy::AgenticLint => {
                        self.action == Action::Unlabeled
                            && self.removed_label.as_deref() == Some(LINT_LABEL)
                    }
                })
    }
}

async fn routes(db: &Db) -> Result<Vec<GithubReviewRoute>> {
    let value = config::try_get(db, GITHUB_REVIEW_ROUTES_KEY).await?;
    config::parse_github_review_routes(value.as_deref().unwrap_or(DEFAULT_GITHUB_REVIEW_ROUTES))
        .map_err(anyhow::Error::msg)
}

/// Keep only the data needed to resolve the current PR. GitHub content such as
/// titles, descriptions and comments never becomes part of the launch goal.
pub(super) async fn enqueue(db: &Db, id: &str, body: &[u8]) -> Result<bool> {
    #[derive(Deserialize)]
    struct Payload {
        repository: ReviewRepository,
        number: i64,
        action: Action,
        label: Option<ReviewLabel>,
        pull_request: Pull,
    }
    #[derive(Deserialize)]
    struct Pull {
        draft: bool,
        head: ReviewHead,
    }
    let payload: Payload = match serde_json::from_slice(body) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(delivery = id, %error, "invalid pull request webhook");
            return Ok(false);
        }
    };
    if payload.pull_request.draft
        || !payload
            .pull_request
            .head
            .repo
            .is_some_and(|repo| repo.id == payload.repository.id)
    {
        return Ok(false);
    }
    let delivery = Delivery {
        repository_id: payload.repository.id,
        repository: payload.repository.full_name,
        number: payload.number,
        action: payload.action,
        removed_label: payload.label.map(|label| label.name),
    };
    if delivery.number <= 0
        || !routes(db)
            .await?
            .iter()
            .any(|route| delivery.matches(route))
    {
        return Ok(false);
    }
    Ok(sqlx::query(
        "INSERT INTO processed_deliveries (delivery_id, review_payload) VALUES (?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(serde_json::to_string(&delivery)?)
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

#[derive(FromRow)]
struct PendingReview {
    delivery_id: String,
    review_payload: String,
    review_attempt: i64,
}

async fn claim(db: &Db) -> Result<Option<PendingReview>> {
    Ok(sqlx::query_as(
        "UPDATE processed_deliveries
         SET review_attempt = review_attempt + 1, review_retry_at = unixepoch() + ?
         WHERE delivery_id = (
             SELECT delivery_id FROM processed_deliveries
             WHERE review_payload IS NOT NULL AND review_retry_at <= unixepoch()
             ORDER BY review_retry_at, received_at, delivery_id LIMIT 1
         )
         RETURNING delivery_id, review_payload, review_attempt",
    )
    .bind(LEASE_SECONDS)
    .fetch_optional(db)
    .await?)
}

pub(crate) async fn run(state: AppState) {
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    let mut active = JoinSet::new();
    loop {
        tokio::select! {
            Some(result) = active.join_next(), if !active.is_empty() => {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => tracing::warn!(%error, "could not record GitHub review delivery outcome"),
                    Err(error) => tracing::error!(%error, "GitHub review delivery task stopped"),
                }
            }
            _ = timer.tick(), if active.len() < MAX_IN_FLIGHT => {
                match claim(&state.db).await {
                    Ok(Some(pending)) => {
                        let state = state.clone();
                        active.spawn(async move { process(&state, pending).await });
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!(%error, "could not claim GitHub review delivery"),
                }
            }
        }
    }
}

async fn process(state: &AppState, pending: PendingReview) -> Result<()> {
    let result = tokio::time::timeout(
        Duration::from_secs((LEASE_SECONDS - 30) as u64),
        dispatch(state, &pending.review_payload),
    )
    .await
    .context("GitHub review dispatch timed out")
    .and_then(|result| result);
    let retry = result.is_err() && pending.review_attempt < MAX_ATTEMPTS;
    if let Err(error) = result {
        tracing::warn!(delivery = pending.delivery_id, attempt = pending.review_attempt, retry, %error, "GitHub review dispatch failed");
    }
    sqlx::query(
        "UPDATE processed_deliveries
         SET review_payload = ?, review_retry_at = unixepoch() + ?
         WHERE delivery_id = ? AND review_attempt = ?",
    )
    .bind(retry.then_some(&pending.review_payload))
    .bind(10 * pending.review_attempt)
    .bind(&pending.delivery_id)
    .bind(pending.review_attempt)
    .execute(&state.db)
    .await?;
    Ok(())
}

fn reviewable(pull: &ReviewPullRequest, delivery: &Delivery) -> bool {
    let same_repo = |repo: &ReviewRepository| {
        repo.id == delivery.repository_id
            && repo.full_name.eq_ignore_ascii_case(&delivery.repository)
    };
    pull.state == "open"
        && !pull.draft
        && pull.base.repo.as_ref().is_some_and(same_repo)
        && pull.head.repo.as_ref().is_some_and(same_repo)
        && pull.head.sha.len() == 40
        && pull.head.sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn dispatch(state: &AppState, payload: &str) -> Result<()> {
    let delivery: Delivery = serde_json::from_str(payload)?;
    let routes: Vec<_> = routes(&state.db)
        .await?
        .into_iter()
        .filter(|route| delivery.matches(route))
        .collect();
    if routes.is_empty() {
        return Ok(());
    }
    let slug = parse_slug(&delivery.repository).map_err(anyhow::Error::msg)?;
    let app = state.trigger.app().context("GitHub App is unavailable")?;
    let pull = app.review_pull_request(&slug, delivery.number).await?;
    if !reviewable(&pull, &delivery) {
        return Ok(());
    }
    let is_bot = pull.user.kind == "Bot";
    let routes: Vec<_> = routes
        .into_iter()
        .filter(|route| {
            !is_bot
                || route
                    .allowed_bots
                    .iter()
                    .any(|login| login == &pull.user.login)
        })
        .collect();
    if routes.is_empty() || (!is_bot && !app.review_author_can_write(&slug, &pull.user).await?) {
        return Ok(());
    }

    // Resolve all applicable profiles independently so a slow or misconfigured
    // reviewer does not delay the other review lanes for this PR.
    let delivery = Arc::new(delivery);
    let pull = Arc::new(pull);
    let mut launches = JoinSet::new();
    for route in routes {
        let state = state.clone();
        let delivery = delivery.clone();
        let pull = pull.clone();
        let slug = slug.clone();
        launches.spawn(async move {
            dispatch_route(&state, &slug, &delivery, &pull, &route)
                .await
                .with_context(|| format!("review route {}", route.federation))
        });
    }
    let mut failed = Vec::new();
    while let Some(result) = launches.join_next().await {
        if let Err(error) = result
            .context("review route stopped")
            .and_then(|result| result)
        {
            failed.push(format!("{error:#}"));
        }
    }
    if !failed.is_empty() {
        bail!("{}", failed.join("; "));
    }
    Ok(())
}

fn abstraction_in_patch(paths: &[String], filename: &str, patch: &str) -> bool {
    let source_path = paths.iter().any(|path| {
        let mut components = filename.split('/');
        path.split('/').all(|part| {
            components
                .next()
                .is_some_and(|name| part == "*" || part == name)
        }) && components.next().is_some()
    });
    source_path
        && patch.lines().any(|line| {
            line.strip_prefix('+')
                .map(str::trim_start)
                .is_some_and(|line| line.starts_with("class ") || line.starts_with("@dataclass"))
        })
}

async fn qualifies(
    app: &GithubApp,
    slug: &RepoSlug,
    delivery: &Delivery,
    pull: &ReviewPullRequest,
    policy: &GithubReviewPolicy,
) -> Result<bool> {
    match policy {
        GithubReviewPolicy::Correctness => Ok(true),
        GithubReviewPolicy::AgenticLint => {
            Ok(!pull.labels.iter().any(|label| label.name == LINT_LABEL))
        }
        GithubReviewPolicy::Holistic {
            changed_lines,
            push_changed_lines,
            abstraction_paths,
        } => {
            if delivery.action == Action::Synchronize
                && app.review_head_changed_lines(slug, &pull.head.sha).await? <= *push_changed_lines
            {
                return Ok(false);
            }
            if pull.additions + pull.deletions >= *changed_lines {
                return Ok(true);
            }
            if abstraction_paths.is_empty() {
                return Ok(false);
            }
            // The GitHub files endpoint returns at most 3,000 files.
            for page in 1..=30 {
                let files = app.review_file_page(slug, delivery.number, page).await?;
                if files.iter().any(|file| {
                    abstraction_in_patch(
                        abstraction_paths,
                        &file.filename,
                        file.patch.as_deref().unwrap_or_default(),
                    )
                }) {
                    return Ok(true);
                }
                if files.len() < 100 {
                    break;
                }
            }
            Ok(false)
        }
    }
}

fn review_run(
    delivery: &Delivery,
    sha: &str,
    policy: &GithubReviewPolicy,
    profile: String,
) -> Result<Input> {
    let number = delivery.number;
    let (title, key, channel) = match policy {
        GithubReviewPolicy::Correctness => ("PR review", format!("pr-review:{number}"), None),
        GithubReviewPolicy::Holistic { .. } => {
            let pass = if delivery.action == Action::Synchronize {
                "followup"
            } else {
                "initial"
            };
            let channel = format!("pr-holistic:{number}");
            (
                "Holistic review",
                format!("{channel}:{pass}"),
                Some(channel),
            )
        }
        GithubReviewPolicy::AgenticLint => (
            "Agentic lint review",
            format!("agentic-lint:{number}:{sha}"),
            None,
        ),
    };
    Ok(Input {
        profile,
        idempotency_key: key,
        // Reuse the workflow's channel identity and reservations during rollout.
        source: "actions".to_string(),
        watch_id: None,
        channel,
        slack: None,
        session: serde_json::from_value(serde_json::json!({
            "title": format!("{title} for #{number}"),
            "repo": delivery.repository,
            "base": sha,
            "github_issue": number,
            "goal": format!("{title} for pull request #{number} at head {sha}. Check out that exact commit before reviewing. Follow this profile's review instructions and publish its typed result."),
        }))?,
    })
}

async fn dispatch_route(
    state: &AppState,
    slug: &RepoSlug,
    delivery: &Delivery,
    pull: &ReviewPullRequest,
    route: &GithubReviewRoute,
) -> Result<()> {
    let app = state.trigger.app().context("GitHub App is unavailable")?;
    if !qualifies(app, slug, delivery, pull, &route.policy).await? {
        return Ok(());
    }
    let mapping = federation_get(&state.db, &route.federation)
        .await?
        .with_context(|| format!("missing GitHub federation {}", route.federation))?;
    let repository_id = delivery.repository_id.to_string();
    let workflow_ref = mapping.workflow_ref.as_deref().unwrap_or_default();
    if mapping.provider != "github"
        || mapping.repository_id.as_deref() != Some(repository_id.as_str())
        || mapping.event_name.as_deref() != Some("pull_request_target")
        || !workflow_ref.starts_with(&format!("{}/.github/workflows/", delivery.repository))
    {
        bail!("review routes require a pull_request_target federation for the same repository");
    }
    let [profile] = mapping.profiles.as_slice() else {
        bail!("a review federation must select one launch profile");
    };
    let subject = github_automation_subject(
        &repository_id,
        workflow_ref,
        &format!("repo:{}:pull_request", delivery.repository),
    );
    let request = review_run(delivery, &pull.head.sha, &route.policy, profile.clone())?;
    let RunView { id, status, .. } = create_run_core(
        state,
        request,
        subject,
        mapping.profiles,
        RunCaller::GithubReview {
            repository_id: &repository_id,
            service_tag: &mapping.service_tag,
        },
    )
    .await
    .map_err(|error| anyhow!(error.message().to_string()))?;
    if matches!(status.as_str(), "creating" | "waiting" | "delivering") {
        bail!("automation run {id} is {status}");
    }
    tracing::info!(
        repo = delivery.repository,
        number = delivery.number,
        run = id,
        status,
        "GitHub review delivery recorded"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unconfigured_events_are_ignored_and_unfinished_deliveries_are_reclaimed() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "repository": {"id": 99, "full_name": "acme/widgets"},
            "number": 42,
            "action": "synchronize",
            "pull_request": {
                "draft": false,
                "head": {"sha": "a".repeat(40), "repo": {"id": 99, "full_name": "acme/widgets"}}
            }
        }))
        .unwrap();
        assert!(!enqueue(&db, "push", &body).await.unwrap());
        config::apply(
            &db,
            &[(
                GITHUB_REVIEW_ROUTES_KEY.to_string(),
                Some(r#"[{"repository_id":99,"federation":"review","policy":{"kind":"holistic","changed_lines":250,"push_changed_lines":100}}]"#.into()),
            )],
        )
        .await
        .unwrap();
        assert!(enqueue(&db, "push", &body).await.unwrap());
        let first = claim(&db).await.unwrap().unwrap();
        assert!(claim(&db).await.unwrap().is_none());
        // Simulate a dispatch task that died before releasing its delivery.
        sqlx::query(
            "UPDATE processed_deliveries SET review_retry_at = 0 WHERE delivery_id = 'push'",
        )
        .execute(&db)
        .await
        .unwrap();
        let reclaimed = claim(&db).await.unwrap().unwrap();
        assert_eq!(reclaimed.delivery_id, first.delivery_id);
        assert_eq!(reclaimed.review_payload, first.review_payload);
        assert_eq!(reclaimed.review_attempt, first.review_attempt + 1);
    }

    #[test]
    fn small_holistic_reviews_require_an_added_abstraction_in_a_configured_source_directory() {
        let paths = vec!["lib/*/src".into(), "experiments".into()];
        assert!(abstraction_in_patch(
            &paths,
            "lib/iris/src/models.py",
            "-class Old:\n+    @dataclass(frozen=True)"
        ));
        assert!(!abstraction_in_patch(
            &paths,
            "lib/iris/helpers/src/models.py",
            "+class Helper:"
        ));
        assert!(!abstraction_in_patch(
            &paths,
            "experiments/example.py",
            "-class Old:\n+result = run()"
        ));
    }
}
