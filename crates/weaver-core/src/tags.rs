//! Tags: a general per-branch annotation.
//!
//! A tag is a single-valued `(key, value)` fact stamped on a branch, with a
//! one-line `note`, the author (`set_by`), and a timestamp (`set_at`). It
//! collapses what used to be two near-identical status axes — the agent's
//! `attention` self-report and a watch's `triage` assessment — into one
//! mechanism: those become two **well-known keys** (see [`ATTENTION_KEY`] /
//! [`TRIAGE_KEY`]), and a new axis (priority, needs-rebase, …) costs zero
//! schema.
//!
//! **Absence is the calm/default state.** There is no stored `ok`: clearing a
//! tag is [`clear`], which deletes the row, so "ok ⇒ no tag" is structural. The
//! branch's prose status message lives on [`crate::branch::Branch::description`]
//! and is independent of any tag.
//!
//! **Staleness is generic**, computed by callers: a tag is stale once
//! `set_at < last_activity_at` (the session moved on since it was set).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::{now_iso, Db};

/// One tag row: a `(key, value)` annotation on a branch with attribution.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tag {
    /// The axis, e.g. [`ATTENTION_KEY`] or [`TRIAGE_KEY`], or any free-form key.
    pub key: String,
    /// The level/payload. For the loud keys, one of [`ATTENTION_VALUES`].
    pub value: String,
    /// One-line reason accompanying the tag.
    pub note: String,
    /// Who set it — `agent`, a watch name, or `manual`. Attribution.
    pub set_by: String,
    /// When it was last set. Compared against a session's last activity to render
    /// the tag stale once the session has moved past it.
    pub set_at: String,
}

// ---------------------------------------------------------------------------
// Registry of well-known keys
// ---------------------------------------------------------------------------

/// The agent's self-reported attention level — "does this need me?". Authored by
/// the agent via `weaver status`. Loud (raises a badge).
pub const ATTENTION_KEY: &str = "attention";

/// A watch's (or `manual`) outside assessment of a branch — a second axis
/// distinct from the agent's own [`ATTENTION_KEY`]. Loud (raises a badge). Its
/// `note`/`set_by`/`set_at` carry the mark's reason, attribution, and staleness
/// anchor.
pub const TRIAGE_KEY: &str = "triage";

/// A soothing, **quiet** mark stamped mechanically when the agent goes quiet (a
/// finished turn or a `waiting` lull — see loom's `apply_hook`). It is the calm
/// "this agent is resting, no one is needed" signal — deliberately *not* on the
/// loud ladder, so an idle agent no longer reads as needing the user. Its value
/// is the fixed [`IDLE_VALUE`]; the status watch may replace it with a real loud
/// status (or clear it) once it judges the session genuinely needs a human.
pub const IDLE_KEY: &str = "idle";

/// The fixed value the [`IDLE_KEY`] tag carries. Quiet by design (not on
/// [`ATTENTION_VALUES`]), so it renders soothing rather than loud.
pub const IDLE_VALUE: &str = "idle";

/// A quiet lifecycle mark stamped when an archived session is recovered. The
/// GitHub PR poller uses this to avoid immediately re-archiving a session whose
/// already-merged PR is still visible.
pub const RECOVERED_KEY: &str = "recovered";

/// The fixed value the [`RECOVERED_KEY`] tag carries.
pub const RECOVERED_VALUE: &str = "true";

/// A quiet operator override that keeps automatic retention paths from
/// archiving this branch's live session. Manual Archive remains available.
pub const AUTO_ARCHIVE_KEY: &str = "auto-archive";

/// The fixed opt-out value carried by [`AUTO_ARCHIVE_KEY`].
pub const AUTO_ARCHIVE_DISABLED_VALUE: &str = "disabled";

/// Branch tag wiring a session to a GitHub thread; the value is
/// `owner/name#number` (an issue or a PR — GitHub comments treat them alike).
/// Quiet. While present, loom mirrors every `weaver status` write onto one
/// comment on that thread — the "On it" status card, edited in place. The
/// `@loom` trigger stamps it at launch; `weaver tag set github owner/name#123`
/// wires a session by hand; clearing it stops the mirroring.
pub const GITHUB_KEY: &str = "github";

/// Loom's bookkeeping for the status card: the GitHub comment id it edits,
/// with the wiring it belongs to in the note. Machine-owned — the tag routes
/// refuse to set it by hand (a forged id would aim loom's edits at someone
/// else's comment); clearing it just makes the next status write post afresh.
pub const GITHUB_COMMENT_KEY: &str = "github.status_comment";

/// Loom's bookkeeping that a session's PR back-link comment was already posted,
/// so the poll loop doesn't re-post it. Machine-owned like [`GITHUB_COMMENT_KEY`].
pub const GITHUB_LINKED_KEY: &str = "github.linked";

/// Branch tag wiring a session to a Slack thread; the value is
/// `team_id/channel_id/thread_ts` (team-scoped — channel ids only mean something
/// within a workspace). Quiet. While present, loom mirrors every `weaver status`
/// write onto one message in that thread — the "On it" status card, edited in
/// place. The `/marinbot` (and `@marinbot`) trigger stamps it at launch;
/// clearing it stops the mirroring.
pub const SLACK_KEY: &str = "slack";

/// Loom's bookkeeping for the Slack status card: the message `ts` it edits, with
/// the wiring it belongs to in the note. Machine-owned — the tag routes refuse
/// to set it by hand (a forged `ts` would aim loom's edits at another message);
/// clearing it just makes the next status write post afresh.
pub const SLACK_STATUS_MSG_KEY: &str = "slack.status_message";

/// Keys loom stamps mechanically to track its own integration side-effects. The
/// generic tag routes refuse to *set* them — a forged comment id / message `ts`
/// would aim loom's edits at someone else's content. Clearing stays allowed
/// (harmless: loom re-creates its bookkeeping on the next pass). Centralized here
/// (rather than per-integration) because both generic tag routes gate on it.
pub fn is_reserved_tag(key: &str) -> bool {
    matches!(
        key,
        GITHUB_LINKED_KEY | GITHUB_COMMENT_KEY | SLACK_STATUS_MSG_KEY
    )
}

/// The loud keys: those that raise an attention signal on the dashboard. Any
/// other key is quiet (a deletable pill) — including the soothing [`IDLE_KEY`].
pub const LOUD_KEYS: &[&str] = &[ATTENTION_KEY, TRIAGE_KEY];

/// Whether `key` is a loud (badge-raising) key.
pub fn is_loud(key: &str) -> bool {
    LOUD_KEYS.contains(&key)
}

/// The storable values for the loud keys, ordered calm → urgent. `ok`/empty is
/// never stored — it means "clear the tag" (absence is the calm state):
///
/// * `attention` — wants the user to look: a question, a decision, "ready".
/// * `blocked` — stuck or errored, needs help to proceed.
pub const ATTENTION_VALUES: &[&str] = &["attention", "blocked"];

/// Whether `value` is storable under `key`. A loud key admits only the levels in
/// [`ATTENTION_VALUES`] (the calm `ok` clears rather than stores); any other key
/// accepts any non-empty value.
pub fn is_valid_value(key: &str, value: &str) -> bool {
    if is_loud(key) {
        ATTENTION_VALUES.contains(&value)
    } else {
        !value.is_empty()
    }
}

/// Whether `value` raises a badge — i.e. it sits on the [`ATTENTION_VALUES`]
/// ladder. **Loudness is carried by the value**, so *any* key holding such a
/// value is loud (the agent's own `attention`, a watch's typed `review`/`stuck`,
/// …); the dashboard renders each as a chip labelled by its key. Distinct from
/// [`is_loud`], which gates the well-known *keys* to the ladder in validation.
pub fn is_loud_value(value: &str) -> bool {
    ATTENTION_VALUES.contains(&value)
}

/// The quiet values that **park** a branch *below* the calm default in the
/// dashboard's fleet sort — the opposite end of the ladder from
/// [`ATTENTION_VALUES`]. A parked branch is waiting on an external actor (a human
/// PR reviewer, a CI run) and needs nothing from the user, so a scanning user can
/// skip past it: the dashboard sinks it under the live-but-calm rows it should
/// look at first. The value names *what is awaited* (`review`, …); the key is the
/// axis (e.g. the review watch's `awaiting`). Quiet by design — these never raise
/// a badge — so a parked row renders as a plain pill, never a loud chip.
/// Mirrored by the frontend's `PARKED` map and `weaver_loom.PARKED_VALUES`.
pub const PARKED_VALUES: &[&str] = &["review"];

/// Whether `value` parks a branch — i.e. it sits on the [`PARKED_VALUES`] ladder,
/// sinking the row below the calm default in the fleet sort. Like
/// [`is_loud_value`], the signal is **value-driven**: any key holding such a
/// value parks, so a watch picks its own axis key and the value carries the
/// meaning. A value is never both parked and loud (the two ladders are disjoint).
pub fn is_parked_value(value: &str) -> bool {
    PARKED_VALUES.contains(&value)
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

/// Set (insert or replace) a tag on a branch. Single-valued per `(branch_id,
/// key)`: a second set for the same key overwrites the value, note, and
/// attribution and re-stamps `set_at`. The caller is expected to have validated
/// `value` (see [`is_valid_value`]); clearing is [`clear`], not a set with an
/// empty value.
pub async fn set(
    db: &Db,
    branch_id: &str,
    key: &str,
    value: &str,
    note: &str,
    set_by: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO tags (branch_id, key, value, note, set_by, set_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(branch_id, key) DO UPDATE SET
           value = excluded.value, note = excluded.note,
           set_by = excluded.set_by, set_at = excluded.set_at",
    )
    .bind(branch_id)
    .bind(key)
    .bind(value)
    .bind(note)
    .bind(set_by)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

/// Clear a tag — delete the `(branch_id, key)` row. A no-op when the tag is
/// absent. This is how a loud axis returns to calm (`ok`).
pub async fn clear(db: &Db, branch_id: &str, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM tags WHERE branch_id = ? AND key = ?")
        .bind(branch_id)
        .bind(key)
        .execute(db)
        .await?;
    Ok(())
}

/// One desired tag in an atomic author-scoped replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInput {
    pub key: String,
    pub value: String,
    pub note: String,
}

/// One exact `(key, value)` tag to clear alongside an author-scoped
/// replacement. Exact matching keeps a stale lifecycle clear from deleting a
/// newer, unrelated value that reused the same key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMatch {
    pub key: String,
    pub value: String,
}

/// The before/after state of an atomic tag replacement.
#[derive(Debug)]
pub struct TagReplacement {
    pub before: Vec<Tag>,
    pub after: Vec<Tag>,
}

/// Replace every tag currently authored by `set_by` with `desired`, and apply
/// the additional exact-match clears, in one transaction.
///
/// Watches use this instead of a sequence of per-key upserts and deletes. A
/// stale watch snapshot can therefore no longer clear a key another actor
/// replaced after the snapshot: only rows still attributed to this author (or
/// rows matching an explicit `(key, value)` lifecycle clear) are removed.
pub async fn replace_by(
    db: &Db,
    branch_id: &str,
    set_by: &str,
    desired: &[TagInput],
    clear: &[TagMatch],
) -> Result<TagReplacement> {
    let mut tx = db.begin().await?;
    let before = sqlx::query_as::<_, Tag>(
        "SELECT key, value, note, set_by, set_at FROM tags
         WHERE branch_id = ? ORDER BY key",
    )
    .bind(branch_id)
    .fetch_all(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM tags WHERE branch_id = ? AND set_by = ?")
        .bind(branch_id)
        .bind(set_by)
        .execute(&mut *tx)
        .await?;
    for tag in clear {
        sqlx::query("DELETE FROM tags WHERE branch_id = ? AND key = ? AND value = ?")
            .bind(branch_id)
            .bind(&tag.key)
            .bind(&tag.value)
            .execute(&mut *tx)
            .await?;
    }

    let set_at = now_iso();
    for tag in desired {
        sqlx::query(
            "INSERT INTO tags (branch_id, key, value, note, set_by, set_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(branch_id, key) DO UPDATE SET
               value = excluded.value, note = excluded.note,
               set_by = excluded.set_by, set_at = excluded.set_at",
        )
        .bind(branch_id)
        .bind(&tag.key)
        .bind(&tag.value)
        .bind(&tag.note)
        .bind(set_by)
        .bind(&set_at)
        .execute(&mut *tx)
        .await?;
    }

    let after = sqlx::query_as::<_, Tag>(
        "SELECT key, value, note, set_by, set_at FROM tags
         WHERE branch_id = ? ORDER BY key",
    )
    .bind(branch_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(TagReplacement { before, after })
}

/// Fetch one tag by key, or `None` when the branch has no tag for that key.
pub async fn get(db: &Db, branch_id: &str, key: &str) -> Result<Option<Tag>> {
    let row = sqlx::query_as::<_, Tag>(
        "SELECT key, value, note, set_by, set_at FROM tags
         WHERE branch_id = ? AND key = ?",
    )
    .bind(branch_id)
    .bind(key)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Whether this branch explicitly opts out of automatic archive paths.
pub async fn auto_archive_disabled(db: &Db, branch_id: &str) -> Result<bool> {
    Ok(get(db, branch_id, AUTO_ARCHIVE_KEY)
        .await?
        .is_some_and(|tag| tag.value == AUTO_ARCHIVE_DISABLED_VALUE))
}

/// Every tag on a branch, ordered by key for a stable presentation.
pub async fn list(db: &Db, branch_id: &str) -> Result<Vec<Tag>> {
    let rows = sqlx::query_as::<_, Tag>(
        "SELECT key, value, note, set_by, set_at FROM tags
         WHERE branch_id = ? ORDER BY key",
    )
    .bind(branch_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loud_keys_validate_against_the_attention_ladder() {
        assert!(is_loud(ATTENTION_KEY));
        assert!(is_loud(TRIAGE_KEY));
        assert!(!is_loud("priority"));

        // Loud keys accept only the storable levels; `ok`/empty clears instead.
        assert!(is_valid_value(ATTENTION_KEY, "attention"));
        assert!(is_valid_value(TRIAGE_KEY, "blocked"));
        assert!(!is_valid_value(ATTENTION_KEY, "ok"));
        assert!(!is_valid_value(ATTENTION_KEY, ""));

        // A free-form key accepts any non-empty value.
        assert!(is_valid_value("priority", "high"));
        assert!(!is_valid_value("priority", ""));

        // `idle` is a quiet, soothing key — never on the loud ladder, so an idle
        // agent doesn't read as needing the user. It validates as a free-form
        // key (any non-empty value), and its fixed value is not loud.
        assert!(!is_loud(IDLE_KEY));
        assert!(!is_loud_value(IDLE_VALUE));
        assert!(is_valid_value(IDLE_KEY, IDLE_VALUE));
        assert!(!is_loud(RECOVERED_KEY));
        assert!(!is_loud_value(RECOVERED_VALUE));
        assert!(is_valid_value(RECOVERED_KEY, RECOVERED_VALUE));
        assert!(!is_loud(AUTO_ARCHIVE_KEY));
        assert!(!is_loud_value(AUTO_ARCHIVE_DISABLED_VALUE));
        assert!(is_valid_value(
            AUTO_ARCHIVE_KEY,
            AUTO_ARCHIVE_DISABLED_VALUE
        ));

        // Loudness is value-driven: any key holding a ladder value is loud (a
        // watch's typed `review`/`stuck`), while a quiet value never is.
        assert!(is_loud_value("attention"));
        assert!(is_loud_value("blocked"));
        assert!(!is_loud_value("high"));
        assert!(!is_loud_value("ok"));
        // A free-form key may legitimately carry a loud value (the watch's marks).
        assert!(is_valid_value("review", "attention"));

        // Parking is the value-driven mirror of loudness: a parked value sinks
        // the row below the calm default, and the two ladders are disjoint (a
        // value is never both). Parked values are quiet — never loud.
        assert!(is_parked_value("review"));
        assert!(!is_parked_value("attention"));
        assert!(!is_parked_value("waiting"));
        assert!(!is_loud_value("review"));
        for v in PARKED_VALUES {
            assert!(
                !ATTENTION_VALUES.contains(v),
                "ladders must stay disjoint: {v}"
            );
            assert!(!is_loud_value(v));
        }
        // A parked mark stores fine on a free-form axis key (any non-empty value).
        assert!(is_valid_value("awaiting", "review"));
    }

    #[tokio::test]
    async fn set_get_clear_list_roundtrip() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let b = crate::branch::upsert(&db, "/r", "main", "main")
            .await
            .unwrap();

        assert!(get(&db, &b.id, ATTENTION_KEY).await.unwrap().is_none());
        assert!(list(&db, &b.id).await.unwrap().is_empty());

        set(
            &db,
            &b.id,
            ATTENTION_KEY,
            "blocked",
            "build broken",
            "agent",
        )
        .await
        .unwrap();
        set(&db, &b.id, "priority", "high", "", "manual")
            .await
            .unwrap();

        let t = get(&db, &b.id, ATTENTION_KEY).await.unwrap().unwrap();
        assert_eq!(t.value, "blocked");
        assert_eq!(t.note, "build broken");
        assert_eq!(t.set_by, "agent");
        assert!(!t.set_at.is_empty());

        // list is stable, ordered by key.
        let all = list(&db, &b.id).await.unwrap();
        let keys: Vec<&str> = all.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, vec!["attention", "priority"]);

        clear(&db, &b.id, ATTENTION_KEY).await.unwrap();
        assert!(get(&db, &b.id, ATTENTION_KEY).await.unwrap().is_none());
        // Clearing one key leaves the others.
        assert_eq!(list(&db, &b.id).await.unwrap().len(), 1);
        // Clearing an absent tag is a no-op.
        clear(&db, &b.id, ATTENTION_KEY).await.unwrap();
    }

    #[tokio::test]
    async fn set_upserts_in_place() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let b = crate::branch::upsert(&db, "/r", "main", "main")
            .await
            .unwrap();
        set(&db, &b.id, ATTENTION_KEY, "attention", "first", "agent")
            .await
            .unwrap();
        set(&db, &b.id, ATTENTION_KEY, "blocked", "second", "agent")
            .await
            .unwrap();
        // A single row, overwritten — not a second insert.
        let all = list(&db, &b.id).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].value, "blocked");
        assert_eq!(all[0].note, "second");
    }

    #[tokio::test]
    async fn triage_is_a_separate_axis_from_attention() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let b = crate::branch::upsert(&db, "/r", "main", "main")
            .await
            .unwrap();
        // The agent declares its own attention.
        set(&db, &b.id, ATTENTION_KEY, "blocked", "", "agent")
            .await
            .unwrap();
        // The watch stamps a different opinion via triage — an independent
        // row with its own value and attribution.
        set(
            &db,
            &b.id,
            TRIAGE_KEY,
            "attention",
            "looks stuck on the same test",
            "status-check",
        )
        .await
        .unwrap();

        // Both coexist, each carrying its own value/note/author.
        let attention = get(&db, &b.id, ATTENTION_KEY).await.unwrap().unwrap();
        assert_eq!(attention.value, "blocked");
        assert_eq!(attention.set_by, "agent");

        let triage = get(&db, &b.id, TRIAGE_KEY).await.unwrap().unwrap();
        assert_eq!(triage.value, "attention");
        assert_eq!(triage.note, "looks stuck on the same test");
        assert_eq!(triage.set_by, "status-check");
        assert!(!triage.set_at.is_empty());

        // Clearing one leaves the other untouched.
        clear(&db, &b.id, TRIAGE_KEY).await.unwrap();
        assert_eq!(
            get(&db, &b.id, ATTENTION_KEY).await.unwrap().unwrap().value,
            "blocked"
        );
    }

    #[tokio::test]
    async fn replace_by_is_atomic_and_author_scoped() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let b = crate::branch::upsert(&db, "/r", "main", "main")
            .await
            .unwrap();
        set(&db, &b.id, "stuck", "blocked", "", "watch")
            .await
            .unwrap();
        set(&db, &b.id, "manual", "keep", "", "manual")
            .await
            .unwrap();
        set(&db, &b.id, IDLE_KEY, IDLE_VALUE, "", "agent")
            .await
            .unwrap();

        replace_by(
            &db,
            &b.id,
            "watch",
            &[TagInput {
                key: "review".to_string(),
                value: "attention".to_string(),
                note: "ready".to_string(),
            }],
            &[TagMatch {
                key: IDLE_KEY.to_string(),
                value: IDLE_VALUE.to_string(),
            }],
        )
        .await
        .unwrap();

        let tags = list(&db, &b.id).await.unwrap();
        assert!(tags.iter().all(|tag| tag.key != "stuck"));
        assert!(tags.iter().all(|tag| tag.key != IDLE_KEY));
        assert!(tags
            .iter()
            .any(|tag| tag.key == "manual" && tag.set_by == "manual"));
        assert!(tags
            .iter()
            .any(|tag| tag.key == "review" && tag.set_by == "watch"));
    }

    #[tokio::test]
    async fn replace_by_does_not_clear_a_key_another_author_took_over() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let b = crate::branch::upsert(&db, "/r", "main", "main")
            .await
            .unwrap();
        set(&db, &b.id, "stuck", "blocked", "", "watch")
            .await
            .unwrap();
        // The watch's caller still has the old snapshot, but a person replaced
        // the key before its batch mutation reached the server.
        set(&db, &b.id, "stuck", "keep", "", "manual")
            .await
            .unwrap();

        replace_by(&db, &b.id, "watch", &[], &[]).await.unwrap();

        let tag = get(&db, &b.id, "stuck").await.unwrap().unwrap();
        assert_eq!(tag.value, "keep");
        assert_eq!(tag.set_by, "manual");
    }
}
