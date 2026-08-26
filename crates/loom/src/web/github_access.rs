//! Human-authorized, per-session GitHub App repository access.
//!
//! Launch policy remains an immutable snapshot. These small overrides are the
//! audited escape hatch for work that legitimately expands to another repo.

use axum::http::StatusCode;
use serde_json::json;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::operations::sessions as session_operations;
use weaver_api::SessionGithubAccessView;

use super::operations::OperationContext;
use super::{require_session, ApiResult, AppError, AppState};

/// The concrete repositories one session's App token may be scoped to. Any
/// `owner/*` entry in the launch policy is dropped here: a pattern authorizes
/// expansion, it is not itself a token scope.
pub(super) async fn effective_repositories(
    db: &crate::Db,
    session: &crate::session::Session,
) -> anyhow::Result<Vec<String>> {
    let policy: Vec<String> = serde_json::from_str(&session.policy_github_repositories)?;
    let mut repositories = crate::runtime::concrete_repositories(&policy);
    for grant in crate::github_access::list(db, &session.id).await? {
        repositories.retain(|candidate| candidate != &grant.repository);
        if grant.mode == crate::github_access::Mode::Write {
            repositories.push(grant.repository);
        }
    }
    repositories.sort();
    repositories.dedup();
    Ok(repositories)
}

/// The `owner/*` entries stamped on one session's launch policy — the owners it
/// may expand into without a human decision.
pub(super) fn policy_repository_patterns(
    session: &crate::session::Session,
) -> anyhow::Result<Vec<String>> {
    let policy: Vec<String> = serde_json::from_str(&session.policy_github_repositories)?;
    Ok(crate::runtime::repository_patterns(&policy))
}

/// Validate that the GitHub App can actually grant write access to one
/// repository. Nothing is stored until this succeeds.
///
/// Only the new repository is minted, never the session's whole prospective
/// set: tokens are brokered per repository, so a session may hold access
/// spanning several owners even though no single installation token could
/// cover them all.
pub(super) async fn validate_github_write(st: &AppState, repository: &str) -> ApiResult<()> {
    let repositories = vec![repository.to_string()];
    let app = st.trigger.app().ok_or_else(|| {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub App credential is unavailable",
        )
    })?;
    if !app.is_configured().await {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub App credential is unavailable",
        ));
    }
    app.token_for_repositories(&repositories)
        .await
        .map_err(|error| {
            AppError::new(
                StatusCode::BAD_GATEWAY,
                format!(
                    "could not grant access to {repository}; ensure the Loom GitHub App is installed on that repository: {error}"
                ),
            )
        })?;
    Ok(())
}

/// `sessions.github.access.list`. The `actor = User` declaration on the
/// operation already enforces the human-only restriction centrally.
pub(super) async fn list_github_access_operation(
    context: OperationContext,
    input: session_operations::github::access::list::Input,
) -> ApiResult<Vec<SessionGithubAccessView>> {
    let st = &context.state;
    let (session, _) = require_session(&st.db, &input.session).await?;
    Ok(crate::github_access::list(&st.db, &session.id)
        .await?
        .into_iter()
        .map(|grant| SessionGithubAccessView {
            repository: grant.repository,
            mode: grant.mode.as_str().to_string(),
            granted_by: grant.granted_by,
            granted_at: grant.granted_at,
        })
        .collect())
}

/// Shared body for `permissions.github.grant` and `permissions.github.revoke`:
/// store the requested mode for one repository and audit the change. Which
/// humans may reach this at all is `actor = User` on each declaration,
/// enforced centrally — nothing here re-checks who the caller is.
async fn set_github_access_and_record(
    st: &AppState,
    granted_by: &str,
    session_key: &str,
    repository: &str,
    mode: crate::github_access::Mode,
) -> ApiResult<SessionGithubAccessView> {
    let (session, branch) = require_session(&st.db, session_key).await?;
    let repository = crate::repo::parse_slug(repository.trim())
        .map_err(AppError::bad_request)?
        .slug();

    // Prove the App can grant this repository before changing durable access,
    // so an uninstalled repo fails here rather than surprising the agent on its
    // next push.
    if mode == crate::github_access::Mode::Write {
        validate_github_write(st, &repository).await?;
    }

    crate::github_access::set(&st.db, &session.id, &repository, mode, granted_by).await?;
    let grant = crate::github_access::list(&st.db, &session.id)
        .await?
        .into_iter()
        .find(|grant| grant.repository == repository)
        .ok_or_else(|| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "GitHub access update was not stored",
            )
        })?;
    if let Err(error) = crate::events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "github_access",
        json!({
            "repository": grant.repository,
            "mode": grant.mode.as_str(),
            "by": grant.granted_by,
        }),
    )
    .await
    {
        tracing::warn!(session = %session.id, %repository, error = %error,
            "failed to record GitHub access audit event");
    }
    Ok(SessionGithubAccessView {
        repository: grant.repository,
        mode: grant.mode.as_str().to_string(),
        granted_by: grant.granted_by,
        granted_at: grant.granted_at,
    })
}

pub(super) async fn grant_github_access_operation(
    context: OperationContext,
    input: permission_operations::github::grant::Input,
) -> ApiResult<permission_operations::github::grant::Output> {
    set_github_access_and_record(
        &context.state,
        &context.principal.username,
        &input.session,
        &input.repository,
        crate::github_access::Mode::Write,
    )
    .await
}

pub(super) async fn revoke_github_access_operation(
    context: OperationContext,
    input: permission_operations::github::revoke::Input,
) -> ApiResult<permission_operations::github::revoke::Output> {
    set_github_access_and_record(
        &context.state,
        &context.principal.username,
        &input.session,
        &input.repository,
        crate::github_access::Mode::None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{effective_repositories, grant_github_access_operation, OperationContext};
    use crate::auth::{AuthVia, Grant, Principal};

    fn principal(grant: Grant) -> Principal {
        Principal {
            username: "test".to_string(),
            github_login: None,
            via: AuthVia::Token,
            grant,
            automation_context: None,
        }
    }

    /// `actor = User` on each operation ensures only humans can reach this code.
    #[test]
    fn only_humans_can_change_github_access() {
        for id in [
            "permissions.github.grant",
            "permissions.github.revoke",
            "sessions.github.access.list",
        ] {
            let spec = weaver_api::operation(id).expect(id);
            assert_eq!(
                spec.actor,
                weaver_api::ActorPolicy::User,
                "{id} must stay human-only"
            );
        }
    }

    #[tokio::test]
    async fn overrides_add_and_mask_launch_policy_repositories() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = weaver_core::branch::upsert(&db, "/repo", "weaver/access", "main")
            .await
            .unwrap();
        crate::session::insert(
            &db,
            &crate::session::NewSession {
                id: "access".to_string(),
                branch_id: branch.id,
                work_dir: "/w".to_string(),
                term_session: "weaver-access".to_string(),
                agent_kind: "codex".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: Some("alice".to_string()),
                protocol: "acp".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        sqlx::query("UPDATE sessions SET policy_github_repositories = ? WHERE id = 'access'")
            .bind(r#"["acme/base"]"#)
            .execute(&db)
            .await
            .unwrap();
        crate::github_access::set(
            &db,
            "access",
            "acme/base",
            crate::github_access::Mode::None,
            "alice",
        )
        .await
        .unwrap();
        crate::github_access::set(
            &db,
            "access",
            "acme/extra",
            crate::github_access::Mode::Write,
            "alice",
        )
        .await
        .unwrap();
        let session = crate::session::get(&db, "access").await.unwrap().unwrap();

        assert_eq!(
            effective_repositories(&db, &session).await.unwrap(),
            vec!["acme/extra"]
        );
    }

    #[tokio::test]
    async fn write_grant_is_validated_and_stored_for_a_human() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let branch = weaver_core::branch::upsert(&db, "/repo", "weaver/grant", "main")
            .await
            .unwrap();
        crate::session::insert(
            &db,
            &crate::session::NewSession {
                id: "grant".to_string(),
                branch_id: branch.id,
                work_dir: "/w".to_string(),
                term_session: "weaver-grant".to_string(),
                agent_kind: "codex".to_string(),
                model: String::new(),
                effort: String::new(),
                status: "running".to_string(),
                github_repo: None,
                parent_branch_id: None,
                managed_by: None,
                created_by: Some("alice".to_string()),
                protocol: "acp".to_string(),
                origin: "user".to_string(),
                class: "interactive".to_string(),
                tracking_issue_id: None,
            },
        )
        .await
        .unwrap();
        let app = Arc::new(crate::github_app::tests::configured_test_app(db.clone()).await);
        let state = super::AppState {
            ctx: crate::Ctx {
                db: db.clone(),
                bus: crate::events::EventBus::new(),
                addr: "127.0.0.1:0".to_string(),
            },
            ide: Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::with_app(app),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        };

        let view = grant_github_access_operation(
            OperationContext::new(state, principal(Grant::Admin)),
            weaver_api::operations::permissions::github::grant::Input {
                repository: "marin-community/loom".to_string(),
                session: "grant".to_string(),
            },
        )
        .await
        .unwrap();

        assert_eq!(view.repository, "marin-community/loom");
        assert_eq!(view.mode, "write");
        assert_eq!(
            crate::github_access::list(&db, "grant").await.unwrap()[0].repository,
            "marin-community/loom"
        );
    }
}
