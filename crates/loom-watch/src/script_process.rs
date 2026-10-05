//! Ownership and teardown of scheduled script subprocesses.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant};
use weaver_core::{db::Db, occurrence::Occurrence};

use weaver_core::process_identity::ProcessIdentity as Identity;

#[derive(Serialize, Deserialize)]
struct Lease {
    owner: Identity,
    child: Option<Identity>,
    token_id: Option<String>,
}

async fn lease(db: &Db, id: &str) -> Result<Option<Lease>> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT script_process FROM watch_occurrences WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await?;
    value
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(Into::into)
}

/// Only one daemon can change a queued script into running work.
pub(crate) async fn claim(db: &Db, occurrence: &Occurrence) -> Result<bool> {
    let owner = Lease {
        owner: Identity::read(std::process::id())?,
        child: None,
        token_id: None,
    };
    Ok(sqlx::query("UPDATE watch_occurrences SET status = 'running', script_process = ? WHERE id = ? AND status = 'pending' AND EXISTS(SELECT 1 FROM watches WHERE id = watch_id AND revision = ? AND (? = 0 OR (enabled = 1 AND paused = 0)))")
        .bind(serde_json::to_string(&owner)?).bind(&occurrence.id).bind(occurrence.revision).bind(occurrence.automatic).execute(db).await?.rows_affected() == 1)
}

pub(crate) async fn token(db: &Db, occurrence: &str, token_id: &str) -> Result<()> {
    let mut lease = lease(db, occurrence)
        .await?
        .ok_or_else(|| anyhow::anyhow!("script has no owner"))?;
    lease.token_id = Some(token_id.to_string());
    if sqlx::query(
        "UPDATE watch_occurrences SET script_process = ? WHERE id = ? AND status = 'running'",
    )
    .bind(serde_json::to_string(&lease)?)
    .bind(occurrence)
    .execute(db)
    .await?
    .rows_affected()
        != 1
    {
        crate::auth::revoke_engine_token(db, token_id).await?;
        bail!("script occurrence is no longer running");
    }
    Ok(())
}

/// The child waits on stdin until this durable record exists. A crash before
/// registration therefore cannot leave an untracked script executing actions.
pub(crate) async fn register(db: &Db, occurrence: &str, pid: u32) -> Result<()> {
    let mut lease = lease(db, occurrence)
        .await?
        .ok_or_else(|| anyhow::anyhow!("script has no owner"))?;
    lease.child = Some(Identity::read(pid)?);
    if sqlx::query(
        "UPDATE watch_occurrences SET script_process = ? WHERE id = ? AND status = 'running'",
    )
    .bind(serde_json::to_string(&lease)?)
    .bind(occurrence)
    .execute(db)
    .await?
    .rows_affected()
        != 1
    {
        bail!("script occurrence is no longer running");
    }
    Ok(())
}

pub(crate) async fn owner_alive(db: &Db, id: &str) -> Result<bool> {
    match lease(db, id).await? {
        Some(lease) => lease.owner.alive(),
        None => Ok(false),
    }
}

fn kill_group(pid: u32) -> Result<()> {
    // SAFETY: scheduled children are spawned with process_group(0); the
    // recorded child PID is their process group ID.
    if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error.into());
        }
    }
    Ok(())
}

fn group_alive(pid: u32) -> Result<bool> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            continue;
        };
        let fields: Vec<_> = fields.split_whitespace().collect();
        if fields.first().copied() != Some("Z")
            && fields
                .get(2)
                .is_some_and(|group| group.parse::<u32>() == Ok(pid))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Stop and confirm the whole subprocess group before releasing overlap.
pub(crate) async fn stop(db: &Db, id: &str) -> Result<()> {
    sqlx::query(
        "UPDATE watch_occurrences SET status = 'finishing' WHERE id = ? AND status = 'running'",
    )
    .bind(id)
    .execute(db)
    .await?;
    let Some(lease) = lease(db, id).await? else {
        return Ok(());
    };
    if let Some(token_id) = lease.token_id {
        crate::auth::revoke_engine_token(db, &token_id).await?;
    }
    let Some(child) = lease.child else {
        return Ok(());
    };
    // A recycled PID belongs to a new group. Never signal it. Linux retains a
    // group ID while its original members still exist, even after the leader exits.
    if child.boot != std::fs::read_to_string("/proc/sys/kernel/random/boot_id")? {
        return Ok(());
    }
    match Identity::read(child.pid) {
        Ok(current) if current.started != child.started || current.boot != child.boot => {
            return Ok(())
        }
        Ok(_) => {}
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    kill_group(child.pid)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while group_alive(child.pid)? {
        if Instant::now() >= deadline {
            bail!("script process group did not stop");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

/// Cancellation drops the future that owns the child; kill descendants too.
pub(crate) struct GroupGuard(pub Option<u32>);
impl GroupGuard {
    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }
}
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            if let Err(error) = kill_group(pid) {
                tracing::warn!(%error, "script group cleanup failed");
            }
        }
    }
}

pub(crate) async fn cancel_pending(db: &Db, occurrence: &Occurrence) -> Result<()> {
    let mut tx = db.begin().await?;
    let changed = sqlx::query("UPDATE watch_occurrences SET status = 'cancelled' WHERE id = ? AND status = 'pending' AND (deadline_at <= ? OR NOT EXISTS(SELECT 1 FROM watches WHERE id = watch_id AND revision = ? AND (? = 0 OR (enabled = 1 AND paused = 0))))")
        .bind(&occurrence.id).bind(weaver_core::db::now_iso()).bind(occurrence.revision).bind(occurrence.automatic).execute(&mut *tx).await?.rows_affected();
    if changed != 0 {
        sqlx::query("UPDATE watch_runs SET outcome = 'cancelled', summary = 'Watch changed, paused, or expired before execution', finished_at = ? WHERE id = ?")
            .bind(weaver_core::db::now_iso()).bind(occurrence.watch_run_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn complete(
    db: &Db,
    occurrence: &Occurrence,
    record: &weaver_core::watch::RunRecord<'_>,
    state: Option<&serde_json::Value>,
    wake_at: Option<Option<String>>,
) -> Result<()> {
    let mut tx = db.begin().await?;
    let status = if record.outcome == "error" {
        "error"
    } else {
        "ok"
    };
    let changed = sqlx::query(
        "UPDATE watch_occurrences SET status = ? WHERE id = ? AND status = 'finishing'",
    )
    .bind(status)
    .bind(&occurrence.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed != 0 {
        weaver_core::watch::finish_run_on(&mut *tx, occurrence.watch_run_id, record).await?;
        sqlx::query("UPDATE watches SET state = COALESCE(?, state), state_version = state_version + ?, wake_at = CASE WHEN ? THEN ? ELSE wake_at END WHERE id = ?")
            .bind(state.map(serde_json::Value::to_string)).bind(i64::from(state.is_some())).bind(wake_at.is_some()).bind(wake_at.flatten()).bind(&occurrence.watch_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
