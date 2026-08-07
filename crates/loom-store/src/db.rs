//! Loom database setup: opens the shared database through `weaver-core`, then
//! applies loom's independently versioned, loom-owned schema.

use anyhow::{Context, Result};
use std::path::Path;

pub use weaver_core::db::{
    connect_in_memory as core_connect_in_memory, default_db_path, now_iso, run_dir, weaver_home, Db,
};
use weaver_core::migrations::{add_column_if_missing, split_statements, table_columns, Stream};

const LOOM_MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "baseline",
        include_str!("../migrations/0001_baseline.sql"),
    ),
    (
        2,
        "profiles",
        include_str!("../migrations/0002_profiles.sql"),
    ),
    (
        3,
        "grants-runs",
        include_str!("../migrations/0003_grants_runs.sql"),
    ),
    (
        4,
        "workload-federation",
        include_str!("../migrations/0004_workload_federation.sql"),
    ),
    (
        5,
        "restricted-profiles",
        include_str!("../migrations/0005_restricted_profiles.sql"),
    ),
    (
        6,
        "mcp-access",
        include_str!("../migrations/0006_mcp_access.sql"),
    ),
    (
        7,
        "automation-channels",
        include_str!("../migrations/0007_automation_channels.sql"),
    ),
    (
        8,
        "acp-metadata",
        include_str!("../migrations/0008_acp_metadata.sql"),
    ),
    (
        9,
        "retired-profiles",
        include_str!("../migrations/0009_retired_profiles.sql"),
    ),
    (
        10,
        "launch-snapshot",
        include_str!("../migrations/0010_launch_snapshot.sql"),
    ),
    (
        11,
        "profile-lifetimes",
        include_str!("../migrations/0011_profile_lifetimes.sql"),
    ),
    (
        12,
        "session-layout",
        include_str!("../migrations/0012_session_layout.sql"),
    ),
    (
        13,
        "review-inbox",
        include_str!("../migrations/0013_review_inbox.sql"),
    ),
    (
        14,
        "metadata-assistance",
        include_str!("../migrations/0014_metadata_assistance.sql"),
    ),
    (
        15,
        "chat-block-ref-id",
        include_str!("../migrations/0015_chat_block_ref_id.sql"),
    ),
    (
        16,
        "channels",
        include_str!("../migrations/0016_channels.sql"),
    ),
    (
        17,
        "session-transitions",
        include_str!("../migrations/0017_session_transitions.sql"),
    ),
    (
        18,
        "github-head-freshness",
        include_str!("../migrations/0018_github_head_freshness.sql"),
    ),
    (
        19,
        "slack-space",
        include_str!("../migrations/0019_slack_space.sql"),
    ),
    (
        20,
        "slack-alert-routes",
        include_str!("../migrations/0020_slack_alert_routes.sql"),
    ),
];

const LOOM_STREAM: Stream = Stream::new("loom_schema_migrations", LOOM_MIGRATIONS);

/// Latest loom-owned schema version compiled into this binary.
pub fn latest_migration_version() -> i64 {
    LOOM_MIGRATIONS.last().map_or(0, |(version, _, _)| *version)
}

/// Open the shared database and apply loom's schema after the core schema.
pub async fn connect(path: &Path) -> Result<Db> {
    let bootstrap = weaver_core::db::begin_bootstrap(path).await?;
    migrate_loom(bootstrap.migration_db()).await?;
    bootstrap.finish().await
}

/// In-memory variant for tests.
pub async fn connect_in_memory() -> Result<Db> {
    let pool = weaver_core::db::connect_in_memory().await?;
    migrate_loom(&pool).await?;
    Ok(pool)
}

async fn migrate_loom(pool: &Db) -> Result<()> {
    let indicator_exists = LOOM_STREAM.indicator_exists(pool).await?;
    let baseline_recorded = if indicator_exists {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM loom_schema_migrations WHERE version = 1)",
        )
        .fetch_one(pool)
        .await
        .context("checking loom baseline version")?
    } else {
        false
    };

    // Loom tables predate their migration stream. If an unversioned sessions
    // table is present, bring every supported historical shape to the baseline
    // end state and stamp it rather than replaying baseline DDL against it. Test
    // the version, not just the indicator table: a crash after creating the
    // indicator but before stamping must resume adoption on the next start.
    if !baseline_recorded && !table_columns(pool, "sessions").await?.is_empty() {
        adopt_unversioned_schema(pool).await?;
        LOOM_STREAM.ensure_indicator(pool).await?;
        LOOM_STREAM.stamp(pool, 1, "baseline").await?;
    }

    LOOM_STREAM.ensure_indicator(pool).await?;
    LOOM_STREAM.apply_pending(pool).await?;

    // Configuration-dependent seeding is intentionally not a migration. If the
    // first start has no owner configured, a later restart must retry it.
    seed_owner(pool).await?;
    Ok(())
}

/// Adopt a database built by the pre-stream `LOOM_SCHEMA` runner.
///
/// Every operation is presence-gated or idempotent. The function can therefore
/// resume after any interrupted statement, and it never compares an existing
/// column's exact declaration: deployed databases legitimately carry stricter
/// declarations than some historical source revisions.
async fn adopt_unversioned_schema(pool: &Db) -> Result<()> {
    tracing::info!("adopting unversioned loom schema");

    // Create any loom tables that were introduced after this database was
    // first deployed. Existing tables are left untouched and reconciled below.
    for stmt in split_statements(LOOM_MIGRATIONS[0].2) {
        sqlx::query(&stmt)
            .execute(pool)
            .await
            .with_context(|| format!("adopting loom baseline: {stmt}"))?;
    }

    rename_session_terminal_column(pool).await?;
    for (column, declaration) in [
        ("model", "TEXT NOT NULL DEFAULT ''"),
        ("effort", "TEXT NOT NULL DEFAULT ''"),
        ("last_activity_at", "TEXT"),
        ("parent_branch_id", "TEXT"),
        ("managed_by", "TEXT"),
        ("created_by", "TEXT"),
        ("park", "TEXT"),
        ("sort_order", "REAL"),
        ("protocol", "TEXT NOT NULL DEFAULT 'terminal'"),
        ("acp_session_id", "TEXT"),
        ("acp_ack_seq", "INTEGER NOT NULL DEFAULT 0"),
        ("acp_inflight", "TEXT"),
        ("current_mode", "TEXT"),
        ("pending_prompt", "TEXT NOT NULL DEFAULT ''"),
        ("origin", "TEXT NOT NULL DEFAULT 'user'"),
        ("class", "TEXT NOT NULL DEFAULT 'interactive'"),
        ("turn_count", "INTEGER NOT NULL DEFAULT 0"),
        ("tracking_issue_id", "INTEGER"),
    ] {
        add_column_if_missing(pool, "sessions", column, declaration).await?;
    }

    add_column_if_missing(
        pool,
        "custom_agents",
        "protocol",
        "TEXT NOT NULL DEFAULT 'terminal'",
    )
    .await?;
    add_column_if_missing(pool, "users", "github_user_id", "INTEGER").await?;
    add_column_if_missing(pool, "users", "display_name", "TEXT").await?;

    // These are safe to repeat. In particular, repeating them repairs a crash
    // after `origin` or `class` was added but before the old one-shot backfill.
    sqlx::query(
        "UPDATE sessions SET origin = 'watch', class = 'automation'
         WHERE managed_by IS NOT NULL",
    )
    .execute(pool)
    .await
    .context("backfilling watch session provenance")?;
    sqlx::query(
        "UPDATE sessions SET origin = 'agent'
         WHERE origin = 'user' AND parent_branch_id IS NOT NULL",
    )
    .execute(pool)
    .await
    .context("backfilling delegated session provenance")?;

    // `CREATE INDEX IF NOT EXISTS` retains an older predicate. Rebuild it after
    // session adoption so archived history no longer occupies the branch slot.
    sqlx::query("DROP INDEX IF EXISTS idx_sessions_active_branch")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX idx_sessions_active_branch
         ON sessions(branch_id) WHERE status NOT IN ('done', 'error', 'archived')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Reconcile the one historical rename without inspecting error strings.
async fn rename_session_terminal_column(pool: &Db) -> Result<()> {
    let columns = table_columns(pool, "sessions").await?;
    let has_old = columns.iter().any(|column| column == "tmux_session");
    let has_new = columns.iter().any(|column| column == "term_session");
    match (has_old, has_new) {
        (true, false) => {
            sqlx::query("ALTER TABLE sessions RENAME COLUMN tmux_session TO term_session")
                .execute(pool)
                .await
                .context("renaming sessions.tmux_session to term_session")?;
        }
        // A partially/manual-upgraded database can contain both. Preserve the
        // new column as authoritative, fill only empty values from the old,
        // then remove the legacy NOT NULL column: leaving it behind would make
        // every later insert that supplies only `term_session` fail.
        (true, true) => {
            sqlx::query(
                "UPDATE sessions SET term_session = tmux_session
                 WHERE term_session IS NULL OR term_session = ''",
            )
            .execute(pool)
            .await
            .context("reconciling sessions terminal columns")?;
            sqlx::query("ALTER TABLE sessions DROP COLUMN tmux_session")
                .execute(pool)
                .await
                .context("dropping legacy sessions.tmux_session column")?;
        }
        (false, true) => {}
        (false, false) => {
            anyhow::bail!("unversioned sessions table has neither term_session nor tmux_session")
        }
    }
    Ok(())
}

/// Seed the approved-user allowlist with the configured deploy owner.
async fn seed_owner(pool: &Db) -> Result<()> {
    let owner = std::env::var("LOOM_OWNER_GITHUB")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(owner) = owner else {
        tracing::warn!(
            "LOOM_OWNER_GITHUB is not set — no owner user was seeded. No one can log in until \
             you set LOOM_OWNER_GITHUB and restart the daemon (seeding re-runs on every start, \
             so no fresh migration is needed)."
        );
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO users (username, github_login) VALUES (?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(&owner)
    .bind(&owner)
    .execute(pool)
    .await
    .with_context(|| format!("seeding owner user '{owner}'"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    async fn insert_branch(db: &Db, id: &str) {
        sqlx::query(
            "INSERT INTO branches (id, repo_root, branch, created_at, updated_at)
             VALUES (?, '/r', ?, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(id)
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    }

    async fn migrate_through(db: &Db, count: usize) {
        LOOM_STREAM.ensure_indicator(db).await.unwrap();
        for (version, name, migration) in LOOM_MIGRATIONS.iter().take(count) {
            for statement in split_statements(migration) {
                sqlx::query(&statement).execute(db).await.unwrap();
            }
            LOOM_STREAM.stamp(db, *version, name).await.unwrap();
        }
    }

    #[tokio::test]
    async fn fresh_schema_records_baseline_and_has_final_shape() {
        let db = connect_in_memory().await.unwrap();
        migrate_loom(&db).await.unwrap();
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM loom_schema_migrations ORDER BY version")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            versions,
            (1..=latest_migration_version()).collect::<Vec<_>>()
        );

        let profile_columns = table_columns(&db, "profiles").await.unwrap();
        assert!(profile_columns.iter().any(|column| column == "retired"));
        assert!(profile_columns.iter().any(|column| column == "lifetime"));
        assert!(!table_columns(&db, "session_spaces")
            .await
            .unwrap()
            .is_empty());
        assert!(!table_columns(&db, "session_groups")
            .await
            .unwrap()
            .is_empty());
        assert!(!table_columns(&db, "session_placements")
            .await
            .unwrap()
            .is_empty());
        assert!(!table_columns(&db, "session_metadata_assistance")
            .await
            .unwrap()
            .is_empty());
        let spaces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_spaces")
            .fetch_one(&db)
            .await
            .unwrap();
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(spaces, 4);
        assert_eq!(revision, 1);

        let columns = table_columns(&db, "sessions").await.unwrap();
        for expected in [
            "term_session",
            "parent_branch_id",
            "protocol",
            "pending_prompt",
            "origin",
            "class",
            "turn_count",
            "tracking_issue_id",
            "profile",
            "creator_subject",
            "parent_session_id",
            "policy_prelude",
            "policy_restricted",
            "policy_allowed_tools",
            "policy_mcp_access",
            "launch_snapshot",
            "profile_lifetime",
            "policy_strict",
            "mutation_revision",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }
        let inbox_columns = table_columns(&db, "review_conversation_inbox")
            .await
            .unwrap();
        for expected in [
            "delivery_key",
            "branch_id",
            "preferred_session_id",
            "payload",
            "claim_token",
            "claim_owner",
        ] {
            assert!(
                inbox_columns.iter().any(|column| column == expected),
                "missing review inbox column {expected}"
            );
        }
        let github_columns = table_columns(&db, "branch_github").await.unwrap();
        for expected in ["head_sha", "head_updated_at"] {
            assert!(
                github_columns.iter().any(|column| column == expected),
                "missing GitHub freshness column {expected}"
            );
        }

        insert_branch(&db, "t1").await;
        sqlx::query(
            "INSERT INTO sessions (id, branch_id, work_dir, term_session, status)
             VALUES ('archived', 't1', '/w1', 'term-1', 'archived'),
                    ('active', 't1', '/w2', 'term-2', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn v12_database_adds_the_review_inbox_while_upgrading_to_latest() {
        let db = core_connect_in_memory().await.unwrap();
        migrate_through(&db, 12).await;
        assert!(table_columns(&db, "review_conversation_inbox")
            .await
            .unwrap()
            .is_empty());

        LOOM_STREAM.apply_pending(&db).await.unwrap();
        LOOM_STREAM.apply_pending(&db).await.unwrap();

        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM loom_schema_migrations ORDER BY version")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            versions,
            (1..=latest_migration_version()).collect::<Vec<_>>()
        );
        assert!(!table_columns(&db, "review_conversation_inbox")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn v14_database_backfills_same_id_session_channels() {
        let db = core_connect_in_memory().await.unwrap();
        migrate_through(&db, 14).await;
        insert_branch(&db, "channel-branch").await;
        sqlx::query(
            "UPDATE branches
             SET title = 'Backfilled channel', goal = 'Keep the launch charter'
             WHERE id = 'channel-branch'",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, created_by,
              creator_kind, creator_subject, created_at)
             VALUES ('legacy-session', 'channel-branch', '/w', 'term', 'running',
                     'alice', 'user', 'alice', '2026-01-01T00:00:00.000Z')",
        )
        .execute(&db)
        .await
        .unwrap();

        LOOM_STREAM.apply_pending(&db).await.unwrap();

        let channel: (String, String, String) = sqlx::query_as(
            "SELECT id, name, topic FROM channels WHERE session_id = 'legacy-session'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            channel,
            (
                "legacy-session".to_string(),
                "Backfilled channel".to_string(),
                "Keep the launch charter".to_string(),
            )
        );
        let opening: (String, String) = sqlx::query_as(
            "SELECT kind, body FROM channel_messages
             WHERE channel_id = 'legacy-session' AND seq = 1",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            opening,
            ("goal".to_string(), "Keep the launch charter".to_string())
        );
        let owner: (String, i64) = sqlx::query_as(
            "SELECT mode, read_seq FROM channel_subscriptions
             WHERE channel_id = 'legacy-session'
               AND subject_kind = 'session'
               AND subject_id = 'legacy-session'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(owner, ("deliver".to_string(), 1));
    }

    #[tokio::test]
    async fn v17_database_adopts_manual_slack_space_without_overwriting_placements() {
        let db = core_connect_in_memory().await.unwrap();
        migrate_through(&db, 17).await;

        sqlx::query(
            "INSERT INTO session_spaces
             (id, name, rank, system_key, created_at, updated_at)
             VALUES ('custom-slack', 'Slack', 3, NULL, '', '')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_groups
             (id, space_id, name, rank, system_key, created_at, updated_at)
             VALUES ('custom-slack-inbox', 'custom-slack', 'Inbox', 0, NULL, '', '')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_placement_defaults
             (selector_kind, selector_value, group_id)
             VALUES ('origin', 'slack', 'custom-slack-inbox')",
        )
        .execute(&db)
        .await
        .unwrap();

        for id in ["fallback-slack", "manually-placed-slack"] {
            insert_branch(&db, id).await;
        }
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, origin, class, created_at)
             VALUES
             ('fallback-slack', 'fallback-slack', '/fallback', 't-fallback',
              'running', 'slack', 'interactive', ''),
             ('manually-placed-slack', 'manually-placed-slack', '/manual', 't-manual',
              'running', 'slack', 'interactive', '')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_placements (session_id, group_id, rank, updated_at)
             VALUES
             ('fallback-slack', 'group-user-inbox', 0, ''),
             ('manually-placed-slack', 'group-github-inbox', 0, '')",
        )
        .execute(&db)
        .await
        .unwrap();

        LOOM_STREAM.apply_pending(&db).await.unwrap();
        LOOM_STREAM.apply_pending(&db).await.unwrap();

        let adopted_space: (String, String) =
            sqlx::query_as("SELECT id, system_key FROM session_spaces WHERE name = 'Slack'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            adopted_space,
            ("custom-slack".to_string(), "slack".to_string())
        );
        let adopted_group: (String, String) = sqlx::query_as(
            "SELECT id, system_key FROM session_groups
             WHERE space_id = 'custom-slack' AND name = 'Inbox'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            adopted_group,
            ("custom-slack-inbox".to_string(), "inbox".to_string())
        );
        let placement_default: String = sqlx::query_scalar(
            "SELECT group_id FROM session_placement_defaults
             WHERE selector_kind = 'origin' AND selector_value = 'slack'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(placement_default, "custom-slack-inbox");
        let placements: Vec<(String, String)> = sqlx::query_as(
            "SELECT session_id, group_id FROM session_placements
             WHERE session_id IN ('fallback-slack', 'manually-placed-slack')
             ORDER BY session_id",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(
            placements,
            [
                (
                    "fallback-slack".to_string(),
                    "custom-slack-inbox".to_string(),
                ),
                (
                    "manually-placed-slack".to_string(),
                    "group-github-inbox".to_string(),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn v11_database_upgrades_layout_deterministically_and_idempotently() {
        let db = core_connect_in_memory().await.unwrap();
        migrate_through(&db, 11).await;
        let recorded: Vec<(i64, String)> =
            sqlx::query_as("SELECT version, name FROM loom_schema_migrations ORDER BY version")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            &recorded[9..],
            &[
                (10, "launch-snapshot".to_string()),
                (11, "profile-lifetimes".to_string()),
            ]
        );
        for id in [
            "user",
            "github",
            "ops",
            "slack",
            "parent",
            "child",
            "parked-child",
            "warm",
            "newest",
            "manual",
            "oldest",
            "parked-recent",
        ] {
            insert_branch(&db, id).await;
        }
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, origin, class, park,
              parent_session_id, created_at)
             VALUES
             ('user', 'user', '/user', 't-user', 'running', 'user', 'interactive',
              'parked', NULL, '2026-01-01T00:00:00.000Z'),
             ('github', 'github', '/github', 't-github', 'running', 'github', 'interactive',
              'parked', NULL, '2026-01-01T00:00:01.000Z'),
             ('ops', 'ops', '/ops', 't-ops', 'running', 'actions', 'automation',
              'parked', NULL, '2026-01-01T00:00:02.000Z'),
             ('slack', 'slack', '/slack', 't-slack', 'running', 'slack', 'interactive',
              NULL, NULL, '2026-01-01T00:00:02.500Z'),
             ('parent', 'parent', '/parent', 't-parent', 'running', 'github', 'interactive',
              NULL, NULL, '2026-01-01T00:00:03.000Z'),
             ('child', 'child', '/child', 't-child', 'running', 'agent', 'interactive',
              NULL, 'parent', '2026-01-01T00:00:04.000Z'),
             ('parked-child', 'parked-child', '/parked-child', 't-parked-child',
              'running', 'agent', 'interactive', 'parked', 'child',
              '2026-01-01T00:00:05.000Z'),
             ('warm', 'warm', '/warm', 't-warm', 'running', 'watch', 'automation',
              'parked', NULL, '2026-01-01T00:00:06.000Z')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("UPDATE sessions SET managed_by = 'watch-1' WHERE id = 'warm'")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, origin, class, park,
              last_activity_at, sort_order, created_at)
             VALUES
             ('newest', 'newest', '/newest', 't-newest', 'running', 'user',
              'interactive', NULL, '2026-01-01T00:00:10.000Z', NULL,
              '2026-01-01T00:00:10.000Z'),
             ('manual', 'manual', '/manual', 't-manual', 'running', 'user',
              'interactive', NULL, '2026-01-01T00:00:05.000Z',
              (
                -CAST((julianday('2026-01-01T00:00:10.000Z') - 2440587.5)
                  * 86400000 AS INTEGER)
                + -CAST((julianday('2026-01-01T00:00:00.000Z') - 2440587.5)
                  * 86400000 AS INTEGER)
              ) / 2.0,
              '2026-01-01T00:00:05.000Z'),
             ('oldest', 'oldest', '/oldest', 't-oldest', 'running', 'user',
              'interactive', NULL, '2026-01-01T00:00:00.000Z', NULL,
              '2026-01-01T00:00:00.000Z'),
             ('parked-recent', 'parked-recent', '/parked-recent',
              't-parked-recent', 'running', 'user', 'interactive', 'parked',
              '2026-01-02T00:00:00.000Z', NULL, '2025-12-01T00:00:00.000Z')",
        )
        .execute(&db)
        .await
        .unwrap();

        LOOM_STREAM.apply_pending(&db).await.unwrap();
        LOOM_STREAM.apply_pending(&db).await.unwrap();

        for (session, expected) in [
            ("user", "group-user-later"),
            ("github", "group-github-later"),
            ("ops", "group-ops-later"),
            ("slack", "group-slack-inbox"),
            ("parent", "group-github-inbox"),
            ("child", "group-github-inbox"),
            ("parked-child", "group-github-later"),
        ] {
            let group: String =
                sqlx::query_scalar("SELECT group_id FROM session_placements WHERE session_id = ?")
                    .bind(session)
                    .fetch_one(&db)
                    .await
                    .unwrap();
            assert_eq!(group, expected, "{session}");
        }
        let placed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_placements")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(placed, 11);
        let inbox_order: Vec<String> = sqlx::query_scalar(
            "SELECT session_id FROM session_placements
             WHERE group_id = 'group-user-inbox' ORDER BY rank",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(inbox_order, ["newest", "manual", "oldest"]);
        let later_order: Vec<String> = sqlx::query_scalar(
            "SELECT session_id FROM session_placements
             WHERE group_id = 'group-user-later' ORDER BY rank",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(later_order, ["parked-recent", "user"]);
        let defaults: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_placement_defaults")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(defaults, 9);
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM session_layout_state WHERE id = 1")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(revision, 1);
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM loom_schema_migrations ORDER BY version")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            versions,
            (1..=latest_migration_version()).collect::<Vec<_>>()
        );
        assert!(
            crate::session_layout::placement(&db, "warm")
                .await
                .unwrap()
                .is_none(),
            "layout migration excludes hidden warm infrastructure"
        );
    }

    #[tokio::test]
    async fn mcp_migration_preserves_legacy_profile_intent() {
        let db = core_connect_in_memory().await.unwrap();
        for (_, _, migration) in LOOM_MIGRATIONS.iter().take(5) {
            for statement in split_statements(migration) {
                sqlx::query(&statement).execute(&db).await.unwrap();
            }
        }
        sqlx::query(
            "UPDATE profiles
             SET allowed_tools = '[\"Read(./**)\",\"mcp/github/comment@v1\"]'
             WHERE name = 'default'",
        )
        .execute(&db)
        .await
        .unwrap();

        for statement in split_statements(LOOM_MIGRATIONS[5].2) {
            sqlx::query(&statement).execute(&db).await.unwrap();
        }

        let row =
            sqlx::query("SELECT allowed_tools, mcp_access FROM profiles WHERE name = 'default'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(row.get::<String, _>("allowed_tools"), r#"["Read(./**)"]"#);
        assert_eq!(
            row.get::<String, _>("mcp_access"),
            r#"{"mode":"groups","groups":["github"]}"#
        );
    }

    #[tokio::test]
    async fn lifetime_migration_preserves_strict_precedence_when_provable() {
        let db = core_connect_in_memory().await.unwrap();
        for (_, _, migration) in LOOM_MIGRATIONS.iter().take(10) {
            for statement in split_statements(migration) {
                sqlx::query(&statement).execute(&db).await.unwrap();
            }
        }
        insert_branch(&db, "strict-branch").await;
        sqlx::query("UPDATE profiles SET strict = 1, revision = 7 WHERE name = 'default'")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, profile,
              profile_revision, launch_snapshot)
             VALUES
             ('provable', 'strict-branch', '/w1', 'term-1', 'running',
              'default', 7, ''),
             ('unknown', 'strict-branch', '/w2', 'term-2', 'archived',
              'default', 3, ''),
             ('snapshot', 'strict-branch', '/w3', 'term-3', 'done',
              'default', 7, '{\"policy\":{\"strict\":true}}')",
        )
        .execute(&db)
        .await
        .unwrap();

        for statement in split_statements(LOOM_MIGRATIONS[10].2) {
            sqlx::query(&statement).execute(&db).await.unwrap();
        }

        let provable = sqlx::query(
            "SELECT profile_lifetime, policy_strict, mutation_revision
             FROM sessions WHERE id = 'provable'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(provable.get::<i64, _>("profile_lifetime"), 1);
        assert!(provable.get::<bool, _>("policy_strict"));
        assert_eq!(provable.get::<i64, _>("mutation_revision"), 1);
        let unknown: i64 =
            sqlx::query_scalar("SELECT profile_lifetime FROM sessions WHERE id = 'unknown'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            unknown, 0,
            "an ambiguous upgraded lifetime must fail closed"
        );
        let snapshot_lifetime: i64 = sqlx::query_scalar(
            "SELECT json_extract(launch_snapshot, '$.profile_lifetime')
             FROM sessions WHERE id = 'snapshot'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(snapshot_lifetime, 1);
    }

    /// Regression for the on-disk upgrade path: loom once opened the shared
    /// five-connection pool after core migrations, then changed the sessions
    /// schema through that pool. Connections cached different table widths and
    /// index definitions, producing `Row` out-of-bounds panics and an
    /// `idx_sessions_active_branch already exists` startup failure. The
    /// single-connection in-memory tests cannot exercise that boundary.
    #[tokio::test]
    async fn on_disk_adoption_finishes_before_the_shared_pool_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weaver.db");
        let core = weaver_core::db::connect(&path).await.unwrap();
        insert_branch(&core, "legacy-branch").await;
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                branch_id TEXT NOT NULL,
                work_dir TEXT NOT NULL,
                term_session TEXT NOT NULL,
                agent_kind TEXT NOT NULL DEFAULT 'claude',
                status TEXT NOT NULL,
                github_repo TEXT,
                last_activity_at TEXT,
                created_at TEXT NOT NULL DEFAULT ''
             )",
        )
        .execute(&core)
        .await
        .unwrap();
        sqlx::query(
            "CREATE UNIQUE INDEX idx_sessions_active_branch
             ON sessions(branch_id) WHERE status NOT IN ('done', 'error')",
        )
        .execute(&core)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, created_at)
             VALUES ('legacy', 'legacy-branch', '/w', 'term', 'running', '2026-01-01')",
        )
        .execute(&core)
        .await
        .unwrap();
        core.close().await;

        let db = connect(&path).await.unwrap();
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM loom_schema_migrations ORDER BY version")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            versions,
            (1..=latest_migration_version()).collect::<Vec<_>>()
        );
        let index_sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_sessions_active_branch'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert!(index_sql.contains("'archived'"));

        // Hold every connection at once so each member of the finished pool
        // decodes the widened row instead of repeatedly borrowing one member.
        let mut connections = Vec::new();
        for _ in 0..db.options().get_max_connections() {
            let mut connection = db.acquire().await.unwrap();
            let session = sqlx::query_as::<_, crate::session::Session>(
                "SELECT * FROM sessions WHERE id = 'legacy'",
            )
            .fetch_one(&mut *connection)
            .await
            .unwrap();
            assert_eq!(session.class, "interactive");
            assert_eq!(session.turn_count, 0);
            connections.push(connection);
        }
    }

    #[tokio::test]
    async fn adopts_old_terminal_name_and_preserves_rows() {
        let db = core_connect_in_memory().await.unwrap();
        insert_branch(&db, "b1").await;
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                branch_id TEXT NOT NULL,
                work_dir TEXT NOT NULL,
                tmux_session TEXT NOT NULL,
                agent_kind TEXT NOT NULL DEFAULT 'claude',
                status TEXT NOT NULL,
                github_repo TEXT,
                last_activity_at TEXT,
                created_at TEXT NOT NULL DEFAULT ''
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, tmux_session, status, created_at)
             VALUES ('s1', 'b1', '/w', 'legacy-term', 'running', '2026-01-01')",
        )
        .execute(&db)
        .await
        .unwrap();

        migrate_loom(&db).await.unwrap();
        migrate_loom(&db).await.unwrap();

        let columns = table_columns(&db, "sessions").await.unwrap();
        assert!(columns.iter().any(|column| column == "term_session"));
        assert!(!columns.iter().any(|column| column == "tmux_session"));
        let row = sqlx::query("SELECT term_session, protocol, origin, class FROM sessions")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("term_session"), "legacy-term");
        assert_eq!(row.get::<String, _>("protocol"), "terminal");
        assert_eq!(row.get::<String, _>("origin"), "user");
        assert_eq!(row.get::<String, _>("class"), "interactive");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM loom_schema_migrations")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, latest_migration_version());

        // Adoption replaced the historical index predicate: archived history
        // no longer prevents a new active session from claiming the branch.
        sqlx::query("UPDATE sessions SET status = 'archived' WHERE id = 's1'")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, branch_id, work_dir, term_session, status)
             VALUES ('s2', 'b1', '/w2', 'new-term', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn partial_provenance_upgrade_is_repaired_idempotently() {
        let db = core_connect_in_memory().await.unwrap();
        insert_branch(&db, "watch-branch").await;
        insert_branch(&db, "agent-branch").await;
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                branch_id TEXT NOT NULL,
                work_dir TEXT NOT NULL,
                term_session TEXT NOT NULL,
                agent_kind TEXT NOT NULL DEFAULT 'claude',
                status TEXT NOT NULL,
                github_repo TEXT,
                last_activity_at TEXT,
                parent_branch_id TEXT,
                managed_by TEXT,
                origin TEXT NOT NULL DEFAULT 'user',
                created_at TEXT NOT NULL DEFAULT ''
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, managed_by, created_at)
             VALUES ('watch', 'watch-branch', '/w', 't1', 'running', 'watch-1', '2026-01-01')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, term_session, status, parent_branch_id, created_at)
             VALUES ('agent', 'agent-branch', '/a', 't2', 'running', 'parent', '2026-01-01')",
        )
        .execute(&db)
        .await
        .unwrap();

        // Model a crash after the indicator table was created but before the
        // legacy baseline was stamped.
        LOOM_STREAM.ensure_indicator(&db).await.unwrap();
        migrate_loom(&db).await.unwrap();
        migrate_loom(&db).await.unwrap();

        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT id, origin, class FROM sessions ORDER BY id")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![
                ("agent".into(), "agent".into(), "interactive".into()),
                ("watch".into(), "watch".into(), "automation".into()),
            ]
        );
    }

    #[tokio::test]
    async fn adoption_reconciles_both_terminal_columns() {
        let db = core_connect_in_memory().await.unwrap();
        insert_branch(&db, "b1").await;
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, branch_id TEXT NOT NULL, work_dir TEXT NOT NULL,
                tmux_session TEXT NOT NULL, term_session TEXT,
                agent_kind TEXT NOT NULL DEFAULT 'claude', status TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT ''
             )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions
             (id, branch_id, work_dir, tmux_session, term_session, status)
             VALUES ('s1', 'b1', '/w', 'old-value', '', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();

        migrate_loom(&db).await.unwrap();
        let value: String = sqlx::query_scalar("SELECT term_session FROM sessions WHERE id = 's1'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(value, "old-value");

        let columns = table_columns(&db, "sessions").await.unwrap();
        assert!(!columns.iter().any(|column| column == "tmux_session"));
        insert_branch(&db, "b2").await;
        sqlx::query(
            "INSERT INTO sessions (id, branch_id, work_dir, term_session, status)
             VALUES ('s2', 'b2', '/w2', 'new-value', 'running')",
        )
        .execute(&db)
        .await
        .unwrap();
    }
}
