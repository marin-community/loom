//! `loom permissions` — what a session may reach, and the requests to widen it.
//!
//! Each command here keeps a flag its operation has no room for. `show`,
//! `requests` and `request` take `--session`; the declared `session` is a
//! context operand with no command-line spelling, so this is the only way an
//! operator inspects a session other than the one they are standing in.
//! `grant`/`revoke` default that target from the environment, where the
//! declaration requires it. `approve`/`deny` join their trailing argv words
//! into one `reason`, and a declared operand is a single value.

use crate::client;
use anyhow::{Context, Result};
use clap::Subcommand;
use weaver_api::operations::permissions as perm_ops;
use weaver_api::{PermissionRequestView, SessionGithubAccessView};

#[derive(Subcommand)]
pub enum PermissionsCmd {
    /// Show effective Loom operations, GitHub scope, and pending requests.
    Show {
        /// Session key. Defaults to the session containing this command.
        #[arg(long)]
        session: Option<String>,
        /// Emit the typed response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Request a human-approved expansion of this session's external access.
    Request {
        #[command(subcommand)]
        resource: PermissionRequestResource,
    },
    /// List durable access requests for a session.
    Requests {
        #[arg(long)]
        session: Option<String>,
        /// pending, approved, or denied. Omit to list all.
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Approve and apply one pending request (human operator only).
    Approve {
        request: String,
        /// Optional audit reason. Multiple words are joined.
        reason: Vec<String>,
    },
    /// Deny one pending request (human operator only).
    Deny {
        request: String,
        /// Optional audit reason. Multiple words are joined.
        reason: Vec<String>,
    },
    /// Directly grant external access without a prior request (human only).
    Grant {
        #[command(subcommand)]
        resource: PermissionGrantResource,
    },
    /// Revoke an explicit external-access override (human only).
    Revoke {
        #[command(subcommand)]
        resource: PermissionGrantResource,
    },
}

#[derive(Subcommand)]
pub enum PermissionRequestResource {
    /// Ask for GitHub App write access to one repository.
    GithubRepository {
        repository: String,
        /// Why the task needs this repository.
        #[arg(long, required = true)]
        reason: String,
        #[arg(long, default_value = "write")]
        mode: String,
        /// Session key. Defaults to the session containing this command.
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PermissionGrantResource {
    /// Grant or revoke GitHub App write access to one repository.
    GithubRepository {
        repository: String,
        /// Session key. Defaults to the session containing this command.
        #[arg(long)]
        session: Option<String>,
    },
}

pub async fn run_permissions(cmd: PermissionsCmd) -> Result<()> {
    let client = client::default()?;
    match cmd {
        PermissionsCmd::Show { session, json } => {
            let session = github_access_session(session)?;
            let view = client
                .invoke::<perm_ops::effective::get::Op>(&perm_ops::effective::get::Input {
                    session,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!("session: {}", view.session_id);
                println!("actor:   {}", view.actor);
                println!("operations ({}):", view.operations.len());
                for operation in view.operations {
                    println!("  {operation}");
                }
                println!("GitHub repositories ({}):", view.github_repositories.len());
                for repository in view.github_repositories {
                    println!("  {repository}");
                }
                if !view.github_repository_patterns.is_empty() {
                    println!(
                        "GitHub owners grantable without review ({}):",
                        view.github_repository_patterns.len()
                    );
                    for pattern in view.github_repository_patterns {
                        println!("  {pattern}");
                    }
                }
                println!("pending requests ({}):", view.pending_requests.len());
                for request in view.pending_requests {
                    println!(
                        "  {}  {} {} — {}",
                        request.id, request.mode, request.repository, request.reason
                    );
                }
            }
            Ok(())
        }
        PermissionsCmd::Request { resource } => match resource {
            PermissionRequestResource::GithubRepository {
                repository,
                reason,
                mode,
                session,
            } => {
                let session = github_access_session(session)?;
                let request = client
                    .invoke::<perm_ops::requests::create::Op>(&perm_ops::requests::create::Input {
                        repository,
                        reason,
                        mode,
                        session,
                    })
                    .await?;
                println!("{}", permission_request_confirmation(&request));
                Ok(())
            }
        },
        PermissionsCmd::Requests {
            session,
            state,
            json,
        } => {
            let session = github_access_session(session)?;
            let requests = client
                .invoke::<perm_ops::requests::list::Op>(&perm_ops::requests::list::Input {
                    state,
                    session,
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&requests)?);
            } else if requests.is_empty() {
                println!("(no permission requests)");
            } else {
                for request in requests {
                    println!(
                        "{}  {:8} {} {} — {}",
                        request.id, request.state, request.mode, request.repository, request.reason
                    );
                }
            }
            Ok(())
        }
        PermissionsCmd::Approve { request, reason } => {
            let decided = client
                .invoke::<perm_ops::requests::approve::Op>(&perm_ops::requests::approve::Input {
                    request,
                    reason: reason.join(" "),
                })
                .await?;
            println!("approved {} — {}", decided.id, decided.repository);
            Ok(())
        }
        PermissionsCmd::Deny { request, reason } => {
            let decided = client
                .invoke::<perm_ops::requests::deny::Op>(&perm_ops::requests::deny::Input {
                    request,
                    reason: reason.join(" "),
                })
                .await?;
            println!("denied {} — {}", decided.id, decided.repository);
            Ok(())
        }
        // Grant and revoke are separate operations, not one command with a mode flag.
        PermissionsCmd::Grant { resource } => {
            let PermissionGrantResource::GithubRepository {
                repository,
                session,
            } = resource;
            let view = client
                .invoke::<perm_ops::github::grant::Op>(&perm_ops::github::grant::Input {
                    repository,
                    session: github_access_session(session)?,
                })
                .await?;
            print_github_access(&view);
            Ok(())
        }
        PermissionsCmd::Revoke { resource } => {
            let PermissionGrantResource::GithubRepository {
                repository,
                session,
            } = resource;
            let view = client
                .invoke::<perm_ops::github::revoke::Op>(&perm_ops::github::revoke::Input {
                    repository,
                    session: github_access_session(session)?,
                })
                .await?;
            print_github_access(&view);
            Ok(())
        }
    }
}

pub(crate) fn print_github_access(view: &SessionGithubAccessView) {
    println!("{} {} — {}", view.mode, view.repository, view.granted_by);
}

/// What `loom permissions request` prints back.
///
/// It reports the state the server returned rather than assuming `pending`: a
/// request for a repository already covered by a grant pattern is approved on
/// arrival, and saying "pending" then sent the caller off to wait for a
/// decision that had already been made.
fn permission_request_confirmation(request: &PermissionRequestView) -> String {
    format!(
        "request {} {} — {} {}",
        request.id, request.state, request.mode, request.repository
    )
}

pub(crate) fn github_access_session(explicit: Option<String>) -> Result<String> {
    explicit
        .or_else(|| std::env::var("LOOM_SESSION_ID").ok())
        .or_else(|| std::env::var("WEAVER_BRANCH").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .context("not inside a loom session — pass the target explicitly with --session <session>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_request_confirmation_reports_the_returned_state() {
        let mut request = PermissionRequestView {
            id: "req-1".to_string(),
            session_id: "session-1".to_string(),
            kind: "github_repository".to_string(),
            repository: "acme/widgets".to_string(),
            mode: "write".to_string(),
            reason: "open the pull request".to_string(),
            state: "pending".to_string(),
            requested_by: "session:session-1".to_string(),
            requested_at: "2026-08-24T00:00:00Z".to_string(),
            decided_by: None,
            decided_at: None,
            decision_reason: None,
        };
        assert_eq!(
            permission_request_confirmation(&request),
            "request req-1 pending — write acme/widgets"
        );

        request.state = "approved".to_string();
        request.decided_by = Some("policy:acme/*".to_string());
        assert_eq!(
            permission_request_confirmation(&request),
            "request req-1 approved — write acme/widgets"
        );
    }
}
