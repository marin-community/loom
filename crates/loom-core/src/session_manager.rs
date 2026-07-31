//! Database-backed ownership reconciliation for detached session resources.
//!
//! Tapestry supervisors deliberately outlive `loom server run`. That makes a
//! control-plane restart safe, but it also means the external runtime needs the
//! inverse of the monitor's orphan check:
//!
//! * session row + missing supervisor -> the monitor marks the row `orphaned`;
//! * supervisor + no durable owner -> this manager tears the supervisor down.
//!
//! Only Loom's two deterministic namespaces are considered. The operator
//! scratch shell and unrelated Tapestry users are never touched.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;

use crate::db::Db;
use crate::{backend, runs, session, shell};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub inspected: usize,
    pub invalidated_sessions: usize,
    pub removed_agents: usize,
    pub removed_debug_shells: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum OwnedResource<'a> {
    Agent(&'a str),
    TransientRelay,
    DebugShell(&'a str),
}

/// Parse only supervisor names reserved by Loom.
fn owned_resource(name: &str) -> Option<OwnedResource<'_>> {
    if name
        .strip_prefix("weaver-acp-prompt-")
        .is_some_and(|nonce| !nonce.is_empty())
    {
        return Some(OwnedResource::TransientRelay);
    }
    if let Some(id) = name.strip_prefix("weaver-").filter(|id| !id.is_empty()) {
        return Some(OwnedResource::Agent(id));
    }
    let rest = name.strip_prefix("loom-shell-")?;
    let (id, index) = rest.rsplit_once('-')?;
    if id.is_empty() || index.parse::<u32>().is_err() {
        return None;
    }
    Some(OwnedResource::DebugShell(id))
}

/// Tear down the deterministic runtime names held by an automation launch
/// reservation that never produced a session row.
///
/// Every operation is idempotent. A launch racing cancellation performs a
/// second ownership check before promotion and tears down any later session it
/// managed to create.
pub async fn teardown_reserved_runtime(session_id: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let agent = format!("weaver-{session_id}");
    if let Err(error) = backend::kill_session_and_wait(&agent).await {
        warnings.push(format!("terminal remove: {error}"));
    }
    shell::kill_debug_all(session_id).await;
    warnings
}

/// Reconcile every live Loom-owned supervisor against durable ownership.
///
/// Every non-archived session owns its agent and debug supervisors, including
/// inspectable `done`/`error` sessions. An active automation run temporarily
/// owns the agent's deterministic name before a session row appears. A live
/// one-shot ACP prompt owns its nondurable relay through the ACP registry.
/// Archived, missing, and cancellation-invalidated sessions own no runtime.
pub async fn reconcile_supervisors(
    db: &Db,
    acp: &crate::acp::AcpRegistry,
) -> Result<ReconcileReport> {
    // Close the only crash window in cancellation-wins provisioning: a session
    // row may have landed just before the request was cancelled, while the
    // provisioning task died before it could perform its final ownership check.
    // Keep the row visible for an operator, but make it terminal before runtime
    // classification so its supervisor is removed below.
    let invalidated: HashSet<String> = runs::invalidate_sessions_from_cancelled_launches(db)
        .await?
        .into_iter()
        .collect();
    for id in &invalidated {
        tracing::warn!(
            session = id,
            "session materialized after its launch attempt was cancelled; marked error"
        );
    }

    let sessions: HashMap<String, String> = session::list(db)
        .await?
        .into_iter()
        .map(|session| (session.id, session.status))
        .collect();
    let run_owners = runs::runtime_owner_ids(db).await?;
    let supervisors = backend::list_sessions().await?;
    let mut report = ReconcileReport {
        inspected: supervisors.len(),
        invalidated_sessions: invalidated.len(),
        ..ReconcileReport::default()
    };

    for name in supervisors {
        let Some(resource) = owned_resource(&name) else {
            continue;
        };
        let keep = match resource {
            OwnedResource::Agent(id) => {
                !invalidated.contains(id)
                    && (sessions.get(id).is_some_and(|status| status != "archived")
                        || run_owners.contains(id))
            }
            OwnedResource::TransientRelay => acp.transient_sessions().contains(&name),
            OwnedResource::DebugShell(id) => {
                !invalidated.contains(id)
                    && sessions.get(id).is_some_and(|status| status != "archived")
            }
        };
        if keep {
            continue;
        }

        match backend::kill_session(&name).await {
            Ok(()) => match resource {
                OwnedResource::Agent(_) | OwnedResource::TransientRelay => {
                    report.removed_agents += 1
                }
                OwnedResource::DebugShell(_) => report.removed_debug_shells += 1,
            },
            Err(error) => report.warnings.push(format!("{name}: {error}")),
        }
    }

    if report.invalidated_sessions > 0
        || report.removed_agents > 0
        || report.removed_debug_shells > 0
    {
        tracing::warn!(
            invalidated_sessions = report.invalidated_sessions,
            removed_agents = report.removed_agents,
            removed_debug_shells = report.removed_debug_shells,
            warnings = report.warnings.len(),
            "removed detached session resources without a live database owner"
        );
    }
    Ok(report)
}

/// Periodically converge detached supervisors after startup. A one-shot pass
/// runs before adoption in `server::run`; this loop handles a process crash or
/// a late provisioning/cancellation race after the server is already serving.
pub async fn run(db: Db, acp: crate::acp::AcpRegistry) {
    let start = tokio::time::Instant::now() + RECONCILE_INTERVAL;
    let mut interval = tokio::time::interval_at(start, RECONCILE_INTERVAL);
    loop {
        interval.tick().await;
        if let Err(error) = reconcile_supervisors(&db, &acp).await {
            tracing::warn!(%error, "session resource reconciliation failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_loom_owned_supervisor_names() {
        assert_eq!(
            owned_resource("weaver-abc-123"),
            Some(OwnedResource::Agent("abc-123"))
        );
        assert_eq!(
            owned_resource("weaver-acp-prompt-deadbeef"),
            Some(OwnedResource::TransientRelay)
        );
        assert_eq!(
            owned_resource("loom-shell-abc-123-7"),
            Some(OwnedResource::DebugShell("abc-123"))
        );
        assert_eq!(owned_resource("loom-scratch-shell"), None);
        assert_eq!(owned_resource("somebody-elses-tapestry"), None);
        assert_eq!(owned_resource("weaver-"), None);
        assert_eq!(owned_resource("loom-shell-id-not-a-number"), None);
    }
}
