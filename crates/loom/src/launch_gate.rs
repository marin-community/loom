//! Namespaced serialization for launch and attachment invariants.
//!
//! Git stores refs and worktree metadata in one repository-wide namespace.
//! Provisioning two sessions concurrently against the same checkout therefore
//! races clone/fetch, branch selection, and `git worktree add`. Profile
//! admission and per-session Scratch mutation have similar check-then-write
//! boundaries. One namespaced registry keeps those domains independent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum LaunchGateKey {
    Repo(PathBuf),
    Profile(String),
    Scratch(String),
}

#[derive(Clone, Default)]
pub struct RepoLaunchGate {
    locks: Arc<Mutex<HashMap<LaunchGateKey, Weak<Mutex<()>>>>>,
}

pub struct RepoLaunchPermit {
    _guard: OwnedMutexGuard<()>,
}

impl RepoLaunchGate {
    async fn acquire_key(&self, key: LaunchGateKey) -> RepoLaunchPermit {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(&key).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        RepoLaunchPermit {
            _guard: lock.lock_owned().await,
        }
    }

    /// Wait until no other new session is being provisioned for `repo`.
    ///
    /// The caller keeps the returned permit until the agent has started (or
    /// provisioning fails). Weak entries make the registry self-pruning once a
    /// repository has no active or waiting launches.
    pub async fn acquire(&self, repo: &Path) -> RepoLaunchPermit {
        self.acquire_key(LaunchGateKey::Repo(repo.to_path_buf()))
            .await
    }

    /// Serialize the capacity check and session insertion for one profile.
    pub async fn acquire_profile(&self, profile: &str) -> RepoLaunchPermit {
        self.acquire_key(LaunchGateKey::Profile(profile.to_string()))
            .await
    }

    /// Acquire several profile lifetime/admission permits in stable name order.
    /// Clone touches both source and target; global ordering prevents crossed
    /// clones from deadlocking while keeping each lifetime serialized.
    pub async fn acquire_profiles<'a>(
        &self,
        profiles: impl IntoIterator<Item = &'a str>,
    ) -> Vec<RepoLaunchPermit> {
        let mut names: Vec<&str> = profiles.into_iter().collect();
        names.sort_unstable();
        names.dedup();
        let mut permits = Vec::with_capacity(names.len());
        for name in names {
            permits.push(self.acquire_profile(name).await);
        }
        permits
    }

    /// Serialize limit validation and file mutation for one session's Scratch.
    pub async fn acquire_scratch(&self, session_id: &str) -> RepoLaunchPermit {
        self.acquire_key(LaunchGateKey::Scratch(session_id.to_string()))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn same_repo_waits_until_the_agent_start_permit_drops() {
        let gate = RepoLaunchGate::default();
        let first = gate.acquire(Path::new("/repos/one")).await;

        let waiting_gate = gate.clone();
        let waiter =
            tokio::spawn(async move { waiting_gate.acquire(Path::new("/repos/one")).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());

        drop(first);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("same-repo waiter should be released")
            .expect("waiter task should complete");
    }

    #[tokio::test]
    async fn different_repos_provision_independently() {
        let gate = RepoLaunchGate::default();
        let _first = gate.acquire(Path::new("/repos/one")).await;
        tokio::time::timeout(
            Duration::from_secs(1),
            gate.acquire(Path::new("/repos/two")),
        )
        .await
        .expect("a different repository must not wait");
    }

    #[tokio::test]
    async fn profile_and_scratch_namespaces_serialize_only_matching_keys() {
        let gate = RepoLaunchGate::default();
        let profile = gate.acquire_profile("ops").await;
        let scratch = gate.acquire_scratch("session-a").await;

        tokio::time::timeout(Duration::from_secs(1), gate.acquire_profile("interactive"))
            .await
            .expect("a different profile must not wait");
        tokio::time::timeout(Duration::from_secs(1), gate.acquire_scratch("session-b"))
            .await
            .expect("a different session must not wait");

        let waiting_profile = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire_profile("ops").await })
        };
        let waiting_scratch = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire_scratch("session-a").await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting_profile.is_finished());
        assert!(!waiting_scratch.is_finished());

        drop(profile);
        drop(scratch);
        tokio::time::timeout(Duration::from_secs(1), waiting_profile)
            .await
            .expect("matching profile waiter should be released")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), waiting_scratch)
            .await
            .expect("matching scratch waiter should be released")
            .unwrap();
    }
}
