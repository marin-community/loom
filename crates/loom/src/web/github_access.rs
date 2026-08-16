//! Human-authorized, per-session GitHub App repository access.
//!
//! Launch policy remains an immutable snapshot. These small overrides are the
//! audited escape hatch for work that legitimately expands to another repo.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde_json::json;
use weaver_api::{SessionGithubAccessView, SetSessionGithubAccessReq};

use crate::auth::Principal;

use super::{require_session, ApiResult, AppError, AppState};

fn require_human(principal: &Principal) -> ApiResult<()> {
    if principal.is_human() {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "GitHub repository access must be granted by a human operator",
        ))
    }
}

pub(super) async fn effective_repositories(
    db: &crate::Db,
    session: &crate::session::Session,
) -> anyhow::Result<Vec<String>> {
    let mut repositories: Vec<String> = serde_json::from_str(&session.policy_github_repositories)?;
    for grant in crate::github_access::list(db, &session.id).await? {
        repositories.retain(|candidate| candidate != &grant.repository);
        if grant.mode == "write" {
            repositories.push(grant.repository);
        }
    }
    repositories.sort();
    repositories.dedup();
    Ok(repositories)
}

pub(super) async fn list_github_access(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Json<Vec<SessionGithubAccessView>>> {
    require_human(&principal)?;
    let (session, _) = require_session(&st.db, &key).await?;
    let grants = crate::github_access::list(&st.db, &session.id)
        .await?
        .into_iter()
        .map(|grant| SessionGithubAccessView {
            repository: grant.repository,
            mode: grant.mode,
            granted_by: grant.granted_by,
            granted_at: grant.granted_at,
        })
        .collect();
    Ok(Json(grants))
}

pub(super) async fn set_github_access(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<SetSessionGithubAccessReq>,
) -> ApiResult<Json<SessionGithubAccessView>> {
    require_human(&principal)?;
    let (session, branch) = require_session(&st.db, &key).await?;
    let repository = crate::repo::parse_slug(req.repository.trim())
        .map_err(AppError::bad_request)?
        .slug();
    let mode = req.mode.trim().to_ascii_lowercase();
    if !matches!(mode.as_str(), "write" | "none") {
        return Err(AppError::bad_request(
            "GitHub access mode must be 'write' or 'none'",
        ));
    }

    // Validate the complete prospective token scope before changing durable
    // access. This catches an uninstalled repo and cross-installation mixes at
    // grant time instead of surprising the agent on its next push.
    if mode == "write" {
        let mut repositories = effective_repositories(&st.db, &session)
            .await
            .map_err(|error| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
        repositories.retain(|candidate| candidate != &repository);
        repositories.push(repository.clone());
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
        app.token_for_repositories(&repositories).await.map_err(|error| {
            AppError::new(
                StatusCode::BAD_GATEWAY,
                format!(
                    "could not grant access to {repository}; ensure the Loom GitHub App is installed on that repository: {error}"
                ),
            )
        })?;
    }

    crate::github_access::set(&st.db, &session.id, &repository, &mode, &principal.username).await?;
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
    crate::events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "github_access",
        json!({
            "repository": grant.repository,
            "mode": grant.mode,
            "by": grant.granted_by,
        }),
    )
    .await
    .ok();
    Ok(Json(SessionGithubAccessView {
        repository: grant.repository,
        mode: grant.mode,
        granted_by: grant.granted_by,
        granted_at: grant.granted_at,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::{Extension, Json};

    use super::{effective_repositories, require_human, set_github_access};
    use crate::auth::{AuthVia, Grant, Principal};
    use weaver_api::SetSessionGithubAccessReq;

    fn principal(grant: Grant) -> Principal {
        Principal {
            username: "test".to_string(),
            github_login: None,
            via: AuthVia::Token,
            grant,
            automation_context: None,
        }
    }

    #[test]
    fn only_humans_can_change_github_access() {
        assert!(require_human(&principal(Grant::Admin)).is_ok());
        assert!(require_human(&principal(Grant::User)).is_ok());
        assert!(require_human(&principal(Grant::Session {
            session_id: "session".to_string(),
            branch_id: "branch".to_string(),
        }))
        .is_err());
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
        crate::github_access::set(&db, "access", "acme/base", "none", "alice")
            .await
            .unwrap();
        crate::github_access::set(&db, "access", "acme/extra", "write", "alice")
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

        let Json(view) = set_github_access(
            State(state),
            Path("grant".to_string()),
            Extension(principal(Grant::Admin)),
            Json(SetSessionGithubAccessReq {
                repository: "marin-community/loom".to_string(),
                mode: "write".to_string(),
            }),
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
