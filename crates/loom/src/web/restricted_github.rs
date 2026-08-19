//! Server-side GitHub tools for restricted sessions.
//!
//! The agent-facing MCP bridge authenticates with the session token. This
//! handler resolves the fixed repository from durable session state, validates
//! the stamped tool grant, and calls GitHub through Loom's App client. Neither
//! the repository nor the credential is caller-controlled.

use axum::http::StatusCode;
use serde::Deserialize;
use weaver_api::operations::permissions as permission_operations;
use weaver_api::{GithubTokenView, RestrictedGithubToolView};

use crate::github_app::{GithubApp, GithubThreadKind};

use super::operations::OperationContext;
use super::{ApiResult, AppError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolArguments {
    number: i64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

pub(super) async fn github_token_operation(
    context: OperationContext,
    input: permission_operations::github::token::Input,
) -> ApiResult<permission_operations::github::token::Output> {
    let st = context.state;
    let session = crate::session::get(&st.db, &input.session)
        .await?
        .ok_or_else(|| AppError::not_found("session"))?;
    if session.policy_restricted {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "restricted sessions may use only Loom's server-side GitHub tools",
        ));
    }
    let repositories = super::github_access::effective_repositories(&st.db, &session)
        .await
        .map_err(|error| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if repositories.is_empty() {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "session profile has no GitHub repository credential",
        ));
    }
    let app = super::configured_github_app(&st).await?;
    let token = app
        .token_for_repositories(&repositories)
        .await
        .map_err(|error| AppError::new(StatusCode::BAD_GATEWAY, error.to_string()))?;
    Ok(GithubTokenView { token })
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

async fn invoke_app(
    app: &GithubApp,
    repo: &crate::repo::RepoSlug,
    tool: &str,
    arguments: &ToolArguments,
) -> ApiResult<String> {
    let (kind, verb) = tool
        .split_once('_')
        .ok_or_else(|| AppError::bad_request("invalid restricted GitHub tool"))?;
    let kind = match kind {
        "issue" => GithubThreadKind::Issue,
        "pr" => GithubThreadKind::PullRequest,
        _ => return Err(AppError::bad_request("invalid restricted GitHub resource")),
    };
    let request = async {
        match verb {
            "view" => {
                let value = app.thread_view(repo, kind, arguments.number).await?;
                serde_json::to_string(&value).map_err(anyhow::Error::from)
            }
            "comment" => {
                app.thread_comment(
                    repo,
                    arguments.number,
                    arguments.body.as_deref().unwrap_or_default(),
                )
                .await?;
                Ok(format!(
                    "GitHub {tool} completed for {}#{}",
                    repo.slug(),
                    arguments.number
                ))
            }
            "edit" => {
                app.thread_edit(
                    repo,
                    kind,
                    arguments.number,
                    arguments.title.as_deref(),
                    arguments.body.as_deref().unwrap_or_default(),
                )
                .await?;
                Ok(format!(
                    "GitHub {tool} completed for {}#{}",
                    repo.slug(),
                    arguments.number
                ))
            }
            _ => Err(anyhow::anyhow!("invalid restricted GitHub verb")),
        }
    }
    .await;
    request.map_err(|error| {
        AppError::new(
            StatusCode::BAD_GATEWAY,
            format!("GitHub {tool} failed: {error}"),
        )
    })
}

pub(super) async fn restricted_github_invoke_operation(
    context: OperationContext,
    input: permission_operations::github::restricted::invoke::Input,
) -> ApiResult<permission_operations::github::restricted::invoke::Output> {
    let st = context.state;
    let session = crate::session::get(&st.db, &input.session)
        .await?
        .ok_or_else(|| AppError::not_found("session"))?;
    if !session.policy_restricted {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "session is not restricted",
        ));
    }
    let rule = crate::mcp::github::permission_rule(&input.tool)
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
    let arguments = validate_arguments(&input.tool, input.arguments)?;
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
    let app = super::configured_github_app(&st).await?;
    let text = invoke_app(app, &repo, &input.tool, &arguments).await?;
    Ok(RestrictedGithubToolView { text })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate_arguments;

    // `github_token` used to gate itself with a local `principal_owns_session`
    // check (Grant::Session only, no Admin/User path at all). That check is now
    // `permissions.github.token`'s declared `scope = Session`, enforced once for
    // every operation by `crate::web::scope::require_session_access` — not
    // duplicated here.

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
}
