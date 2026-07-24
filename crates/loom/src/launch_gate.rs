//! Per-repository serialization for new session provisioning.
//!
//! Git stores refs and worktree metadata in one repository-wide namespace.
//! Provisioning two sessions concurrently against the same checkout therefore
//! races clone/fetch, branch selection, and `git worktree add`. Different
//! repositories remain independent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Clone, Default)]
pub struct RepoLaunchGate {
    locks: Arc<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>>,
}

pub struct RepoLaunchPermit {
    _guard: OwnedMutexGuard<()>,
}

impl RepoLaunchGate {
    /// Wait until no other new session is being provisioned for `repo`.
    ///
    /// The caller keeps the returned permit until the agent has started (or
    /// provisioning fails). Weak entries make the registry self-pruning once a
    /// repository has no active or waiting launches.
    pub async fn acquire(&self, repo: &Path) -> RepoLaunchPermit {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(repo).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(repo.to_path_buf(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        RepoLaunchPermit {
            _guard: lock.lock_owned().await,
        }
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
}
