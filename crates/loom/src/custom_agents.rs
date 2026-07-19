//! Operator-defined **custom agents**, stored in the `custom_agents` table.
//!
//! A custom agent lets the user wire up a coding agent loom doesn't ship — by
//! naming the shell commands loom runs at each launch stage — so it appears in
//! the agent list beside the builtin `claude`/`codex` without a code change. The
//! builtin agents keep their bespoke launch logic in [`crate::agent`]; a custom
//! agent is pure data: a name, a label, and a command per stage.
//!
//! Stages (each a shell fragment, exported with loom's launch env):
//!
//! * `setup`  — run in the worktree before the agent starts (e.g. installing the
//!   status hooks that let weaver see the agent's working/idle state);
//! * `launch` — the fresh-session command; the goal file is appended as a
//!   positional `"$(cat …)"` argument, mirroring the builtin runtimes;
//! * `resume` — the adopt/resume command (no goal). Blank falls back to `launch`.
//!
//! `reports_status` records whether the agent fires weaver's lifecycle hooks —
//! the working / idle / attention signals — surfaced to the UI as the runtime's
//! `supports_hooks` capability.

use anyhow::Result;
use serde::Serialize;
use sqlx::Row;

use crate::db::{now_iso, Db};

/// The builtin agent names a custom agent may not shadow. The retired
/// `concierge` name stays reserved because legacy session rows use it. `shell` is
/// deliberately *not* reserved — it is no longer a builtin, so a user may define
/// it themselves.
pub const RESERVED_NAMES: &[&str] = &["claude", "codex", "concierge"];

/// The execution backends a custom agent may declare (the `protocol` column).
/// `terminal` runs its `launch` command in a PTY; `acp` runs its `launch`
/// command as an [ACP](crate::acp) adapter spoken to over stdio.
pub const PROTOCOLS: &[&str] = &["terminal", "acp"];

/// One custom agent definition — a row of the `custom_agents` table and the shape
/// the API returns for the editor. Every field is operator-supplied except the
/// timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomAgent {
    /// The id referenced by the agent list and a session's `agent_kind`.
    pub name: String,
    /// The display name shown in the agent picker.
    pub label: String,
    /// Shell run in the worktree before launch — the "installing hooks" stage.
    pub setup: String,
    /// The fresh-session launch command; the goal is appended as an argument.
    pub launch: String,
    /// The adopt/resume command (no goal). Blank reuses [`Self::launch`].
    pub resume: String,
    /// Whether the agent fires weaver's lifecycle hooks — the working / idle /
    /// attention signals, surfaced as the runtime's `supports_hooks` capability.
    pub reports_status: bool,
    /// Execution backend: `"terminal"` (the `launch` command runs in a PTY) or
    /// `"acp"` (the `launch` command is an [ACP](crate::acp) adapter loom drives
    /// over stdio). Empty reads as `"terminal"`.
    pub protocol: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Validate a custom agent's name. It is an id — referenced by the agent list and
/// stored as a session's `agent_kind` — so keep it to a clean, URL- and
/// log-friendly slug: a leading letter, then letters, digits, hyphens, or
/// underscores. The builtin names in [`RESERVED_NAMES`] are rejected so a custom
/// agent can't shadow (or masquerade as) a real runtime. The error is a key-free
/// reason so callers can prefix it with their own context.
pub fn validate_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("name must not be empty".to_string());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() {
        return Err(format!("name must start with a letter, got '{name}'"));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "name may contain only letters, digits, hyphens, and underscores, got '{name}'"
        ));
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(format!(
            "name '{name}' is reserved for a builtin agent — pick another name"
        ));
    }
    Ok(())
}

/// Validate the operator-supplied fields of a custom agent (everything but the
/// name, which has its own [`validate_name`]). Returns a key-free reason on the
/// first problem.
pub fn validate_fields(agent: &CustomAgent) -> std::result::Result<(), String> {
    if agent.label.trim().is_empty() {
        return Err("label must not be empty".to_string());
    }
    // `protocol` is a closed set; empty is normalized to `terminal` by the caller.
    if !agent.protocol.is_empty() && !PROTOCOLS.contains(&agent.protocol.as_str()) {
        return Err(format!(
            "protocol '{}' is not one of {PROTOCOLS:?}",
            agent.protocol
        ));
    }
    // An acp agent's `launch` command *is* the adapter, so it can't be blank.
    if agent.protocol == "acp" && agent.launch.trim().is_empty() {
        return Err("an acp agent needs a `launch` command (its ACP adapter)".to_string());
    }
    // Every stage command is optional: an agent with no `launch` execs a bare
    // login shell, which is a legitimate manual-terminal agent (the role the old
    // builtin "shell" filled).
    Ok(())
}

fn row_to_agent(r: &sqlx::sqlite::SqliteRow) -> CustomAgent {
    CustomAgent {
        name: r.get::<String, _>("name"),
        label: r.get::<String, _>("label"),
        setup: r.get::<String, _>("setup"),
        launch: r.get::<String, _>("launch"),
        resume: r.get::<String, _>("resume"),
        reports_status: r.get::<i64, _>("reports_status") != 0,
        protocol: {
            let p = r.get::<String, _>("protocol");
            if p.is_empty() {
                "terminal".to_string()
            } else {
                p
            }
        },
        created_at: r.get::<String, _>("created_at"),
        updated_at: r.get::<String, _>("updated_at"),
    }
}

const COLUMNS: &str =
    "name, label, setup, launch, resume, reports_status, protocol, created_at, updated_at";

/// Every custom agent, ordered by name.
pub async fn list(db: &Db) -> Result<Vec<CustomAgent>> {
    let sql = format!("SELECT {COLUMNS} FROM custom_agents ORDER BY name");
    let rows = sqlx::query(&sql).fetch_all(db).await?;
    Ok(rows.iter().map(row_to_agent).collect())
}

/// One custom agent by name, or `None` when it isn't defined.
pub async fn get(db: &Db, name: &str) -> Result<Option<CustomAgent>> {
    let sql = format!("SELECT {COLUMNS} FROM custom_agents WHERE name = ?");
    let row = sqlx::query(&sql).bind(name).fetch_optional(db).await?;
    Ok(row.as_ref().map(row_to_agent))
}

/// Whether a custom agent by this name exists.
pub async fn exists(db: &Db, name: &str) -> Result<bool> {
    Ok(get(db, name).await?.is_some())
}

/// Upsert a custom agent. The caller is expected to [`validate_name`] /
/// [`validate_fields`] first; this only touches the database. `created_at` is
/// preserved on update; `updated_at` is refreshed.
pub async fn set(db: &Db, agent: &CustomAgent) -> Result<()> {
    let now = now_iso();
    // Empty protocol is normalized to the terminal default at rest.
    let protocol = if agent.protocol.trim().is_empty() {
        "terminal"
    } else {
        agent.protocol.trim()
    };
    sqlx::query(
        "INSERT INTO custom_agents
             (name, label, setup, launch, resume, reports_status, protocol, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(name) DO UPDATE SET
             label = excluded.label,
             setup = excluded.setup,
             launch = excluded.launch,
             resume = excluded.resume,
             reports_status = excluded.reports_status,
             protocol = excluded.protocol,
             updated_at = excluded.updated_at",
    )
    .bind(&agent.name)
    .bind(agent.label.trim())
    .bind(&agent.setup)
    .bind(&agent.launch)
    .bind(&agent.resume)
    .bind(i64::from(agent.reports_status))
    .bind(protocol)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    tracing::debug!(name = %agent.name, "custom_agents set");
    Ok(())
}

/// Delete a custom agent. Removing an absent name is a no-op (returns `false`).
pub async fn remove(db: &Db, name: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM custom_agents WHERE name = ?")
        .bind(name)
        .execute(db)
        .await?;
    let removed = res.rows_affected() > 0;
    tracing::debug!(name, removed, "custom_agents remove");
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> CustomAgent {
        CustomAgent {
            name: name.to_string(),
            label: "Aider".to_string(),
            setup: String::new(),
            launch: "aider --message".to_string(),
            resume: String::new(),
            reports_status: false,
            protocol: "terminal".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn validate_name_accepts_slugs() {
        assert!(validate_name("aider").is_ok());
        assert!(validate_name("gpt-cli").is_ok());
        assert!(validate_name("my_agent2").is_ok());
        // `shell` is no longer builtin, so it is a legal custom name.
        assert!(validate_name("shell").is_ok());
    }

    #[test]
    fn validate_name_rejects_bad_shapes_and_builtins() {
        assert!(validate_name("").is_err());
        assert!(validate_name("2fast").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("dots.dots").is_err());
        // Builtin runtimes and retired role names are reserved.
        assert!(validate_name("claude").is_err());
        assert!(validate_name("codex").is_err());
        assert!(validate_name("concierge").is_err());
    }

    #[test]
    fn validate_fields_requires_only_a_label() {
        let mut a = agent("aider");
        assert!(validate_fields(&a).is_ok());
        // A blank label is rejected.
        a.label = "  ".to_string();
        assert!(validate_fields(&a).is_err());
        // A command-less agent (bare shell) is allowed, as long as it has a label.
        a.label = "Bare".to_string();
        a.launch = String::new();
        a.setup = String::new();
        assert!(validate_fields(&a).is_ok());
    }

    #[test]
    fn validate_fields_checks_the_protocol() {
        let mut a = agent("aider");
        a.protocol = "terminal".to_string();
        assert!(validate_fields(&a).is_ok());
        // A blank protocol is allowed (normalized to terminal at rest).
        a.protocol = String::new();
        assert!(validate_fields(&a).is_ok());
        // An unknown protocol is rejected.
        a.protocol = "grpc".to_string();
        assert!(validate_fields(&a).is_err());
        // An acp agent needs a launch command (its adapter).
        a.protocol = "acp".to_string();
        a.launch = "my-adapter --acp".to_string();
        assert!(validate_fields(&a).is_ok());
        a.launch = String::new();
        assert!(validate_fields(&a).is_err());
    }

    #[tokio::test]
    async fn protocol_round_trips_and_defaults_to_terminal() {
        let db = crate::db::connect_in_memory().await.unwrap();
        set(
            &db,
            &CustomAgent {
                protocol: "acp".to_string(),
                launch: "adapter --acp".to_string(),
                ..agent("acp-agent")
            },
        )
        .await
        .unwrap();
        assert_eq!(
            get(&db, "acp-agent").await.unwrap().unwrap().protocol,
            "acp"
        );
        // A blank protocol persists as the terminal default.
        set(
            &db,
            &CustomAgent {
                protocol: String::new(),
                ..agent("plain")
            },
        )
        .await
        .unwrap();
        assert_eq!(
            get(&db, "plain").await.unwrap().unwrap().protocol,
            "terminal"
        );
    }

    #[tokio::test]
    async fn set_get_list_remove_round_trip() {
        let db = crate::db::connect_in_memory().await.unwrap();
        assert!(list(&db).await.unwrap().is_empty());
        assert!(!exists(&db, "aider").await.unwrap());

        set(&db, &agent("aider")).await.unwrap();
        set(
            &db,
            &CustomAgent {
                reports_status: true,
                ..agent("zeta")
            },
        )
        .await
        .unwrap();

        let all = list(&db).await.unwrap();
        assert_eq!(all.len(), 2);
        // Ordered by name.
        assert_eq!(all[0].name, "aider");
        assert_eq!(all[1].name, "zeta");
        assert!(all[1].reports_status);
        assert!(exists(&db, "aider").await.unwrap());

        // Upsert replaces the mutable fields but keeps created_at.
        let first_created = all[0].created_at.clone();
        set(
            &db,
            &CustomAgent {
                label: "Aider v2".to_string(),
                ..agent("aider")
            },
        )
        .await
        .unwrap();
        let updated = get(&db, "aider").await.unwrap().unwrap();
        assert_eq!(updated.label, "Aider v2");
        assert_eq!(updated.created_at, first_created);

        assert!(remove(&db, "aider").await.unwrap());
        assert!(!remove(&db, "aider").await.unwrap());
        assert_eq!(list(&db).await.unwrap().len(), 1);
    }
}
