//! Actor-aware boundary for session runtime operations.
//!
//! HTTP, GitHub, Slack, watches, and automation runs construct an [`Actor`]
//! through the narrow constructors here. Session orchestration consumes the
//! actor's derived attribution instead of accepting client-authored origin or
//! creator strings.

use crate::auth::{Grant, Principal};
use crate::web::{ApiResult, AppState};
use weaver_api::{CreateReq, SessionView};

#[derive(Debug, Clone)]
enum ActorKind {
    Admin {
        username: String,
        delegated: bool,
    },
    Producer {
        origin: &'static str,
        subject: String,
    },
    Automation {
        subject: String,
        profiles: Vec<String>,
        run_id: Option<String>,
        session_id: Option<String>,
    },
    Session {
        username: String,
        session_id: String,
        branch_id: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Actor(ActorKind);

impl Actor {
    pub(crate) fn from_principal(principal: &Principal, delegated: bool) -> Self {
        match &principal.grant {
            Grant::Admin => Self(ActorKind::Admin {
                username: principal.username.clone(),
                delegated,
            }),
            Grant::Automation { subject, profiles } => Self(ActorKind::Automation {
                subject: subject.clone(),
                profiles: profiles.clone(),
                run_id: None,
                session_id: None,
            }),
            Grant::Session {
                session_id,
                branch_id,
            } => Self(ActorKind::Session {
                username: principal.username.clone(),
                session_id: session_id.clone(),
                branch_id: branch_id.clone(),
            }),
        }
    }

    pub(crate) fn producer(origin: &'static str, subject: impl Into<String>) -> Self {
        debug_assert!(matches!(
            origin,
            "github" | "slack" | "watch" | "monitor" | "startup"
        ));
        Self(ActorKind::Producer {
            origin,
            subject: subject.into(),
        })
    }

    pub(crate) fn automation(
        subject: impl Into<String>,
        profiles: Vec<String>,
        run_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self(ActorKind::Automation {
            subject: subject.into(),
            profiles,
            run_id: Some(run_id.into()),
            session_id: Some(session_id.into()),
        })
    }

    pub(crate) fn origin(&self) -> &str {
        match &self.0 {
            ActorKind::Admin {
                delegated: true, ..
            }
            | ActorKind::Session { .. } => "agent",
            ActorKind::Admin { .. } => "user",
            ActorKind::Producer { origin, .. } => origin,
            ActorKind::Automation { .. } => "actions",
        }
    }

    pub(crate) fn display_creator(&self) -> Option<String> {
        match &self.0 {
            ActorKind::Admin { username, .. } | ActorKind::Session { username, .. } => {
                Some(username.clone())
            }
            ActorKind::Producer { subject, .. } | ActorKind::Automation { subject, .. } => {
                Some(subject.clone())
            }
        }
    }

    pub(crate) fn bound_parent_branch(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Session { branch_id, .. } => Some(branch_id),
            _ => None,
        }
    }

    pub(crate) fn creator_identity(&self) -> (&'static str, String) {
        match &self.0 {
            ActorKind::Admin { username, .. } => ("user", username.clone()),
            ActorKind::Producer { subject, .. } => ("system", subject.clone()),
            ActorKind::Automation { subject, .. } => ("automation", subject.clone()),
            ActorKind::Session { session_id, .. } => ("session", session_id.clone()),
        }
    }

    pub(crate) fn allowed_profiles(&self) -> Option<&[String]> {
        match &self.0 {
            ActorKind::Automation { profiles, .. } => Some(profiles),
            _ => None,
        }
    }

    pub(crate) fn automation_run_id(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Automation { run_id, .. } => run_id.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn reserved_session_id(&self) -> Option<&str> {
        match &self.0 {
            ActorKind::Automation { session_id, .. } => session_id.as_deref(),
            _ => None,
        }
    }
}

/// The single actor-taking entrypoint for all session producers. HTTP, Slack,
/// GitHub, and automation runs cannot bypass attribution/grant derivation by
/// calling the web provisioning implementation directly.
pub(crate) async fn create_session(
    state: AppState,
    request: CreateReq,
    actor: Actor,
) -> ApiResult<SessionView> {
    crate::web::sessions::provision_session(state, request, actor).await
}
