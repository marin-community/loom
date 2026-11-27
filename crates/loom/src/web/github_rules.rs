//! Dispatch deployment-defined rules from signed GitHub App deliveries.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use tokio::task::JoinSet;
use weaver_api::{operations::runs::create::Input, RunView};
use weaver_core::{
    config::{self, DEFAULT_GITHUB_PULL_REQUEST_RULES, GITHUB_PULL_REQUEST_RULES_KEY},
    db::Db,
    github_rules::{self, Rule, Target},
};

use crate::{
    automation::{federation_get, github_automation_subject},
    github_app::{PullRequestRepository, PullRequestSnapshot},
    repo::parse_slug,
};

use super::{
    automation::{create_run_core, RunCaller},
    AppState,
};

// Match the public route's Axum body limit so the parser stays bounded if reused.
const MAX_WEBHOOK_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_IN_FLIGHT: usize = 8;
const MAX_ATTEMPTS: i64 = 5;
// Exceed the automation reservation's five-minute stale interval so a
// reclaimed delivery can resume a run interrupted by a daemon restart.
const LEASE_SECONDS: i64 = 330;

#[derive(Deserialize, Serialize)]
struct Delivery {
    repository_id: i64,
    repository: String,
    number: i64,
    action: String,
    label: Option<String>,
}

impl Delivery {
    fn matches(&self, rule: &Rule) -> bool {
        rule.repository_id == self.repository_id
            && rule.events.iter().any(|event| {
                event.actions.contains(&self.action)
                    && event
                        .label
                        .as_ref()
                        .is_none_or(|label| self.label.as_ref() == Some(label))
            })
    }
}

async fn rules(db: &Db) -> Result<Vec<Rule>> {
    let value = config::try_get(db, GITHUB_PULL_REQUEST_RULES_KEY).await?;
    github_rules::parse(
        value
            .as_deref()
            .unwrap_or(DEFAULT_GITHUB_PULL_REQUEST_RULES),
    )
    .map_err(anyhow::Error::msg)
}

fn delivery_from_payload(payload: &Value) -> Option<Delivery> {
    let repository_id = payload["repository"]["id"].as_i64()?;
    let number = payload["number"].as_i64()?;
    let pull = &payload["pull_request"];
    if number <= 0
        || pull["draft"].as_bool()?
        || pull["head"]["repo"]["id"].as_i64()? != repository_id
    {
        return None;
    }
    Some(Delivery {
        repository_id,
        repository: payload["repository"]["full_name"].as_str()?.to_string(),
        number,
        action: payload["action"].as_str()?.to_string(),
        label: payload["label"]["name"].as_str().map(str::to_string),
    })
}

/// Store the PR identity and event action needed to resolve the current head.
pub(super) async fn enqueue(db: &Db, id: &str, body: &[u8]) -> Result<bool> {
    if body.len() > MAX_WEBHOOK_BODY_BYTES {
        bail!("GitHub webhook body exceeds the pull request payload limit");
    }
    let payload: Value = match serde_json::from_slice(body) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(delivery = id, %error, "invalid pull request webhook");
            return Ok(false);
        }
    };
    let Some(delivery) = delivery_from_payload(&payload) else {
        return Ok(false);
    };
    if !rules(db).await?.iter().any(|rule| delivery.matches(rule)) {
        return Ok(false);
    }
    Ok(sqlx::query(
        "INSERT INTO processed_deliveries (delivery_id, rule_payload) VALUES (?, ?)
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
struct PendingDelivery {
    delivery_id: String,
    rule_payload: String,
    rule_attempt: i64,
}

async fn claim(db: &Db) -> Result<Option<PendingDelivery>> {
    Ok(sqlx::query_as(
        "UPDATE processed_deliveries
         SET rule_attempt = rule_attempt + 1, rule_retry_at = unixepoch() + ?
         WHERE delivery_id = (
             SELECT delivery_id FROM processed_deliveries
             WHERE rule_payload IS NOT NULL AND rule_retry_at <= unixepoch()
             ORDER BY rule_retry_at, received_at, delivery_id LIMIT 1
         )
         RETURNING delivery_id, rule_payload, rule_attempt",
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
                    Ok(Err(error)) => tracing::warn!(%error, "could not record GitHub rule delivery outcome"),
                    Err(error) => tracing::error!(%error, "GitHub rule delivery task stopped"),
                }
            }
            _ = timer.tick(), if active.len() < MAX_IN_FLIGHT => {
                match claim(&state.db).await {
                    Ok(Some(pending)) => {
                        let state = state.clone();
                        active.spawn(async move { process(&state, pending).await });
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!(%error, "could not claim GitHub rule delivery"),
                }
            }
        }
    }
}

async fn process(state: &AppState, pending: PendingDelivery) -> Result<()> {
    let result = tokio::time::timeout(
        Duration::from_secs((LEASE_SECONDS - 30) as u64),
        dispatch(state, &pending.rule_payload),
    )
    .await
    .context("GitHub rule dispatch timed out")
    .and_then(|result| result);
    let retry = result.is_err() && pending.rule_attempt < MAX_ATTEMPTS;
    if let Err(error) = result {
        tracing::warn!(delivery = pending.delivery_id, attempt = pending.rule_attempt, retry, %error, "GitHub rule dispatch failed");
    }
    sqlx::query(
        "UPDATE processed_deliveries
         SET rule_payload = ?, rule_retry_at = unixepoch() + ?
         WHERE delivery_id = ? AND rule_attempt = ?",
    )
    .bind(retry.then_some(&pending.rule_payload))
    .bind(10 * pending.rule_attempt)
    .bind(&pending.delivery_id)
    .bind(pending.rule_attempt)
    .execute(&state.db)
    .await?;
    Ok(())
}

fn launchable(pull: &PullRequestSnapshot, delivery: &Delivery) -> bool {
    let same_repo = |repo: &PullRequestRepository| {
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
    let rules: Vec<_> = rules(&state.db)
        .await?
        .into_iter()
        .filter(|rule| delivery.matches(rule))
        .collect();
    if rules.is_empty() {
        return Ok(());
    }
    let slug = parse_slug(&delivery.repository).map_err(anyhow::Error::msg)?;
    let app = state.trigger.app().context("GitHub App is unavailable")?;
    let pull = app.pull_request_snapshot(&slug, delivery.number).await?;
    if !launchable(&pull, &delivery) {
        return Ok(());
    }
    let is_bot = pull.user.kind == "Bot";
    let rules: Vec<_> = rules
        .into_iter()
        .filter(|rule| {
            (!is_bot
                || rule
                    .allowed_bots
                    .iter()
                    .any(|login| login.eq_ignore_ascii_case(&pull.user.login)))
                && !pull
                    .labels
                    .iter()
                    .any(|label| rule.excluded_labels.contains(&label.name))
        })
        .collect();
    if rules.is_empty()
        || (!is_bot && !app.pull_request_author_can_write(&slug, &pull.user).await?)
    {
        return Ok(());
    }

    // Each route keeps its own run reservation, so retrying a delivery after
    // one launch fails does not launch the successful routes again.
    let mut launches = JoinSet::new();
    for rule in rules {
        let state = state.clone();
        let repository = delivery.repository.clone();
        let head = pull.head.sha.clone();
        launches.spawn(async move {
            dispatch_rule(&state, &repository, delivery.number, &head, &rule)
                .await
                .with_context(|| format!("pull request rule '{}'", rule.name))
        });
    }
    let mut failures = Vec::new();
    while let Some(result) = launches.join_next().await {
        if let Err(error) = result
            .context("rule task stopped")
            .and_then(|result| result)
        {
            failures.push(format!("{error:#}"));
        }
    }
    if !failures.is_empty() {
        bail!("{}", failures.join("; "));
    }
    Ok(())
}

/// Resolve the existing workflow identity when a rule replaces an Actions launch.
async fn workflow_target(
    db: &Db,
    federation: &str,
    repository: &str,
    repository_id: &str,
) -> Result<(String, String, String)> {
    let mapping = federation_get(db, federation)
        .await?
        .with_context(|| format!("missing GitHub federation {federation}"))?;
    let workflow_ref = mapping.workflow_ref.as_deref().unwrap_or_default();
    if mapping.provider != "github"
        || mapping.repository_id.as_deref() != Some(repository_id)
        || mapping.event_name.as_deref() != Some("pull_request_target")
        || !workflow_ref.starts_with(&format!("{repository}/.github/workflows/"))
    {
        bail!(
            "pull request rules require a pull_request_target federation for the same repository"
        );
    }
    let [profile] = mapping.profiles.as_slice() else {
        bail!("a pull request rule federation must select one launch profile");
    };
    let subject = github_automation_subject(
        repository_id,
        workflow_ref,
        &format!("repo:{repository}:pull_request"),
    );
    Ok((profile.clone(), mapping.service_tag, subject))
}

async fn dispatch_rule(
    state: &AppState,
    repository: &str,
    number: i64,
    head: &str,
    rule: &Rule,
) -> Result<()> {
    let repository_id = rule.repository_id.to_string();
    let (profile, service_tag, subject) = match &rule.target {
        Target::Profile(profile) => (
            profile.clone(),
            "github-webhook".to_string(),
            format!("github-webhook:{}:{}", repository_id, rule.name),
        ),
        Target::Federation(federation) => {
            workflow_target(&state.db, federation, repository, &repository_id).await?
        }
    };
    let number_text = number.to_string();
    let render = |template: &str| {
        github_rules::render(template, &number_text, head).map_err(anyhow::Error::msg)
    };
    let request = Input {
        profile: profile.clone(),
        idempotency_key: render(&rule.run.idempotency_key)?,
        source: "actions".to_string(),
        watch_id: None,
        channel: rule.run.channel.as_deref().map(render).transpose()?,
        slack: None,
        session: serde_json::from_value(serde_json::json!({
            "title": render(&rule.run.title)?,
            "repo": repository,
            "base": head,
            "github_issue": number,
            "goal": render(&rule.run.goal)?,
        }))?,
    };
    let RunView { id, status, .. } = create_run_core(
        state,
        request,
        subject,
        vec![profile],
        RunCaller::GithubWebhook {
            repository_id: &repository_id,
            service_tag: &service_tag,
        },
    )
    .await
    .map_err(|error| anyhow!(error.message().to_string()))?;
    if matches!(status.as_str(), "creating" | "waiting" | "delivering") {
        bail!("automation run {id} is {status}");
    }
    tracing::info!(
        rule = rule.name,
        repo = repository,
        number,
        run = id,
        status,
        "GitHub rule delivery recorded"
    );
    Ok(())
}
