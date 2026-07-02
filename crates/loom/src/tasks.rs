//! An in-memory registry of detached background tasks — currently the GitHub
//! `@loom` trigger launches, which run off the webhook request so a slow clone
//! can't blow GitHub's ~10s delivery timeout. Surfaced on the Debug page so an
//! operator can watch a webhook being handled *after* its `200` was returned —
//! the work that used to vanish into a spawned future with no trace.
//!
//! A capped ring (newest kept), lost on restart, exactly like the log buffer
//! ([`crate::logs`]). Not persisted — it is an observability aid, not a job
//! queue.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// Most task records retained; older ones are evicted.
const CAP: usize = 100;

/// One background task's lifecycle, as shown on the Debug page.
#[derive(Debug, Clone, Serialize)]
pub struct TaskRecord {
    pub id: u64,
    /// A coarse category, e.g. `github-trigger`.
    pub kind: String,
    /// A human label, e.g. `marin-community/marin#6823 (@rjpower)`.
    pub label: String,
    /// `running` | `done` | `error`.
    pub state: String,
    /// Outcome detail: a session id, `forwarded`, or an error message.
    pub detail: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Default)]
struct Inner {
    seq: u64,
    tasks: VecDeque<TaskRecord>,
}

/// The process-wide registry of background tasks.
pub struct Registry {
    inner: Mutex<Inner>,
}

impl Registry {
    fn new() -> Self {
        Registry {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Record a task starting; returns its id for a later [`Self::finish`].
    pub fn start(&self, kind: &str, label: &str) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.seq += 1;
        let id = inner.seq;
        inner.tasks.push_back(TaskRecord {
            id,
            kind: kind.to_string(),
            label: label.to_string(),
            state: "running".to_string(),
            detail: String::new(),
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
        });
        while inner.tasks.len() > CAP {
            inner.tasks.pop_front();
        }
        id
    }

    /// Mark task `id` finished with `state` (`done` | `error`) and a `detail`.
    /// A no-op if the record was already evicted.
    pub fn finish(&self, id: u64, state: &str, detail: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == id) {
            t.state = state.to_string();
            t.detail = detail.to_string();
            t.finished_at = Some(chrono::Utc::now().to_rfc3339());
        }
    }

    /// All records, newest first.
    pub fn snapshot(&self) -> Vec<TaskRecord> {
        let inner = self.inner.lock().unwrap();
        inner.tasks.iter().rev().cloned().collect()
    }
}

/// The process-wide task registry.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_finish_and_snapshot_newest_first() {
        let r = Registry::new();
        let a = r.start("github-trigger", "acme/widgets#1");
        let b = r.start("github-trigger", "acme/widgets#2");
        r.finish(a, "done", "session abc");

        let snap = r.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].id, b, "newest first");
        assert_eq!(snap[0].state, "running");
        assert_eq!(snap[1].id, a);
        assert_eq!(snap[1].state, "done");
        assert_eq!(snap[1].detail, "session abc");
        assert!(snap[1].finished_at.is_some());
    }

    #[test]
    fn evicts_oldest_beyond_cap() {
        let r = Registry::new();
        for i in 0..(CAP + 10) {
            r.start("t", &format!("job-{i}"));
        }
        let snap = r.snapshot();
        assert_eq!(snap.len(), CAP, "capped at CAP");
        assert_eq!(snap[0].label, format!("job-{}", CAP + 9), "newest kept");
    }

    #[test]
    fn finish_on_evicted_id_is_a_noop() {
        let r = Registry::new();
        // id 0 never existed.
        r.finish(0, "done", "nothing");
        assert!(r.snapshot().is_empty());
    }
}
