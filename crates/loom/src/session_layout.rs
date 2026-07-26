//! Durable, shared Spaces → Groups → Sessions organization.
//!
//! Layout is deliberately independent from immutable session provenance and
//! ancestry. Every shared mutation checks one optimistic revision, performs
//! all moves and integer-rank renumbering in one immediate transaction, then
//! advances the revision exactly once.

use anyhow::Result;
use serde_json::json;
use sqlx::{FromRow, Row, SqliteConnection};
use std::collections::HashSet;
use weaver_api::{
    CreateSessionGroupReq, CreateSessionSpaceReq, DeleteSessionGroupReq, DeleteSessionSpaceReq,
    MoveSessionsReq, ReorderSessionLayoutReq, RestoreSessionGroupsReq, SessionGroupView,
    SessionLayoutItemKind, SessionLayoutView, SessionPlacementDefaultView,
    SessionPlacementSelectorKind, SessionPlacementView, SessionSpaceView,
    SetSessionPlacementDefaultReq, UpdateSessionGroupReq, UpdateSessionSpaceReq,
};

use crate::db::{now_iso, Db};
use crate::events::EventBus;
use crate::session::{NewSession, Session, SessionLaunchPolicy};

const USER_INBOX: &str = "group-user-inbox";

pub(crate) async fn publish_invalidation(db: &Db, bus: &EventBus, revision: i64) {
    crate::events::record_system(db, bus, "session_layout", json!({ "revision": revision }))
        .await
        .ok();
}

pub(crate) async fn insert_session(
    db: &Db,
    bus: &EventBus,
    session: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<Session> {
    let (session, revision) =
        crate::session::insert_with_layout_revision(db, session, policy).await?;
    if let Some(revision) = revision {
        publish_invalidation(db, bus, revision).await;
    }
    Ok(session)
}

#[derive(Debug)]
pub enum MutationError {
    Conflict,
    Invalid(String),
    Internal(anyhow::Error),
}

impl From<sqlx::Error> for MutationError {
    fn from(value: sqlx::Error) -> Self {
        Self::Internal(value.into())
    }
}

impl From<anyhow::Error> for MutationError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value)
    }
}

type MutationResult<T> = std::result::Result<T, MutationError>;

#[derive(Debug, FromRow)]
struct SpaceRow {
    id: String,
    name: String,
    rank: i64,
    system_key: Option<String>,
}

#[derive(Debug, FromRow)]
struct GroupRow {
    id: String,
    space_id: String,
    name: String,
    rank: i64,
    system_key: Option<String>,
    collapsed: bool,
}

fn clean_name(name: &str, what: &str) -> MutationResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(MutationError::Invalid(format!("{what} name is required")));
    }
    if name.chars().count() > 80 {
        return Err(MutationError::Invalid(format!(
            "{what} name must be 80 characters or fewer"
        )));
    }
    Ok(name.to_string())
}

async fn current_revision_tx(tx: &mut SqliteConnection) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
        .fetch_one(tx)
        .await
}

pub(crate) async fn bump_revision_tx(tx: &mut SqliteConnection) -> sqlx::Result<()> {
    sqlx::query("UPDATE session_layout_state SET revision = revision + 1 WHERE id = 1")
        .execute(tx)
        .await?;
    Ok(())
}

struct LayoutCommand<'a> {
    db: &'a Db,
    username: &'a str,
    tx: weaver_core::db::DbTransaction<'static>,
}

impl<'a> LayoutCommand<'a> {
    async fn begin(db: &'a Db, username: &'a str, expected_revision: i64) -> MutationResult<Self> {
        let mut tx = weaver_core::db::begin_immediate(db).await?;
        if current_revision_tx(&mut tx).await? != expected_revision {
            return Err(MutationError::Conflict);
        }
        Ok(Self { db, username, tx })
    }

    async fn commit(mut self) -> MutationResult<SessionLayoutView> {
        bump_revision_tx(&mut self.tx).await?;
        self.tx.commit().await?;
        Ok(get_layout(self.db, self.username).await?)
    }
}

pub async fn get_layout(db: &Db, username: &str) -> Result<SessionLayoutView> {
    // The revision and every row it labels must come from one SQLite snapshot.
    // Otherwise a writer can commit between these reads and produce revision N
    // with contents from N+1.
    let mut tx = db.begin().await?;
    let revision = sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
        .fetch_one(&mut *tx)
        .await?;
    let spaces = sqlx::query_as::<_, SpaceRow>(
        "SELECT id, name, rank, system_key FROM session_spaces ORDER BY rank, id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let groups = sqlx::query_as::<_, GroupRow>(
        "SELECT g.id, g.space_id, g.name, g.rank, g.system_key,
                COALESCE(pref.collapsed, 0) AS collapsed
         FROM session_groups g
         LEFT JOIN user_session_group_state pref
           ON pref.group_id = g.id AND pref.user_id = ?
         ORDER BY g.space_id, g.rank, g.id",
    )
    .bind(username)
    .fetch_all(&mut *tx)
    .await?;
    let mut group_views = Vec::with_capacity(groups.len());
    for group in groups {
        let session_ids = sqlx::query_scalar::<_, String>(
            "SELECT placement.session_id
             FROM session_placements placement
             JOIN sessions session ON session.id = placement.session_id
             WHERE placement.group_id = ? AND session.managed_by IS NULL
             ORDER BY placement.rank, placement.session_id",
        )
        .bind(&group.id)
        .fetch_all(&mut *tx)
        .await?;
        group_views.push(SessionGroupView {
            id: group.id,
            space_id: group.space_id,
            name: group.name,
            rank: group.rank,
            system_key: group.system_key,
            collapsed: group.collapsed,
            session_ids,
        });
    }
    let spaces = spaces
        .into_iter()
        .map(|space| SessionSpaceView {
            groups: group_views
                .iter()
                .filter(|group| group.space_id == space.id)
                .cloned()
                .collect(),
            id: space.id,
            name: space.name,
            rank: space.rank,
            system_key: space.system_key,
        })
        .collect();
    let defaults = sqlx::query(
        "SELECT selector_kind, selector_value, group_id
         FROM session_placement_defaults
         ORDER BY selector_kind, selector_value",
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|row| SessionPlacementDefaultView {
        selector_kind: row
            .get::<String, _>("selector_kind")
            .parse()
            .expect("migration only seeds supported placement selectors"),
        selector_value: row.get("selector_value"),
        group_id: row.get("group_id"),
    })
    .collect();
    let layout = SessionLayoutView {
        revision,
        spaces,
        defaults,
    };
    tx.commit().await?;
    Ok(layout)
}

pub async fn placement(db: &Db, session_id: &str) -> Result<Option<SessionPlacementView>> {
    let row = sqlx::query(
        "SELECT p.session_id, p.group_id, p.rank,
                g.name AS group_name, g.system_key AS group_system_key,
                s.id AS space_id, s.name AS space_name
         FROM session_placements p
         JOIN session_groups g ON g.id = p.group_id
         JOIN session_spaces s ON s.id = g.space_id
         WHERE p.session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| SessionPlacementView {
        session_id: row.get("session_id"),
        group_id: row.get("group_id"),
        group_name: row.get("group_name"),
        group_system_key: row.get("group_system_key"),
        space_id: row.get("space_id"),
        space_name: row.get("space_name"),
        rank: row.get("rank"),
    }))
}

/// Add the canonical placement inside the same transaction that inserts a
/// session. Delegated sessions inherit their launcher's group; configured
/// watch/profile/origin selectors then fall back to User / Inbox.
pub(crate) async fn insert_default_placement_tx(
    tx: &mut SqliteConnection,
    session: &NewSession,
    policy: &SessionLaunchPolicy,
) -> Result<Option<i64>> {
    // Warm watch sessions are engine infrastructure, not fleet membership.
    // Keeping them out of canonical placement also keeps their lifecycle from
    // advancing an otherwise unchanged visible layout revision.
    if session.managed_by.is_some() {
        return Ok(None);
    }
    let inherited = if session.origin == "agent" {
        if let Some(parent_id) = policy.parent_session_id.as_deref() {
            sqlx::query_scalar::<_, String>(
                "SELECT group_id FROM session_placements WHERE session_id = ?",
            )
            .bind(parent_id)
            .fetch_optional(&mut *tx)
            .await?
        } else {
            None
        }
    } else {
        None
    };
    let mut group_id = inherited;
    if group_id.is_none() {
        let watch_id = if let Some(run_id) = policy.automation_run_id.as_deref() {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT json_extract(request_json, '$.watch_id')
                 FROM automation_runs WHERE id = ?",
            )
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten()
        } else {
            None
        };
        let selectors = [
            watch_id.as_deref().map(|id| ("watch", id)),
            Some(("profile", policy.profile.as_str())),
            Some(("origin", session.origin.as_str())),
            Some(("origin", "*")),
        ];
        for selector in selectors.into_iter().flatten() {
            group_id = sqlx::query_scalar::<_, String>(
                "SELECT group_id FROM session_placement_defaults
                 WHERE selector_kind = ? AND selector_value = ?",
            )
            .bind(selector.0)
            .bind(selector.1)
            .fetch_optional(&mut *tx)
            .await?;
            if group_id.is_some() {
                break;
            }
        }
    }
    let group_id = group_id.unwrap_or_else(|| USER_INBOX.to_string());
    let rank: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(rank) + 1, 0) FROM session_placements WHERE group_id = ?",
    )
    .bind(&group_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_placements (session_id, group_id, rank, updated_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&session.id)
    .bind(group_id)
    .bind(rank)
    .bind(now_iso())
    .execute(&mut *tx)
    .await?;
    bump_revision_tx(tx).await?;
    Ok(Some(current_revision_tx(tx).await?))
}

pub async fn create_space(
    db: &Db,
    username: &str,
    req: &CreateSessionSpaceReq,
) -> MutationResult<SessionLayoutView> {
    let name = clean_name(&req.name, "space")?;
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM session_spaces WHERE name = ? COLLATE NOCASE)",
    )
    .bind(&name)
    .fetch_one(&mut *tx)
    .await?;
    if duplicate {
        return Err(MutationError::Invalid(format!(
            "a space named '{name}' already exists"
        )));
    }
    let space_id = format!("space-{}", weaver_core::branch::new_id());
    let group_id = format!("group-{}", weaver_core::branch::new_id());
    let rank: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(rank) + 1, 0) FROM session_spaces")
        .fetch_one(&mut *tx)
        .await?;
    let now = now_iso();
    sqlx::query(
        "INSERT INTO session_spaces (id, name, rank, system_key, created_at, updated_at)
         VALUES (?, ?, ?, NULL, ?, ?)",
    )
    .bind(&space_id)
    .bind(name)
    .bind(rank)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO session_groups
         (id, space_id, name, rank, system_key, created_at, updated_at)
         VALUES (?, ?, 'Inbox', 0, 'inbox', ?, ?)",
    )
    .bind(group_id)
    .bind(space_id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    command.commit().await
}

pub async fn update_space(
    db: &Db,
    username: &str,
    id: &str,
    req: &UpdateSessionSpaceReq,
) -> MutationResult<SessionLayoutView> {
    let name = clean_name(&req.name, "space")?;
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let result = sqlx::query(
        "UPDATE session_spaces SET name = ?, updated_at = ?
         WHERE id = ? AND NOT EXISTS (
             SELECT 1 FROM session_spaces other
             WHERE other.name = ? COLLATE NOCASE AND other.id != ?
         )",
    )
    .bind(&name)
    .bind(now_iso())
    .bind(id)
    .bind(&name)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() != 1 {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_spaces WHERE id = ?)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        return Err(MutationError::Invalid(if exists {
            format!("a space named '{name}' already exists")
        } else {
            "space not found".to_string()
        }));
    }
    command.commit().await
}

async fn ordered_sessions(tx: &mut SqliteConnection, group_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT placement.session_id
         FROM session_placements placement
         JOIN sessions session ON session.id = placement.session_id
         WHERE placement.group_id = ? AND session.managed_by IS NULL
         ORDER BY placement.rank, placement.session_id",
    )
    .bind(group_id)
    .fetch_all(tx)
    .await
}

async fn write_session_order(
    tx: &mut SqliteConnection,
    group_id: &str,
    session_ids: &[String],
) -> sqlx::Result<()> {
    let now = now_iso();
    for (rank, session_id) in session_ids.iter().enumerate() {
        sqlx::query(
            "UPDATE session_placements
             SET group_id = ?, rank = ?, updated_at = ?
             WHERE session_id = ?",
        )
        .bind(group_id)
        .bind(rank as i64)
        .bind(&now)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

async fn move_container_contents(
    tx: &mut SqliteConnection,
    source_group_ids: &[String],
    destination_group_id: Option<&str>,
    container_name: &str,
) -> MutationResult<()> {
    let placement_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_placements
         WHERE group_id IN (SELECT value FROM json_each(?))",
    )
    .bind(serde_json::to_string(source_group_ids).expect("string ids serialize"))
    .fetch_one(&mut *tx)
    .await?;
    let default_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_placement_defaults
         WHERE group_id IN (SELECT value FROM json_each(?))",
    )
    .bind(serde_json::to_string(source_group_ids).expect("string ids serialize"))
    .fetch_one(&mut *tx)
    .await?;
    if placement_count + default_count > 0 && destination_group_id.is_none() {
        return Err(MutationError::Invalid(format!(
            "deleting a non-empty {container_name} requires a destination group"
        )));
    }
    let Some(destination) = destination_group_id else {
        return Ok(());
    };
    if source_group_ids.iter().any(|id| id == destination) {
        return Err(MutationError::Invalid(
            "destination group must be outside the deleted container".to_string(),
        ));
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_groups WHERE id = ?)")
            .bind(destination)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        return Err(MutationError::Invalid(
            "destination group not found".to_string(),
        ));
    }
    let mut destination_order = ordered_sessions(tx, destination).await?;
    for source in source_group_ids {
        destination_order.extend(ordered_sessions(tx, source).await?);
    }
    write_session_order(tx, destination, &destination_order).await?;
    sqlx::query(
        "UPDATE session_placement_defaults SET group_id = ?
         WHERE group_id IN (SELECT value FROM json_each(?))",
    )
    .bind(destination)
    .bind(serde_json::to_string(source_group_ids).expect("string ids serialize"))
    .execute(&mut *tx)
    .await?;
    Ok(())
}

pub async fn delete_space(
    db: &Db,
    username: &str,
    id: &str,
    req: &DeleteSessionSpaceReq,
) -> MutationResult<SessionLayoutView> {
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let space_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_spaces")
        .fetch_one(&mut *tx)
        .await?;
    if space_count <= 1 {
        return Err(MutationError::Invalid(
            "the last space cannot be deleted".to_string(),
        ));
    }
    let groups = sqlx::query_scalar::<_, String>(
        "SELECT id FROM session_groups WHERE space_id = ? ORDER BY rank, id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    if groups.is_empty() {
        return Err(MutationError::Invalid("space not found".to_string()));
    }
    move_container_contents(tx, &groups, req.destination_group_id.as_deref(), "space").await?;
    sqlx::query("DELETE FROM session_spaces WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    renumber_spaces(tx).await?;
    command.commit().await
}

pub async fn create_group(
    db: &Db,
    username: &str,
    req: &CreateSessionGroupReq,
) -> MutationResult<SessionLayoutView> {
    let name = clean_name(&req.name, "group")?;
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let space_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_spaces WHERE id = ?)")
            .bind(&req.space_id)
            .fetch_one(&mut *tx)
            .await?;
    if !space_exists {
        return Err(MutationError::Invalid("space not found".to_string()));
    }
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM session_groups
             WHERE space_id = ? AND name = ? COLLATE NOCASE
         )",
    )
    .bind(&req.space_id)
    .bind(&name)
    .fetch_one(&mut *tx)
    .await?;
    if duplicate {
        return Err(MutationError::Invalid(format!(
            "a group named '{name}' already exists in this space"
        )));
    }
    let rank: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(rank) + 1, 0) FROM session_groups WHERE space_id = ?",
    )
    .bind(&req.space_id)
    .fetch_one(&mut *tx)
    .await?;
    let now = now_iso();
    sqlx::query(
        "INSERT INTO session_groups
         (id, space_id, name, rank, system_key, created_at, updated_at)
         VALUES (?, ?, ?, ?, NULL, ?, ?)",
    )
    .bind(format!("group-{}", weaver_core::branch::new_id()))
    .bind(&req.space_id)
    .bind(name)
    .bind(rank)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    command.commit().await
}

pub async fn update_group(
    db: &Db,
    username: &str,
    id: &str,
    req: &UpdateSessionGroupReq,
) -> MutationResult<SessionLayoutView> {
    let name = clean_name(&req.name, "group")?;
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let result = sqlx::query(
        "UPDATE session_groups SET name = ?, updated_at = ?
         WHERE id = ? AND NOT EXISTS (
             SELECT 1
             FROM session_groups other
             JOIN session_groups current ON current.id = ?
             WHERE other.space_id = current.space_id
               AND other.name = ? COLLATE NOCASE
               AND other.id != ?
         )",
    )
    .bind(&name)
    .bind(now_iso())
    .bind(id)
    .bind(id)
    .bind(&name)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() != 1 {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_groups WHERE id = ?)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        return Err(MutationError::Invalid(if exists {
            format!("a group named '{name}' already exists in this space")
        } else {
            "group not found".to_string()
        }));
    }
    command.commit().await
}

pub async fn delete_group(
    db: &Db,
    username: &str,
    id: &str,
    req: &DeleteSessionGroupReq,
) -> MutationResult<SessionLayoutView> {
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let space_id: Option<String> =
        sqlx::query_scalar("SELECT space_id FROM session_groups WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(space_id) = space_id else {
        return Err(MutationError::Invalid("group not found".to_string()));
    };
    let group_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM session_groups WHERE space_id = ?")
            .bind(&space_id)
            .fetch_one(&mut *tx)
            .await?;
    if group_count <= 1 {
        return Err(MutationError::Invalid(
            "the last group in a space cannot be deleted".to_string(),
        ));
    }
    move_container_contents(
        tx,
        &[id.to_string()],
        req.destination_group_id.as_deref(),
        "group",
    )
    .await?;
    sqlx::query("DELETE FROM session_groups WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    renumber_groups(tx, &space_id).await?;
    command.commit().await
}

async fn renumber_spaces(tx: &mut SqliteConnection) -> sqlx::Result<()> {
    let ids = sqlx::query_scalar::<_, String>("SELECT id FROM session_spaces ORDER BY rank, id")
        .fetch_all(&mut *tx)
        .await?;
    for (rank, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE session_spaces SET rank = ?, updated_at = ? WHERE id = ?")
            .bind(rank as i64)
            .bind(now_iso())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

async fn renumber_groups(tx: &mut SqliteConnection, space_id: &str) -> sqlx::Result<()> {
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT id FROM session_groups WHERE space_id = ? ORDER BY rank, id",
    )
    .bind(space_id)
    .fetch_all(&mut *tx)
    .await?;
    for (rank, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE session_groups SET rank = ?, updated_at = ? WHERE id = ?")
            .bind(rank as i64)
            .bind(now_iso())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

fn reposition(ids: &mut Vec<String>, id: &str, before_id: Option<&str>) -> MutationResult<()> {
    let Some(index) = ids.iter().position(|candidate| candidate == id) else {
        return Err(MutationError::Invalid("layout item not found".to_string()));
    };
    let item = ids.remove(index);
    let insert_at = if let Some(before) = before_id {
        ids.iter()
            .position(|candidate| candidate == before)
            .ok_or_else(|| MutationError::Invalid("reorder anchor not found".to_string()))?
    } else {
        ids.len()
    };
    ids.insert(insert_at, item);
    Ok(())
}

pub async fn reorder(
    db: &Db,
    username: &str,
    req: &ReorderSessionLayoutReq,
) -> MutationResult<SessionLayoutView> {
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    match req.kind {
        SessionLayoutItemKind::Space => {
            let mut ids =
                sqlx::query_scalar::<_, String>("SELECT id FROM session_spaces ORDER BY rank, id")
                    .fetch_all(&mut *tx)
                    .await?;
            reposition(&mut ids, &req.id, req.before_id.as_deref())?;
            for (rank, id) in ids.iter().enumerate() {
                sqlx::query("UPDATE session_spaces SET rank = ?, updated_at = ? WHERE id = ?")
                    .bind(rank as i64)
                    .bind(now_iso())
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        SessionLayoutItemKind::Group => {
            let group =
                sqlx::query("SELECT space_id, name, system_key FROM session_groups WHERE id = ?")
                    .bind(&req.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some(group) = group else {
                return Err(MutationError::Invalid("group not found".to_string()));
            };
            let source_space: String = group.get("space_id");
            let group_name: String = group.get("name");
            let system_key: Option<String> = group.get("system_key");
            let destination_space = req
                .destination_space_id
                .as_deref()
                .unwrap_or(&source_space)
                .to_string();
            let destination_exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_spaces WHERE id = ?)")
                    .bind(&destination_space)
                    .fetch_one(&mut *tx)
                    .await?;
            if !destination_exists {
                return Err(MutationError::Invalid(
                    "destination space not found".to_string(),
                ));
            }
            if source_space != destination_space {
                if let Some(system_key) = system_key.as_deref() {
                    let collision: bool = sqlx::query_scalar(
                        "SELECT EXISTS(
                            SELECT 1 FROM session_groups
                            WHERE space_id = ? AND system_key = ? AND id != ?
                         )",
                    )
                    .bind(&destination_space)
                    .bind(system_key)
                    .bind(&req.id)
                    .fetch_one(&mut *tx)
                    .await?;
                    if collision {
                        return Err(MutationError::Invalid(format!(
                            "destination space already has a group with system key '{system_key}'"
                        )));
                    }
                }
                let name_collision: bool = sqlx::query_scalar(
                    "SELECT EXISTS(
                        SELECT 1 FROM session_groups
                        WHERE space_id = ? AND name = ? AND id != ?
                     )",
                )
                .bind(&destination_space)
                .bind(&group_name)
                .bind(&req.id)
                .fetch_one(&mut *tx)
                .await?;
                if name_collision {
                    return Err(MutationError::Invalid(format!(
                        "destination space already has a group named '{group_name}'"
                    )));
                }
                let source_count: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM session_groups WHERE space_id = ?")
                        .bind(&source_space)
                        .fetch_one(&mut *tx)
                        .await?;
                if source_count <= 1 {
                    return Err(MutationError::Invalid(
                        "the last group cannot be moved out of a space".to_string(),
                    ));
                }
                sqlx::query(
                    "UPDATE session_groups SET space_id = ?, rank = 2147483647, updated_at = ?
                     WHERE id = ?",
                )
                .bind(&destination_space)
                .bind(now_iso())
                .bind(&req.id)
                .execute(&mut *tx)
                .await?;
                renumber_groups(tx, &source_space).await?;
            }
            let mut ids = sqlx::query_scalar::<_, String>(
                "SELECT id FROM session_groups WHERE space_id = ? ORDER BY rank, id",
            )
            .bind(&destination_space)
            .fetch_all(&mut *tx)
            .await?;
            reposition(&mut ids, &req.id, req.before_id.as_deref())?;
            for (rank, id) in ids.iter().enumerate() {
                sqlx::query("UPDATE session_groups SET rank = ?, updated_at = ? WHERE id = ?")
                    .bind(rank as i64)
                    .bind(now_iso())
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    command.commit().await
}

pub async fn move_sessions(
    db: &Db,
    username: &str,
    req: &MoveSessionsReq,
) -> MutationResult<SessionLayoutView> {
    if req.session_ids.is_empty() {
        return Err(MutationError::Invalid(
            "at least one session is required".to_string(),
        ));
    }
    let unique: HashSet<&str> = req.session_ids.iter().map(String::as_str).collect();
    if unique.len() != req.session_ids.len() {
        return Err(MutationError::Invalid(
            "session ids must be unique".to_string(),
        ));
    }
    if req
        .before_session_id
        .as_deref()
        .is_some_and(|anchor| unique.contains(anchor))
    {
        return Err(MutationError::Invalid(
            "the insertion anchor cannot also be moved".to_string(),
        ));
    }
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let destination_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_groups WHERE id = ?)")
            .bind(&req.destination_group_id)
            .fetch_one(&mut *tx)
            .await?;
    if !destination_exists {
        return Err(MutationError::Invalid(
            "destination group not found".to_string(),
        ));
    }
    let source_rows = sqlx::query(
        "SELECT p.session_id, p.group_id, s.managed_by
         FROM session_placements p
         JOIN sessions s ON s.id = p.session_id
         WHERE p.session_id IN (SELECT value FROM json_each(?))",
    )
    .bind(serde_json::to_string(&req.session_ids).expect("string ids serialize"))
    .fetch_all(&mut *tx)
    .await?;
    if source_rows.len() != req.session_ids.len() {
        return Err(MutationError::Invalid(
            "one or more sessions were not found".to_string(),
        ));
    }
    if source_rows
        .iter()
        .any(|row| row.get::<Option<String>, _>("managed_by").is_some())
    {
        return Err(MutationError::Invalid(
            "engine-managed sessions cannot be moved".to_string(),
        ));
    }
    let source_groups: HashSet<String> =
        source_rows.iter().map(|row| row.get("group_id")).collect();
    let mut destination_order = ordered_sessions(tx, &req.destination_group_id).await?;
    destination_order.retain(|id| !unique.contains(id.as_str()));
    let insert_at = if let Some(anchor) = req.before_session_id.as_deref() {
        destination_order
            .iter()
            .position(|id| id == anchor)
            .ok_or_else(|| {
                MutationError::Invalid(
                    "insertion anchor is not in the destination group".to_string(),
                )
            })?
    } else {
        destination_order.len()
    };
    destination_order.splice(insert_at..insert_at, req.session_ids.iter().cloned());
    write_session_order(tx, &req.destination_group_id, &destination_order).await?;
    for source in source_groups {
        if source == req.destination_group_id {
            continue;
        }
        let order = ordered_sessions(tx, &source).await?;
        write_session_order(tx, &source, &order).await?;
    }
    command.commit().await
}

pub async fn restore_groups(
    db: &Db,
    username: &str,
    req: &RestoreSessionGroupsReq,
) -> MutationResult<SessionLayoutView> {
    if req.groups.is_empty() {
        return Err(MutationError::Invalid(
            "at least one group snapshot is required".to_string(),
        ));
    }
    let group_ids: HashSet<&str> = req
        .groups
        .iter()
        .map(|group| group.group_id.as_str())
        .collect();
    if group_ids.len() != req.groups.len() {
        return Err(MutationError::Invalid(
            "group snapshot ids must be unique".to_string(),
        ));
    }
    let desired_ids: HashSet<&str> = req
        .groups
        .iter()
        .flat_map(|group| group.session_ids.iter().map(String::as_str))
        .collect();
    let desired_count: usize = req.groups.iter().map(|group| group.session_ids.len()).sum();
    if desired_ids.len() != desired_count {
        return Err(MutationError::Invalid(
            "session ids must be unique across the restore".to_string(),
        ));
    }

    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    for group_id in &group_ids {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_groups WHERE id = ?)")
                .bind(group_id)
                .fetch_one(&mut *tx)
                .await?;
        if !exists {
            return Err(MutationError::Invalid(format!(
                "restore group '{group_id}' not found"
            )));
        }
    }

    let mut current_ids = HashSet::new();
    for group_id in &group_ids {
        let ids = sqlx::query_scalar::<_, String>(
            "SELECT p.session_id
             FROM session_placements p
             JOIN sessions s ON s.id = p.session_id
             WHERE p.group_id = ? AND s.managed_by IS NULL",
        )
        .bind(group_id)
        .fetch_all(&mut *tx)
        .await?;
        current_ids.extend(ids);
    }
    let desired_owned: HashSet<String> = desired_ids.into_iter().map(str::to_string).collect();
    if current_ids != desired_owned {
        return Err(MutationError::Conflict);
    }

    for group in &req.groups {
        for (rank, session_id) in group.session_ids.iter().enumerate() {
            sqlx::query(
                "UPDATE session_placements
                 SET group_id = ?, rank = ?, updated_at = ?
                 WHERE session_id = ?",
            )
            .bind(&group.group_id)
            .bind(rank as i64)
            .bind(now_iso())
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    command.commit().await
}

pub async fn set_preference(
    db: &Db,
    username: &str,
    group_id: &str,
    collapsed: bool,
) -> Result<SessionLayoutView> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_groups WHERE id = ?)")
            .bind(group_id)
            .fetch_one(db)
            .await?;
    anyhow::ensure!(exists, "group not found");
    sqlx::query(
        "INSERT INTO user_session_group_state (user_id, group_id, collapsed, updated_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(user_id, group_id) DO UPDATE
         SET collapsed = excluded.collapsed, updated_at = excluded.updated_at",
    )
    .bind(username)
    .bind(group_id)
    .bind(collapsed)
    .bind(now_iso())
    .execute(db)
    .await?;
    get_layout(db, username).await
}

pub async fn set_default(
    db: &Db,
    username: &str,
    req: &SetSessionPlacementDefaultReq,
) -> MutationResult<SessionLayoutView> {
    let value = req.selector_value.trim();
    if value.is_empty() {
        return Err(MutationError::Invalid(
            "selector value is required".to_string(),
        ));
    }
    let mut command = LayoutCommand::begin(db, username, req.expected_revision).await?;
    let tx = &mut *command.tx;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_groups WHERE id = ?)")
            .bind(&req.group_id)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        return Err(MutationError::Invalid("group not found".to_string()));
    }
    sqlx::query(
        "INSERT INTO session_placement_defaults
         (selector_kind, selector_value, group_id) VALUES (?, ?, ?)
         ON CONFLICT(selector_kind, selector_value)
         DO UPDATE SET group_id = excluded.group_id",
    )
    .bind(req.selector_kind.as_str())
    .bind(value)
    .bind(&req.group_id)
    .execute(&mut *tx)
    .await?;
    command.commit().await
}

pub async fn delete_default(
    db: &Db,
    username: &str,
    selector_kind: SessionPlacementSelectorKind,
    selector_value: &str,
    expected_revision: i64,
) -> MutationResult<SessionLayoutView> {
    if selector_kind == SessionPlacementSelectorKind::Origin && selector_value == "*" {
        return Err(MutationError::Invalid(
            "the fallback origin default cannot be removed".to_string(),
        ));
    }
    let mut command = LayoutCommand::begin(db, username, expected_revision).await?;
    let tx = &mut *command.tx;
    let result = sqlx::query(
        "DELETE FROM session_placement_defaults
         WHERE selector_kind = ? AND selector_value = ?",
    )
    .bind(selector_kind.as_str())
    .bind(selector_value)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() != 1 {
        return Err(MutationError::Invalid(
            "placement default not found".to_string(),
        ));
    }
    command.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaver_api::SessionGroupOrderReq;

    async fn seed_placement(db: &Db, session_id: &str, group_id: &str, rank: i64) {
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch, created_at, updated_at)
             VALUES (?, '/repo', ?, '2026-01-01T00:00:00.000Z',
                     '2026-01-01T00:00:00.000Z')",
        )
        .bind(session_id)
        .bind(session_id)
        .execute(db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status)
             VALUES (?, ?, ?, ?, 'done')",
        )
        .bind(session_id)
        .bind(session_id)
        .bind(format!("/{session_id}"))
        .bind(format!("term-{session_id}"))
        .execute(db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_placements (session_id, group_id, rank, updated_at)
             VALUES (?, ?, ?, '2026-01-01T00:00:00.000Z')",
        )
        .bind(session_id)
        .bind(group_id)
        .bind(rank)
        .execute(db)
        .await
        .unwrap();
    }

    fn order(layout: &SessionLayoutView, group_id: &str) -> Vec<String> {
        layout
            .spaces
            .iter()
            .flat_map(|space| &space.groups)
            .find(|group| group.id == group_id)
            .unwrap()
            .session_ids
            .clone()
    }

    #[tokio::test]
    async fn moves_preserve_order_fence_stale_revisions_and_restore_atomically() {
        let db = crate::db::connect_in_memory().await.unwrap();
        seed_placement(&db, "a", "group-user-inbox", 0).await;
        seed_placement(&db, "b", "group-user-inbox", 1).await;
        seed_placement(&db, "c", "group-github-inbox", 0).await;

        let moved = move_sessions(
            &db,
            "operator",
            &MoveSessionsReq {
                session_ids: vec!["b".to_string(), "a".to_string()],
                destination_group_id: "group-github-inbox".to_string(),
                before_session_id: Some("c".to_string()),
                expected_revision: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(moved.revision, 2);
        assert_eq!(order(&moved, "group-github-inbox"), ["b", "a", "c"]);
        assert!(matches!(
            move_sessions(
                &db,
                "operator",
                &MoveSessionsReq {
                    session_ids: vec!["a".to_string()],
                    destination_group_id: "group-user-inbox".to_string(),
                    before_session_id: None,
                    expected_revision: 1,
                },
            )
            .await,
            Err(MutationError::Conflict)
        ));

        sqlx::query(
            "CREATE TRIGGER fail_second_restore
             BEFORE UPDATE OF group_id ON session_placements
             WHEN NEW.session_id = 'b'
             BEGIN SELECT RAISE(FAIL, 'injected restore failure'); END",
        )
        .execute(&db)
        .await
        .unwrap();
        let restore = RestoreSessionGroupsReq {
            groups: vec![
                SessionGroupOrderReq {
                    group_id: "group-user-inbox".to_string(),
                    session_ids: vec!["a".to_string(), "b".to_string()],
                },
                SessionGroupOrderReq {
                    group_id: "group-github-inbox".to_string(),
                    session_ids: vec!["c".to_string()],
                },
            ],
            expected_revision: 2,
        };
        assert!(matches!(
            restore_groups(&db, "operator", &restore).await,
            Err(MutationError::Internal(_))
        ));
        let unchanged = get_layout(&db, "operator").await.unwrap();
        assert_eq!(unchanged.revision, 2);
        assert_eq!(order(&unchanged, "group-github-inbox"), ["b", "a", "c"]);

        sqlx::query("DROP TRIGGER fail_second_restore")
            .execute(&db)
            .await
            .unwrap();
        let restored = restore_groups(&db, "operator", &restore).await.unwrap();
        assert_eq!(restored.revision, 3);
        assert_eq!(order(&restored, "group-user-inbox"), ["a", "b"]);
        assert_eq!(order(&restored, "group-github-inbox"), ["c"]);
    }

    #[tokio::test]
    async fn cross_space_group_moves_preflight_both_collision_kinds() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let user_review = create_group(
            &db,
            "operator",
            &CreateSessionGroupReq {
                space_id: "space-user".to_string(),
                name: "Review".to_string(),
                expected_revision: 1,
            },
        )
        .await
        .unwrap();
        let review_id = user_review
            .spaces
            .iter()
            .flat_map(|space| &space.groups)
            .find(|group| group.space_id == "space-user" && group.name == "Review")
            .unwrap()
            .id
            .clone();
        let ops_review = create_group(
            &db,
            "operator",
            &CreateSessionGroupReq {
                space_id: "space-ops".to_string(),
                name: "Review".to_string(),
                expected_revision: user_review.revision,
            },
        )
        .await
        .unwrap();
        let move_group = |id: &str, destination: &str, revision| ReorderSessionLayoutReq {
            kind: SessionLayoutItemKind::Group,
            id: id.to_string(),
            before_id: None,
            destination_space_id: Some(destination.to_string()),
            expected_revision: revision,
        };
        assert!(matches!(
            reorder(
                &db,
                "operator",
                &move_group(&review_id, "space-ops", ops_review.revision),
            )
            .await,
            Err(MutationError::Invalid(message)) if message.contains("named 'Review'")
        ));
        assert!(matches!(
            reorder(
                &db,
                "operator",
                &move_group(
                    "group-user-inbox",
                    "space-github",
                    ops_review.revision,
                ),
            )
            .await,
            Err(MutationError::Invalid(message)) if message.contains("system key 'inbox'")
        ));

        let unique = create_group(
            &db,
            "operator",
            &CreateSessionGroupReq {
                space_id: "space-user".to_string(),
                name: "Unique".to_string(),
                expected_revision: ops_review.revision,
            },
        )
        .await
        .unwrap();
        let unique_id = unique
            .spaces
            .iter()
            .flat_map(|space| &space.groups)
            .find(|group| group.space_id == "space-user" && group.name == "Unique")
            .unwrap()
            .id
            .clone();
        let crossed = reorder(
            &db,
            "operator",
            &move_group(&unique_id, "space-ops", unique.revision),
        )
        .await
        .unwrap();
        assert_eq!(crossed.revision, unique.revision + 1);
        assert!(crossed
            .spaces
            .iter()
            .find(|space| space.id == "space-ops")
            .unwrap()
            .groups
            .iter()
            .any(|group| group.id == unique_id));
    }
}
