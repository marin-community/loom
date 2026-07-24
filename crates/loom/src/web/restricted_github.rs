//! Server-side GitHub tools for restricted sessions.
//!
//! The agent-facing MCP bridge authenticates with the session token. This
//! handler resolves the fixed repository and GitHub credential from durable
//! session/profile state, validates the stamped tool grant, and invokes `gh`
//! without a shell. Neither the repository nor the token is caller-controlled.

use std::process::Stdio;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use tokio::process::Command;
use weaver_api::{RestrictedGithubToolReq, RestrictedGithubToolView};

use super::{ApiResult, AppError, AppState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolArguments {
    number: i64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

async fn restricted_github_token(
    st: &AppState,
    profile: &str,
    repo: &crate::repo::RepoSlug,
) -> ApiResult<String> {
    if let Some(app) = st.trigger.app() {
        if app.is_configured().await {
            return app
                .token_for_repo(&repo.owner, &repo.name)
                .await
                .map_err(|error| {
                    AppError::new(
                        StatusCode::BAD_GATEWAY,
                        format!(
                            "could not mint a GitHub App token for {}: {error}",
                            repo.slug()
                        ),
                    )
                });
        }
    }
    if let Some(token) = crate::profile::env_pairs(&st.db, profile)
        .await
        .map_err(|error| AppError::new(StatusCode::BAD_GATEWAY, error.to_string()))?
        .into_iter()
        .find_map(|(name, value)| (name == "GH_TOKEN").then_some(value))
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(token);
    }
    Err(AppError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "restricted GitHub credential is unavailable",
    ))
}

fn validate_arguments(tool: &str, value: serde_json::Value) -> ApiResult<ToolArguments> {
    let arguments: ToolArguments = serde_json::from_value(value)
        .map_err(|error| AppError::bad_request(format!("invalid {tool} arguments: {error}")))?;
    if arguments.number <= 0 {
        return Err(AppError::bad_request("GitHub number must be positive"));
    }
    let requires_body = matches!(
        tool,
        "issue_comment" | "issue_edit" | "pr_comment" | "pr_edit"
    );
    match arguments.body.as_deref() {
        Some(body) if body.len() > crate::mcp::github::BODY_MAX_BYTES => {
            return Err(AppError::bad_request(format!(
                "GitHub body must be at most {} bytes",
                crate::mcp::github::BODY_MAX_BYTES
            )))
        }
        None if requires_body => {
            return Err(AppError::bad_request(format!("{tool} requires a body")))
        }
        _ => {}
    }
    if arguments.title.as_deref().is_some_and(|title| {
        title.trim().is_empty() || title.len() > crate::mcp::github::TITLE_MAX_BYTES
    }) {
        return Err(AppError::bad_request(format!(
            "GitHub title must be 1-{} bytes when provided",
            crate::mcp::github::TITLE_MAX_BYTES
        )));
    }
    if matches!(tool, "issue_view" | "pr_view")
        && (arguments.body.is_some() || arguments.title.is_some())
    {
        return Err(AppError::bad_request(format!(
            "{tool} accepts only a number"
        )));
    }
    Ok(arguments)
}

async fn invoke_gh(
    repo: &str,
    tool: &str,
    arguments: &ToolArguments,
    token: &str,
    config_dir: &std::path::Path,
) -> ApiResult<String> {
    let number = arguments.number.to_string();
    let (kind, verb) = tool
        .split_once('_')
        .ok_or_else(|| AppError::bad_request("invalid restricted GitHub tool"))?;
    let mut command = Command::new("gh");
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("GH_TOKEN", token)
        .env("GH_CONFIG_DIR", config_dir)
        .env("GH_PAGER", "cat")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1")
        .args([kind, verb, &number, "--repo", repo])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match verb {
        "view" => {
            command.args(["--json", "number,title,body,url,state"]);
        }
        "comment" => {
            command.args(["--body", arguments.body.as_deref().unwrap_or_default()]);
        }
        "edit" => {
            command.args(["--body", arguments.body.as_deref().unwrap_or_default()]);
            if let Some(title) = arguments.title.as_deref() {
                command.args(["--title", title]);
            }
        }
        _ => return Err(AppError::bad_request("invalid restricted GitHub verb")),
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), command.output())
        .await
        .map_err(|_| AppError::new(StatusCode::GATEWAY_TIMEOUT, "GitHub tool timed out"))?
        .map_err(|error| {
            AppError::new(
                StatusCode::BAD_GATEWAY,
                format!("failed to start GitHub CLI: {error}"),
            )
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).replace(token, "[REDACTED]");
        return Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            format!("GitHub {tool} failed: {}", detail.trim()),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout)
        .replace(token, "[REDACTED]")
        .trim()
        .to_string();
    Ok(if stdout.is_empty() {
        format!("GitHub {tool} completed for {repo}#{}", arguments.number)
    } else {
        stdout
    })
}

pub(super) async fn restricted_github_tool(
    State(st): State<AppState>,
    Path((id, tool)): Path<(String, String)>,
    Json(req): Json<RestrictedGithubToolReq>,
) -> ApiResult<Json<RestrictedGithubToolView>> {
    let session = crate::session::get(&st.db, &id)
        .await?
        .ok_or_else(|| AppError::not_found("session"))?;
    if !session.policy_restricted {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "session is not restricted",
        ));
    }
    let rule = crate::mcp::github::permission_rule(&tool)
        .ok_or_else(|| AppError::not_found("restricted GitHub tool"))?;
    let allowed: Vec<String> = serde_json::from_str(&session.policy_allowed_tools)
        .map_err(|error| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if !allowed.iter().any(|candidate| candidate == &rule) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "tool is not granted by the session policy",
        ));
    }
    let repo = session
        .github_repo
        .as_deref()
        .ok_or_else(|| AppError::bad_request("session has no fixed GitHub repository"))?;
    let repo = crate::repo::parse_slug(repo)
        .map_err(|_| AppError::bad_request("session GitHub repository is invalid"))?;
    let repo_slug = repo.slug();
    let arguments = validate_arguments(&tool, req.arguments)?;
    let tracking_issue = match session.tracking_issue_id {
        Some(id) => weaver_core::issue::get(&st.db, id).await?,
        None => None,
    }
    .ok_or_else(|| AppError::bad_request("session has no linked GitHub thread"))?;
    if tracking_issue.github_issue != Some(arguments.number)
        || tracking_issue.github_repo.as_deref() != Some(repo_slug.as_str())
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "GitHub tool target does not match the session's linked thread",
        ));
    }
    let token = restricted_github_token(&st, &session.profile, &repo).await?;
    let config_dir = crate::db::run_dir(&session.id).join("restricted-gh-config");
    tokio::fs::create_dir_all(&config_dir)
        .await
        .map_err(|error| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("creating restricted GitHub config directory: {error}"),
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| {
                AppError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("securing restricted GitHub config directory: {error}"),
                )
            })?;
    }
    let text = invoke_gh(&repo_slug, &tool, &arguments, &token, &config_dir).await?;
    Ok(Json(RestrictedGithubToolView { text }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use serde_json::json;

    use super::{restricted_github_token, validate_arguments};

    #[test]
    fn only_the_fixed_mcp_tools_map_to_permissions() {
        assert_eq!(
            crate::mcp::github::permission_rule("issue_edit").as_deref(),
            Some("mcp__loom_github__issue_edit")
        );
        assert!(crate::mcp::github::permission_rule("repository_delete").is_none());
    }

    #[test]
    fn arguments_are_bounded_and_tool_specific() {
        assert!(validate_arguments("issue_view", json!({ "number": 7 })).is_ok());
        assert!(validate_arguments("issue_view", json!({ "number": 7, "body": "x" })).is_err());
        assert!(validate_arguments("issue_edit", json!({ "number": 7 })).is_err());
        assert!(
            validate_arguments("issue_edit", json!({ "number": 7, "body": "clean body" })).is_ok()
        );
    }

    #[tokio::test]
    async fn github_app_precedes_the_restricted_profile_credential() {
        let db = crate::db::connect_in_memory().await.unwrap();
        crate::profile::env_set(&db, "github_comment", "GH_TOKEN", "profile-token")
            .await
            .unwrap();
        let app = Arc::new(crate::github_app::tests::configured_test_app(db.clone()).await);
        let state = super::AppState {
            db,
            bus: crate::events::EventBus::new(),
            addr: "127.0.0.1:0".to_string(),
            ide: Arc::new(crate::ide::IdeManager::new(crate::ide::ide_home())),
            trigger: crate::github_trigger::GithubTrigger::with_app(app),
            acp: crate::acp::AcpRegistry::new(),
            launch_gate: crate::launch_gate::RepoLaunchGate::default(),
        };
        let repo = crate::repo::parse_slug("marin-community/loom").unwrap();

        let resolved = restricted_github_token(&state, "github_comment", &repo)
            .await
            .unwrap();

        assert_eq!(
            resolved,
            crate::github_app::tests::MOCK_INSTALLATION_TOKEN,
            "the App credential must override the configured profile credential"
        );

        let uninstalled = crate::repo::parse_slug("uninstalled/loom").unwrap();
        let error = restricted_github_token(&state, "github_comment", &uninstalled)
            .await
            .unwrap_err();
        assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
    }
}
