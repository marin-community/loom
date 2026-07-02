//! Operator and per-session **scratch shells** — plain login shells reachable
//! from the web UI over the same WebSocket terminal bridge agent sessions use.
//!
//! Two flavours share the launch machinery here:
//!
//! * The **operator scratch shell** ([`SHELL_SESSION`]): a single persistent
//!   shell, not tied to any branch or worktree, running in the container user's
//!   `$HOME`. Its purpose is one-time operator setup that would otherwise need
//!   `docker exec -it weaver …` — most concretely `gcloud auth login`, whose
//!   credentials land in the `CLOUDSDK_CONFIG` volume and so survive recreates
//!   (see the Dockerfile / docker-compose.yml). Spawned lazily by [`ensure`],
//!   reset with [`restart`].
//!
//! * Per-session **debug shells** ([`ensure_debug`]): one or more login shells
//!   *in a session's worktree*, carrying its `WEAVER_BRANCH`, for poking at the
//!   agent's checkout (run the tests, inspect the diff) beside the live agent.
//!   They are spawned lazily on first attach and swept with the session on
//!   archive/remove ([`kill_debug_all`]) so a worktree shell never outlives the
//!   worktree it sits in. The UI rediscovers open ones after a reload via
//!   [`list_debug`] (the supervisors are detached, like the agent terminal).
//!
//! Because tapestry supervisors are detached and outlive `loom server run`, a
//! shell — and any login or long-running command in it — persists across a loom
//! restart, exactly like an agent terminal.

use std::path::{Path, PathBuf};

use anyhow::Result;
use weaver_core::branch::Branch;

use crate::agent;
use crate::backend;
use crate::session::Session;
use crate::{agent_env, AppState};

/// The fixed supervisor name for the operator scratch shell. Distinct from any
/// agent session's `term_session` (those are random ids), so it never collides.
pub const SHELL_SESSION: &str = "loom-scratch-shell";

/// Where the scratch shell starts: the container user's `$HOME` (the persisted
/// `/home/app` volume). Falls back to `/` if `$HOME` is somehow unset. The cwd
/// barely matters for its main job — `gcloud`/`gh` write to global config paths
/// — but `$HOME` is the least surprising place to land.
fn shell_cwd() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Build the launch script for a scratch/debug login shell: export the operator
/// surface an agent session gets — `WEAVER_API` + `LOOM_TOKEN` so the in-place
/// `weaver`/`loom` CLIs work, the operator-managed [`agent_env`] vars, and (for a
/// per-session debug shell) the session's `WEAVER_BRANCH` — then `exec` the login
/// shell. This is a plain shell, not an agent, so it uses
/// [`agent::bare_shell_script`] (no inner agent command) rather than going through
/// the agent launch path.
async fn shell_script(st: &AppState, branch_id: Option<&str>) -> String {
    let api_url = format!("http://{}", st.addr);
    let local_token = agent::read_local_token();
    let extra = agent_env::pairs(&st.db).await.unwrap_or_default();

    let mut env: Vec<(&str, &str)> = vec![("WEAVER_API", api_url.as_str())];
    // A debug shell is rooted in a branch's worktree, so it gets that branch; the
    // operator scratch shell has none and is deliberately left without one.
    if let Some(branch) = branch_id {
        env.push(("WEAVER_BRANCH", branch));
    }
    if let Some(token) = local_token.as_deref() {
        env.push(("LOOM_TOKEN", token));
    }
    for (k, v) in &extra {
        env.push((k.as_str(), v.as_str()));
    }

    let loom_exe = std::env::current_exe().ok();
    let weaver_dir = loom_exe.as_deref().and_then(Path::parent);
    agent::bare_shell_script(&env, weaver_dir)
}

/// Ensure the scratch-shell supervisor is up, spawning it if not. Idempotent: a
/// live shell is left untouched (so reconnecting/refreshing the UI reattaches to
/// the same session rather than starting a new one).
pub async fn ensure(st: &AppState) -> Result<()> {
    if backend::has_session(SHELL_SESSION).await {
        return Ok(());
    }
    let script = shell_script(st, None).await;
    let cwd = shell_cwd();
    tracing::info!(session = SHELL_SESSION, cwd = %cwd.display(), "spawning operator scratch shell");
    backend::new_session(SHELL_SESSION, &cwd, &script).await
}

/// Reset the scratch shell: kill the current supervisor (best-effort) and bring
/// a fresh one up. Used by `POST /api/shell/restart` — e.g. to pick up newly
/// edited operator env vars, or to clear a wedged session.
pub async fn restart(st: &AppState) -> Result<()> {
    tracing::info!(session = SHELL_SESSION, "restarting operator scratch shell");
    backend::kill_session(SHELL_SESSION).await.ok();
    // The supervisor removes its socket as it exits; wait briefly for liveness to
    // drop so `ensure` actually respawns rather than re-adopting the dying one.
    for _ in 0..20 {
        if !backend::has_session(SHELL_SESSION).await {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    ensure(st).await
}

// ---------------------------------------------------------------------------
// Per-session worktree debug shells
// ---------------------------------------------------------------------------

/// Supervisor-name prefix for a session's worktree debug shells; each shell is
/// `…-<idx>`. The session id is a random slug, so this collides with neither the
/// agent terminal (`weaver-<id>`) nor the operator shell ([`SHELL_SESSION`]).
fn debug_prefix(session_id: &str) -> String {
    format!("loom-shell-{session_id}-")
}

/// The supervisor name for session `session_id`'s debug shell number `idx`.
pub fn debug_session(session_id: &str, idx: u32) -> String {
    format!("{}{idx}", debug_prefix(session_id))
}

/// Ensure session `session`'s worktree debug shell `idx` is up, spawning it if
/// not. Idempotent: a live shell is reattached (a refresh reconnects to the same
/// one). The shell lands in the session's worktree and carries `branch`'s
/// `WEAVER_BRANCH`, so its in-worktree `weaver`/`loom` resolve to this branch.
/// Returns the supervisor name to bridge to.
pub async fn ensure_debug(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    idx: u32,
) -> Result<String> {
    let name = debug_session(&session.id, idx);
    if backend::has_session(&name).await {
        return Ok(name);
    }
    let script = shell_script(st, Some(&branch.id)).await;
    let cwd = PathBuf::from(&session.work_dir);
    tracing::info!(session = %name, cwd = %cwd.display(), "spawning session debug shell");
    backend::new_session(&name, &cwd, &script).await?;
    Ok(name)
}

/// The live debug-shell indices for a session, parsed from the running
/// supervisor names and sorted ascending — so the UI can re-open the tabs after
/// a reload (the supervisors are detached and outlive the page). Never spawns.
pub async fn list_debug(session_id: &str) -> Vec<u32> {
    let prefix = debug_prefix(session_id);
    let mut idxs: Vec<u32> = backend::list_sessions()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|n| n.strip_prefix(&prefix).and_then(|s| s.parse().ok()))
        .collect();
    idxs.sort_unstable();
    idxs
}

/// Kill one of a session's debug shells (the UI's tab-close). Best-effort: a
/// missing supervisor is already gone.
pub async fn kill_debug(session_id: &str, idx: u32) {
    let name = debug_session(session_id, idx);
    tracing::info!(session = %name, "session debug shell killed");
    backend::kill_session(&name).await.ok();
}

/// Tear down every debug shell for a session — called from archive/remove
/// teardown so a worktree shell never outlives the worktree it sits in.
pub async fn kill_debug_all(session_id: &str) {
    let prefix = debug_prefix(session_id);
    let mut killed = 0u32;
    for name in backend::list_sessions().await.unwrap_or_default() {
        if name.starts_with(&prefix) {
            backend::kill_session(&name).await.ok();
            killed += 1;
        }
    }
    if killed > 0 {
        tracing::info!(
            session = session_id,
            count = killed,
            "session debug shells killed"
        );
    }
}
