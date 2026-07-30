//! Session lifecycle operations — archive, adopt, and warm-session creation.
//!
//! These are the state transitions a session goes through, factored out of the
//! HTTP handlers that used to own them. The callers that drive a session are
//! mostly *not* requests — the monitor's reaper, the GitHub merge path, the
//! restart-time adopt sweep, the watch engine — so keeping the transitions here
//! lets those paths run without reaching up into the web layer.
//!
//! Errors are plain [`anyhow::Error`]. A refusal the *caller* could have
//! avoided carries a [`Refusal`] inside it, so the REST adapter can recover the
//! status it used to return directly; anything else is a genuine 500.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

/// Why a lifecycle operation refused, when the reason is the caller's to fix.
///
/// These transitions are driven from requests *and* from background work, so
/// they cannot speak in HTTP types. Attaching one of these to the error lets the
/// REST adapter recover the status while the reaper and the watch engine go on
/// logging a message like any other failure.
#[derive(Debug)]
pub enum Refusal {
    /// The session is not in a state that permits this — 409.
    Conflict(String),
    /// The request itself is not admissible — 400.
    Invalid(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(m) | Self::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Refusal {}

use serde_json::{json, Value};

use crate::db::Db;
use crate::runtime::{layer_launch_environment, repo_cfg_or_default, set_env};
use crate::session::{self as session_mod, NewSession, Session};
use crate::AppState;
use crate::{agent, backend, custom_agents, db, events, git, repo};
use weaver_api::{LaunchOverrides, LaunchSelection};
use weaver_core::branch as branch_mod;
use weaver_core::branch::{Branch, TitleProvenance};
use weaver_core::tags;
use weaver_core::watch::Watch;

pub async fn delete_session_row(st: &AppState, session_id: &str) -> Result<()> {
    if let Some(revision) = session_mod::delete(&st.db, session_id).await? {
        crate::session_layout::publish_invalidation(&st.db, &st.bus, revision).await;
    }
    Ok(())
}

/// Build the explicit ambient baseline used when Tapestry clears inheritance.
/// Profile/repo values win over baseline and allowlisted ambient values; loom's
/// own session variables are injected later by `agent::session_env`.
pub(crate) async fn resume_environment(
    db: &Db,
    session: &Session,
    repo_root: &std::path::Path,
    cfg: &weaver_core::repo_config::RepoConfig,
) -> Vec<(String, String)> {
    let env = crate::runtime::launch_environment(
        db,
        repo_root,
        cfg,
        &session.profile,
        session.policy_strict,
        session.policy_restricted,
    )
    .await;
    if !session.policy_env_clear {
        return env;
    }
    let allowlist =
        serde_json::from_str::<Vec<String>>(&session.policy_ambient_allowlist).unwrap_or_default();
    crate::profile::cleared_environment(env, &allowlist)
}

pub(crate) async fn rotate_session_token(
    db: &Db,
    session: &Session,
    env: &mut Vec<(String, String)>,
) -> Result<()> {
    crate::auth::revoke_session_tokens(db, &session.id).await?;
    let token = crate::auth::create_session_token(
        db,
        session.created_by.as_deref(),
        &session.id,
        &session.branch_id,
    )
    .await?;
    set_env(env, "LOOM_TOKEN", token);
    Ok(())
}

/// Archive from a retention/integration path unless this branch carries the
/// explicit `auto-archive: disabled` opt-out. The check and teardown share the
/// lifecycle lock, so setting the label before an automatic operation acquires
/// the lock reliably prevents that operation; manual [`archive`] ignores it.
pub async fn auto_archive(
    st: &AppState,
    session: &Session,
    _branch: &Branch,
) -> Result<Option<Vec<String>>> {
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(anyhow!("session not found"));
    };
    if tags::auto_archive_disabled(&st.db, &current_branch.id).await? {
        tracing::info!(
            session = %current_session.id,
            branch = %current_branch.id,
            "automatic archive skipped by auto-archive: disabled tag"
        );
        return Ok(None);
    }
    archive_locked(st, &current_session, &current_branch)
        .await
        .map(Some)
}

/// Shared teardown after the caller has acquired the runtime lifecycle lock and
/// refreshed the session row.
pub async fn archive_locked(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> Result<Vec<String>> {
    tracing::info!(session = %session.id, branch = %branch.id, "archiving session");
    let mut warnings: Vec<String> = Vec::new();

    // Capture the agent's conversation log before teardown. The transcript lives
    // outside the worktree so it would survive removal, but capturing first keeps
    // it whole regardless. Best-effort: failures are warnings, never fatal.
    let (_, log_warnings) = crate::chatlog::capture(&st.db, session, branch).await;
    warnings.extend(log_warnings);
    tracing::debug!(session = %session.id, "captured conversation transcript before teardown");

    // Cancellation is the durable boundary: an automation request that
    // finishes provisioning after this point cannot promote itself.
    crate::runs::cancel_for_session_with_summary(&st.db, &session.id, "session archived").await?;
    // The row must never say `archived` while its supervisor is still live.
    // A tapestry kill is acknowledged before the socket disappears, so wait
    // for teardown and fail without flipping the row if it cannot complete.
    backend::kill_session_and_wait(&session.term_session).await?;
    // The killed relay makes its ACP task exit; remove any handle that has not
    // observed that edge yet. For a terminal session this is a no-op.
    if session.protocol == "acp" {
        st.acp.stop(&session.id);
    }
    crate::auth::revoke_session_tokens(&st.db, &session.id).await?;
    crate::shell::kill_debug_all(&session.id).await;
    st.ide.kill(&session.id);
    let repo_root = PathBuf::from(&branch.repo_root);
    let work_dir = PathBuf::from(&session.work_dir);
    tracing::debug!(session = %session.id, "killed terminal, debug shells, and ide sessions");
    if work_dir.exists() {
        tracing::debug!(session = %session.id, work_dir = %work_dir.display(), "removing worktree");
        if let Err(e) = git::worktree_remove(&repo_root, &work_dir).await {
            warnings.push(format!("worktree remove: {e}"));
            tokio::fs::remove_dir_all(&work_dir).await.ok();
        }
    }
    session_mod::set_status(&st.db, &session.id, "archived").await?;
    crate::channels::archive_session_channel(&st.db, &session.id).await?;
    // A torn-down session cannot keep owning work. Return every issue it held
    // to the repo backlog while preserving source-branch provenance and issue
    // status, just as full session deletion does.
    weaver_core::issue::unclaim_branch(&st.db, &branch.repo_root, &branch.branch).await?;
    // An archived session is finished with: its agent is gone, so it can no
    // longer "need me" — nor is it "resting". Clear every loud tag — the agent's
    // own `attention` and any watch's typed marks (loudness is value-driven, so
    // match on the value, not a fixed key set) — plus the soothing `idle` mark,
    // so the dashboard stops flagging or labelling a torn-down workstream —
    // absence is the calm state. The history (goal, status, events) is kept; the
    // `description` message stays too, as do any free-form quiet pills.
    for tag in tags::list(&st.db, &branch.id).await? {
        if tags::is_loud_value(&tag.value) || tag.key == tags::IDLE_KEY {
            tags::clear(&st.db, &branch.id, &tag.key).await?;
            events::record_tag(&st.db, &st.bus, &branch.id, &tag.key, "", "", "manual")
                .await
                .ok();
        }
    }
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": "archived", "reason": "session archived" }),
    )
    .await
    .ok();
    if warnings.is_empty() {
        tracing::info!(session = %session.id, branch = %branch.id, "session archived");
    } else {
        tracing::warn!(branch = %branch.id, warnings = warnings.len(), "session archived with warnings");
    }
    Ok(warnings)
}

/// Bring up an engine-managed (warm) session for a watch, reusing the same
/// branch/worktree/terminal launch machinery as an ordinary session — the only
/// differences are that it forks a dedicated `weaver/watch-<name>` branch
/// and the row is stamped `managed_by = watch.id` so the fleet listing and
/// every survey hide it.
///
/// A warm session is the watcher's own long-lived agent; its persistence across
/// rounds (the same terminal/worktree, resumed on adopt) is what gives the watch
/// across-round memory. The engine calls this once, on first need
/// ([`crate::watch::ensure_warm_session`]); thereafter it reuses the stored
/// session id.
pub(crate) async fn create_warm_session(
    st: &AppState,
    watch: &Watch,
    repo_root: &std::path::Path,
) -> Result<Session> {
    tracing::info!(watch = %watch.id, repo = %repo_root.display(), "creating warm session for watch");
    let repo_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let selected_profile = match watch.profile.trim() {
        "" => crate::profile::DEFAULT_PROFILE,
        name => name,
    };
    let _profile_permit = st.launch_gate.acquire_profile(selected_profile).await;
    let _resolver_permit = st.launch_gate.acquire_resolver().await;
    let selection = LaunchSelection {
        profile: selected_profile.to_string(),
        overrides: LaunchOverrides {
            model: (!watch.model.trim().is_empty()).then(|| watch.model.trim().to_string()),
            effort: (!watch.effort.trim().is_empty()).then(|| watch.effort.trim().to_string()),
            ..Default::default()
        },
    };
    let resolved = crate::launch::resolve(
        &st.db,
        &selection,
        &crate::launch::ResolveOptions {
            default_class: Some("automation".to_string()),
            ..Default::default()
        },
    )
    .await?;
    if !resolved.view.valid {
        return Err(anyhow!(Refusal::Conflict(
            resolved.view.errors.first().cloned().unwrap_or_else(|| {
                "warm session launch is not currently admissible".to_string()
            })
        )));
    }
    let profile_environment = crate::profile::env_pairs(&st.db, &resolved.profile.name)
        .await
        .map_err(|error| anyhow!(Refusal::Invalid(error.to_string())))?;
    let current_profile = crate::profile::get(&st.db, &resolved.profile.name)
        .await?
        .ok_or_else(|| {
            anyhow!(Refusal::Conflict(
                "watch profile changed during warm launch".to_string()
            ))
        })?;
    if current_profile.revision != resolved.view.profile_revision
        || current_profile.lifetime != resolved.view.profile_lifetime
    {
        return Err(anyhow!(Refusal::Conflict(
            "watch profile changed during warm launch; retry against a fresh resolution"
                .to_string(),
        )));
    }
    let launch_snapshot =
        crate::launch::serialize_snapshot(&resolved.view, resolved.custom_agent.as_ref())
            .map_err(|error| anyhow!(Refusal::Invalid(error.to_string())))?;
    let custom_agent = resolved.custom_agent.clone();
    let launch_profile = resolved.profile;
    let agent = resolved.view.agent;
    let model = resolved.view.model;
    let effort = resolved.view.effort;
    let protocol = resolved.view.protocol;
    let mode = resolved.view.mode;
    let class = resolved.view.class;
    let stamped_allowed_tools = serde_json::to_string(&resolved.runtime_permissions)
        .map_err(|error| anyhow!(Refusal::Invalid(error.to_string())))?;
    let stamped_mcp_access = serde_json::to_string(&resolved.mcp_policy)
        .map_err(|error| anyhow!(Refusal::Invalid(error.to_string())))?;

    let launch_permit = st.launch_gate.acquire(&repo_root).await;
    let repo_root_str = repo_root.display().to_string();
    let base = git::default_base(&repo_root).await?;

    // A stable, collision-resistant branch slug per watch; if an old warm
    // branch lingers (a prior warm session was archived), suffix to a fresh one.
    let base_slug = format!("watch-{}", branch_mod::slugify(&watch.name));
    let mut slug = base_slug.clone();
    let mut suffix = 2;
    loop {
        let branch_name = format!("weaver/{slug}");
        let dir = repo_root.join(".worktrees").join(&slug);
        if !git::branch_exists(&repo_root, &branch_name).await && !dir.exists() {
            break;
        }
        slug = format!("{base_slug}-{suffix}");
        suffix += 1;
    }
    let branch_name = format!("weaver/{slug}");
    let work_dir = repo_root.join(".worktrees").join(&slug);
    tokio::fs::create_dir_all(repo_root.join(".worktrees")).await?;
    git::ensure_excluded(&repo_root, ".worktrees/").await.ok();
    tracing::info!(watch = %watch.id, branch = %branch_name, work_dir = %work_dir.display(), "provisioning worktree for warm session");
    git::worktree_add(&repo_root, &work_dir, &branch_name, &base)
        .await
        .map_err(|e| anyhow!(Refusal::Invalid(e.to_string())))?;

    let branch = branch_mod::upsert(&st.db, &repo_root_str, &branch_name, &base).await?;
    branch_mod::set_title(
        &st.db,
        &branch.id,
        &format!("watch {}", watch.name),
        TitleProvenance::Derived,
    )
    .await?;
    tracing::debug!(watch = %watch.id, branch = %branch.id, "upserted warm session branch row");

    let session_id = branch_mod::new_id();
    let run_dir = db::run_dir(&session_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    tracing::debug!(watch = %watch.id, session = %session_id, "allocated warm session id and run dir");

    let goal_file = match watch
        .params()
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        Some(prompt) => {
            let f = run_dir.join("goal.txt");
            tokio::fs::write(&f, prompt).await?;
            Some(f)
        }
        None => None,
    };

    let term_session = format!("weaver-{session_id}");
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let mut extra_env = layer_launch_environment(
        &st.db,
        &repo_root,
        &repo_cfg,
        &launch_profile.name,
        profile_environment,
        launch_profile.strict,
        launch_profile.restricted,
    )
    .await;
    if launch_profile.env_clear {
        let allowlist = launch_profile
            .ambient_names()
            .map_err(|error| anyhow!(Refusal::Invalid(error.to_string())))?;
        extra_env = crate::profile::cleared_environment(extra_env, &allowlist);
    }

    // Persist before exposing the scoped credential to the child. Token lookup
    // deliberately requires a live bound session, so an eager agent cannot hit
    // a transient authentication failure during startup.
    let status = agent::initial_status(&st.db, &agent).await;
    let session = crate::session_layout::insert_session(
        &st.db,
        &st.bus,
        &NewSession {
            id: session_id.clone(),
            branch_id: branch.id.clone(),
            work_dir: work_dir.display().to_string(),
            term_session: term_session.clone(),
            agent_kind: agent.clone(),
            model: model.clone(),
            effort: effort.clone(),
            status: status.to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: Some(watch.id.clone()),
            created_by: None,
            protocol: protocol.clone(),
            origin: "watch".to_string(),
            class: class.clone(),
            tracking_issue_id: None,
        },
        &session_mod::SessionLaunchPolicy {
            profile: launch_profile.name.clone(),
            launch_mode: mode.clone(),
            profile_revision: launch_profile.revision,
            profile_lifetime: launch_profile.lifetime,
            strict: launch_profile.strict,
            env_clear: launch_profile.env_clear,
            ambient_allowlist: launch_profile.ambient_allowlist.clone(),
            idle_archive_secs: resolved.view.policy.idle_archive_secs,
            turn_budget: resolved.view.policy.turn_budget.unwrap_or(0),
            prelude: launch_profile.prelude.clone(),
            restricted: launch_profile.restricted,
            allowed_tools: stamped_allowed_tools.clone(),
            mcp_access: stamped_mcp_access,
            launch_snapshot,
            creator_kind: "system".to_string(),
            creator_subject: format!("watch:{}", watch.id),
            parent_session_id: None,
            automation_run_id: None,
        },
    )
    .await?;
    let session_token =
        crate::auth::create_session_token(&st.db, None, &session_id, &branch.id).await?;
    set_env(&mut extra_env, "LOOM_TOKEN", session_token);
    tracing::info!(watch = %watch.id, session = %session_id, agent = %agent, protocol = %protocol, work_dir = %work_dir.display(), "launching warm session agent");
    let launch_result = if protocol == "acp" {
        match agent::build_acp_launch(
            &st.db,
            &agent::AcpLaunchSpec {
                session_id: &session.id,
                branch_id: &branch.id,
                runtime: &agent,
                work_dir: &work_dir,
                server_addr: &st.addr,
                model: &model,
                effort: &effort,
                goal_file: goal_file.as_deref(),
                primer_file: None,
                extra_env: &extra_env,
                env_clear: launch_profile.env_clear,
                mode: &mode,
                prelude: &launch_profile.prelude,
                restricted: launch_profile.restricted,
                allowed_tools: &stamped_allowed_tools,
                mcp_access: &session.policy_mcp_access,
                custom: custom_agent.as_ref(),
            },
            agent::AcpOpen::Fresh,
        )
        .await
        {
            Ok(launch) => crate::acp::start(&st.acp_ctx(), &session.id, launch).await,
            Err(error) => Err(error),
        }
    } else {
        agent::launch(
            &st.db,
            &agent::LaunchSpec {
                branch_id: &branch.id,
                runtime: &agent,
                work_dir: &work_dir,
                term_session: &term_session,
                goal_file: goal_file.as_deref(),
                primer_file: None,
                prelude: &launch_profile.prelude,
                server_addr: &st.addr,
                model: &model,
                effort: &effort,
                extra_env: &extra_env,
                env_clear: launch_profile.env_clear,
                custom: custom_agent.as_ref(),
            },
            agent::LaunchMode::Fresh,
        )
        .await
    };
    if let Err(error) = launch_result {
        crate::auth::revoke_session_tokens(&st.db, &session_id)
            .await
            .ok();
        st.acp.stop(&session_id);
        backend::kill_session(&term_session).await.ok();
        delete_session_row(st, &session_id).await.ok();
        return Err(anyhow!(Refusal::Invalid(error.to_string(),)));
    }
    tracing::info!(watch = %watch.id, session = %session_id, "warm session agent launched");
    drop(launch_permit);

    repo::record_use(&st.db, &repo_root_str).await.ok();
    tracing::info!(
        watch = %watch.id,
        session = %session.id,
        "warm session created"
    );
    Ok(session)
}

/// Guard for [`adopt`] and [`recover`]: 409 when a *different* session on the
/// same branch is still active. Archived no longer occupies the branch slot, so
/// the slot may have been re-let since this session left the fleet — resuming it
/// then would collide on the worktree path and the one-active-session-per-branch
/// index.
pub async fn require_branch_slot_free(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> Result<()> {
    if let Some(other) = session_mod::active_for_branch(&st.db, &branch.id).await? {
        if other.id != session.id {
            return Err(anyhow!(Refusal::Conflict(format!(
                "branch '{}' already has an active session ({})",
                branch.branch, other.id
            ))));
        }
    }
    Ok(())
}

/// Prove that a respawn still targets the profile lifetime accepted by this
/// session. A same-lifetime edit, credential rotation, or retirement remains
/// valid; a recreate under the same name does not.
pub async fn require_session_profile_lifetime(
    db: &Db,
    session: &Session,
) -> Result<crate::profile::Profile> {
    let profile = crate::profile::get_including_retired(db, &session.profile)
        .await?
        .ok_or_else(|| {
            anyhow!(Refusal::Conflict(format!(
                "session '{}' profile lifetime is no longer available",
                session.profile
            )))
        })?;
    if session.profile_lifetime == 0 || profile.lifetime != session.profile_lifetime {
        return Err(anyhow!(Refusal::Conflict(format!(
            "session '{}' belongs to an unavailable profile lifetime; create a canonical replacement instead of reusing same-name credentials",
            session.id
        ))));
    }
    Ok(profile)
}

pub fn stamped_custom_agent(session: &Session) -> Result<Option<custom_agents::CustomAgent>> {
    if agent::builtin_agent_type(&session.agent_kind).is_some() {
        return Ok(None);
    }
    if session.launch_snapshot.trim().is_empty() {
        return Err(anyhow!(Refusal::Conflict(format!(
            "session '{}' has no captured custom-agent definition; create a canonical replacement instead of consulting the mutable registry",
            session.id
        ))));
    }
    let snapshot =
        crate::launch::deserialize_snapshot(&session.launch_snapshot).map_err(|error| {
            anyhow!(Refusal::Conflict(format!(
                "session '{}' has an unreadable launch snapshot: {error}",
                session.id
            )))
        })?;
    let custom = snapshot.custom_agent.ok_or_else(|| {
        anyhow!(Refusal::Conflict(format!(
            "session '{}' has no captured custom-agent definition; create a canonical replacement instead of consulting the mutable registry",
            session.id
        )))
    })?;
    if custom.name != session.agent_kind {
        return Err(anyhow!(Refusal::Conflict(format!(
            "session '{}' captured custom agent '{}' but is stamped as '{}'",
            session.id, custom.name, session.agent_kind
        ))));
    }
    Ok(Some(custom))
}

pub async fn require_resume_capacity(
    db: &Db,
    session: &Session,
    profile: &crate::profile::Profile,
) -> Result<()> {
    if profile.max_concurrent <= 0 {
        return Ok(());
    }
    let active = crate::profile::active_count(db, &profile.name).await?;
    let keeps_existing_slot = crate::profile::status_consumes_capacity(&session.status);
    if !keeps_existing_slot && active >= profile.max_concurrent {
        return Err(anyhow!(Refusal::Conflict(format!(
            "profile '{}' has reached its max_concurrent limit ({})",
            profile.name, profile.max_concurrent
        ))));
    }
    Ok(())
}

/// Recreate an orphaned session's terminal and resume its agent. The worktree is
/// expected to still be on disk (an orphaned session only lost its terminal); a
/// missing worktree is an error here — recovering a *torn-down* (archived)
/// session, which rebuilds the worktree first, goes through [`recover`].
pub async fn adopt(st: &AppState, session: &Session, _branch: &Branch) -> Result<()> {
    // Lock order shared with handoff/archive/delete: source session, global
    // lifecycle mutation, then profile lifetime/admission. Profile CRUD never
    // waits on a session/lifecycle lock, so this order cannot form a cycle.
    let _source_permit = st.launch_gate.acquire_session(&session.id).await;
    let _lifecycle = crate::runtime::LIFECYCLE_LOCK.lock().await;
    let Some((current_session, _current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(anyhow!("session not found"));
    };
    let session = &current_session;
    let _profile_permit = st.launch_gate.acquire_profile(&session.profile).await;
    let Some((current_session, current_branch)) =
        session_mod::with_branch(&st.db, &session.id).await?
    else {
        return Err(anyhow!("session not found"));
    };
    let session = &current_session;
    let branch = &current_branch;
    let profile = require_session_profile_lifetime(&st.db, session).await?;
    require_resume_capacity(&st.db, session, &profile).await?;
    let custom_agent = stamped_custom_agent(session)?;
    require_branch_slot_free(st, session, branch).await?;
    if session.protocol == "acp" {
        return adopt_acp(
            st,
            session,
            branch,
            "session adopted",
            custom_agent.as_ref(),
        )
        .await;
    }
    tracing::info!(session = %session.id, branch = %branch.id, "adopting orphaned session");
    if backend::has_session(&session.term_session).await {
        return Err(anyhow!(Refusal::Conflict(
            "session already has a running terminal process".to_string(),
        )));
    }
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Err(anyhow!(Refusal::Invalid(format!(
            "worktree {} no longer exists on disk — cannot adopt",
            session.work_dir
        ))));
    }
    tracing::debug!(session = %session.id, work_dir = %work_dir.display(), "adopt preflight checks passed");
    // The post-flip conversion: a terminal session whose builtin runtime now
    // declares acp is adopted *into* acp rather than back onto a PTY. Claude
    // reopens its own on-disk conversation (the adapter's session ids are
    // claude's ids); codex — which never had a scoped terminal resume — starts
    // fresh from the goal file. Custom agents and any runtime still declaring
    // terminal keep the PTY relaunch.
    let runtime = session.agent_kind.clone();
    let declares_acp = session.launch_snapshot.trim().is_empty()
        && matches!(
            agent::metadata_for(&st.db, &runtime).await?,
            Some(meta) if meta.builtin && meta.protocol == "acp"
        );
    if declares_acp {
        return adopt_terminal_into_acp(st, session, branch, &runtime).await;
    }
    resume_agent(
        st,
        session,
        branch,
        "session adopted",
        custom_agent.as_ref(),
    )
    .await
}

/// Convert an orphaned terminal session to ACP on adopt: respawn as a relay +
/// adapter, reopening claude's own on-disk conversation via `session/load` when
/// one is recorded for the worktree (else a fresh session re-oriented from the
/// goal file). The chat journal starts empty either way — a load replay is
/// suppressed, and the terminal era lives in the captured transcript — but the
/// agent-side context survives in full. The acp task's handshake stamps the row
/// (`protocol='acp'` + the adapter session id) once the reopen acks.
pub(crate) async fn adopt_terminal_into_acp(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    runtime: &str,
) -> Result<()> {
    tracing::info!(session = %session.id, branch = %branch.id, runtime = %runtime,
        "adopting terminal session into acp");
    let work_dir = PathBuf::from(&session.work_dir);
    let repo_root = PathBuf::from(&branch.repo_root);
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let mut extra_env = resume_environment(&st.db, session, &repo_root, &repo_cfg).await;
    rotate_session_token(&st.db, session, &mut extra_env).await?;
    let run_dir = db::run_dir(&session.id);
    let primer_file = stamped_primer_file(&run_dir, &session.policy_prelude);
    let goal_file = {
        let f = run_dir.join("goal.txt");
        f.exists().then_some(f)
    };
    // A fresh relay: no spool cursor, no in-flight turn.
    session_mod::set_ack_seq(&st.db, &session.id, 0).await.ok();
    session_mod::set_inflight(&st.db, &session.id, None)
        .await
        .ok();
    let open = if runtime == "claude" {
        match agent::claude_projects_dir()
            .and_then(|d| agent::latest_claude_session_id(&d, &work_dir))
        {
            Some(id) => {
                tracing::info!(session = %session.id, claude_session = %id,
                    "reopening claude's on-disk conversation");
                agent::AcpOpen::Load(id)
            }
            None => agent::AcpOpen::Fresh,
        }
    } else {
        agent::AcpOpen::Fresh
    };
    let launch = agent::build_acp_launch(
        &st.db,
        &agent::AcpLaunchSpec {
            session_id: &session.id,
            branch_id: &branch.id,
            runtime,
            work_dir: &work_dir,
            server_addr: &st.addr,
            model: &session.model,
            effort: &session.effort,
            goal_file: goal_file.as_deref(),
            primer_file: primer_file.as_deref(),
            extra_env: &extra_env,
            env_clear: session.policy_env_clear,
            // Terminal rows carry no mode; on adoption they take the acp default.
            mode: agent::DEFAULT_ACP_MODE,
            prelude: &session.policy_prelude,
            restricted: session.policy_restricted,
            allowed_tools: &session.policy_allowed_tools,
            mcp_access: &session.policy_mcp_access,
            custom: None,
        },
        open,
    )
    .await
    .map_err(|e| anyhow!(e.to_string()))?;
    crate::acp::start(&st.acp_ctx(), &session.id, launch)
        .await
        .map_err(|e| anyhow!(e.to_string()))?;
    session_mod::set_status(&st.db, &session.id, "running").await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": "running", "reason": "session adopted into acp" }),
    )
    .await
    .ok();
    Ok(())
}

/// Adopt an ACP session: respawn its relay + adapter and reopen the conversation.
/// When the relay supervisor is still alive but loom has no task for it (a crashed
/// task), just re-attach ([`crate::acp::attach`]). When the relay is gone, respawn
/// it and reopen via `session/load` (the adapter advertised `loadSession` and we
/// have its id), falling back to a fresh session re-oriented from the goal file.
pub async fn adopt_acp(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    reason: &str,
    custom_agent: Option<&custom_agents::CustomAgent>,
) -> Result<()> {
    tracing::info!(session = %session.id, branch = %branch.id, "adopting acp session");
    if st.acp.is_live(&session.id) {
        return Err(anyhow!(Refusal::Conflict(
            "session already has a live ACP task".to_string()
        )));
    }
    let work_dir = PathBuf::from(&session.work_dir);
    if !work_dir.exists() {
        return Err(anyhow!(Refusal::Conflict(format!(
            "worktree {} no longer exists on disk — cannot adopt",
            session.work_dir
        ))));
    }

    if backend::has_session(&session.term_session).await {
        // The relay outlived a crashed task — re-attach from the persisted cursor.
        tracing::info!(session = %session.id, "acp relay alive; re-attaching");
        crate::acp::attach(&st.acp_ctx(), &session.id)
            .await
            .map_err(|e| anyhow!(e.to_string()))?;
    } else {
        // The relay is gone — respawn the adapter and reopen the conversation.
        let repo_root = PathBuf::from(&branch.repo_root);
        let repo_cfg = repo_cfg_or_default(&repo_root);
        let mut extra_env = resume_environment(&st.db, session, &repo_root, &repo_cfg).await;
        rotate_session_token(&st.db, session, &mut extra_env).await?;
        let runtime = session.agent_kind.clone();
        let (primer_file, goal_file) = resume_prompt_files(st, session, branch).await;
        let mode = session
            .current_mode
            .clone()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| agent::DEFAULT_ACP_MODE.to_string());
        // A respawned relay has a fresh spool (seq 1..) and no in-flight turn —
        // reset the persisted cursor + inflight so a later attach replays cleanly.
        session_mod::set_ack_seq(&st.db, &session.id, 0).await.ok();
        session_mod::set_inflight(&st.db, &session.id, None)
            .await
            .ok();
        // Reopen via session/load where the adapter advertised it and we have an
        // id; otherwise a fresh session re-oriented from the goal file.
        let open = match session.acp_session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => agent::AcpOpen::Load(id.to_string()),
            None => agent::AcpOpen::Fresh,
        };
        let launch = agent::build_acp_launch(
            &st.db,
            &agent::AcpLaunchSpec {
                session_id: &session.id,
                branch_id: &branch.id,
                runtime: &runtime,
                work_dir: &work_dir,
                server_addr: &st.addr,
                model: &session.model,
                effort: &session.effort,
                goal_file: goal_file.as_deref(),
                primer_file: primer_file.as_deref(),
                extra_env: &extra_env,
                env_clear: session.policy_env_clear,
                mode: &mode,
                prelude: &session.policy_prelude,
                restricted: session.policy_restricted,
                allowed_tools: &session.policy_allowed_tools,
                mcp_access: &session.policy_mcp_access,
                custom: custom_agent,
            },
            open,
        )
        .await
        .map_err(|e| anyhow!(e.to_string()))?;
        crate::acp::start(&st.acp_ctx(), &session.id, launch)
            .await
            .map_err(|e| anyhow!(e.to_string()))?;
    }

    // A re-adopted ACP session is live again — mark it running.
    let status = agent::initial_status(&st.db, &session.agent_kind).await;
    session_mod::set_status(&st.db, &session.id, status).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": status, "reason": reason }),
    )
    .await
    .ok();
    tracing::info!(session = %session.id, branch = %branch.id, "acp session adopted");
    Ok(())
}

pub(crate) fn stamped_primer_file(run_dir: &std::path::Path, prelude: &str) -> Option<PathBuf> {
    if prelude != "weaver" {
        return None;
    }
    let file = run_dir.join("primer.txt");
    file.exists().then_some(file)
}

/// Resolve the persisted primer/goal files used to resume either backend. Refresh
/// the positional goal from the authoritative branch artifact first: an ACP
/// adapter that cannot load its old provider session falls back to this prompt in
/// exactly the same way as a native terminal resume.
pub(crate) async fn resume_prompt_files(
    st: &AppState,
    session: &Session,
    branch: &Branch,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let run_dir = db::run_dir(&session.id);
    let primer_file = stamped_primer_file(&run_dir, &session.policy_prelude);
    let goal_file = {
        let f = run_dir.join("goal.txt");
        if f.exists() {
            match branch_mod::current_goal(&st.db, branch).await {
                Ok(goal) => {
                    if let Err(e) = tokio::fs::write(&f, &goal).await {
                        tracing::warn!(error = %e, "failed to refresh goal.txt on resume");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to read goal for resume refresh"),
            }
            tracing::debug!(session = %session.id, "refreshed goal file for resume");
            Some(f)
        } else {
            None
        }
    };
    (primer_file, goal_file)
}

/// Re-launch a session's agent in a worktree that already exists on disk: the
/// shared tail of [`adopt`] (orphaned → resume) and [`recover`] (archived →
/// rebuild the worktree, then resume). `reason` is the status event's reason
/// string. Setup is never re-run here — the worktree is already provisioned; this
/// only resumes the agent (Claude via `--continue`, so it reloads its prior
/// conversation from the same cwd).
pub async fn resume_agent(
    st: &AppState,
    session: &Session,
    branch: &Branch,
    reason: &str,
    custom_agent: Option<&custom_agents::CustomAgent>,
) -> Result<()> {
    tracing::info!(session = %session.id, branch = %branch.id, reason = %reason, "resuming agent");
    let work_dir = PathBuf::from(&session.work_dir);
    // Restore the persisted positional prompt and any optional system primer.
    let (primer_file, goal_file) = resume_prompt_files(st, session, branch).await;
    // Re-launch with the same layered env the session started with, so a resumed
    // session keeps its per-repo / config-file environment (not just the global
    // agent_env). Setup is NOT re-run on adopt — the worktree is already
    // provisioned; this only resumes the agent.
    let repo_root = PathBuf::from(&branch.repo_root);
    let repo_cfg = repo_cfg_or_default(&repo_root);
    let mut extra_env = resume_environment(&st.db, session, &repo_root, &repo_cfg).await;
    rotate_session_token(&st.db, session, &mut extra_env).await?;
    let runtime = session.agent_kind.clone();
    tracing::info!(session = %session.id, branch = %branch.id, runtime = %runtime, work_dir = %work_dir.display(), "relaunching agent terminal for resume");
    agent::launch(
        &st.db,
        &agent::LaunchSpec {
            branch_id: &branch.id,
            runtime: &runtime,
            work_dir: &work_dir,
            term_session: &session.term_session,
            goal_file: goal_file.as_deref(),
            primer_file: primer_file.as_deref(),
            prelude: &session.policy_prelude,
            server_addr: &st.addr,
            model: &session.model,
            effort: &session.effort,
            extra_env: &extra_env,
            env_clear: session.policy_env_clear,
            custom: custom_agent,
        },
        agent::LaunchMode::Adopt,
    )
    .await
    .map_err(|e| anyhow!(e.to_string()))?;
    tracing::debug!(session = %session.id, "agent terminal relaunched, resuming conversation");
    // A resumed agent is already established and live — mark it `running`.
    let status = agent::initial_status(&st.db, &runtime).await;
    session_mod::set_status(&st.db, &session.id, status).await?;
    events::record(
        &st.db,
        &st.bus,
        &branch.id,
        "status",
        json!({ "status": status, "reason": reason }),
    )
    .await
    .ok();
    tracing::info!(session = %session.id, branch = %branch.id, reason = %reason, "session resumed");
    Ok(())
}
