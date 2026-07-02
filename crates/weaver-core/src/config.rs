//! Key/value settings stored in the `settings` table.
//!
//! The table itself is a plain key/value store, but every setting weaver knows
//! about is also declared in [`REGISTRY`] — a single source of truth that gives
//! each key a label, help text, type, default, and group. The registry drives
//! validation ([`validate`]) and the settings pane in the web UI
//! ([`describe`]); the raw [`get`]/[`set`] helpers still accept arbitrary keys
//! so nothing breaks if a setting is read before it is registered.

use anyhow::Result;
use serde::Serialize;
use sqlx::Row;

use crate::db::{now_iso, Db};

pub const DEFAULT_AGENT: &str = "claude";
pub const DEFAULT_AGENT_MODEL: &str = "";
pub const DEFAULT_AGENT_EFFORT: &str = "";
/// The agent that backs the fleet Chat concierge when `concierge.runtime` is
/// unset. Claude is the default because only it fires weaver's lifecycle hooks.
pub const DEFAULT_CONCIERGE_RUNTIME: &str = "claude";
pub const DEFAULT_CONCIERGE_MODEL: &str = "";
pub const DEFAULT_CONCIERGE_EFFORT: &str = "";
/// Whether the server adopts orphaned sessions on startup. Off by default:
/// the operator opts in via `weaver config set server.auto_adopt true`.
pub const DEFAULT_AUTO_ADOPT: bool = false;
/// Whether loom polls GitHub (via the `gh` CLI) for each session's PR, review,
/// and check status. On by default, but a no-op wherever `gh` is missing.
pub const DEFAULT_GITHUB_POLL: bool = true;
/// Whether loom archives a session automatically once its pull request merges.
/// On by default — a merged branch's worktree has served its purpose.
pub const DEFAULT_GITHUB_ARCHIVE_ON_MERGE: bool = true;
/// The phrase an `issue_comment` must begin with to trigger a loom session via
/// the GitHub webhook. Fixed (not free-text) in v1 to shrink the abuse surface.
pub const DEFAULT_GITHUB_TRIGGER_PHRASE: &str = "@loom";
/// The palette the browser terminal (xterm.js) renders with. `dark` keeps the
/// classic black background; `light` swaps in a light, readable palette.
pub const DEFAULT_TERMINAL_THEME: &str = "dark";
/// Whether requests from the loopback interface are trusted as the machine owner
/// without a token or login. On by default: it keeps the local CLI, the agent,
/// and watch scripts working with no configuration. Turn it off behind a
/// same-host reverse proxy, where forwarded requests appear to come from
/// loopback (the proxy and local automation then authenticate with tokens).
pub const DEFAULT_TRUST_LOOPBACK: bool = true;
/// Whether the login cookie carries the `Secure` attribute (HTTPS-only). Off by
/// default so plain-HTTP and direct-IP access work; turn it on when loom is
/// reached over HTTPS (e.g. behind a TLS-terminating proxy).
pub const DEFAULT_COOKIE_SECURE: bool = false;
/// Wall-clock budget for a repo's `.weaver/config.toml` `[setup]` script, run
/// when a session launches against an allowlisted repo. A run that overruns is
/// killed and the session is left in a visible error state. 600s mirrors the
/// watch/lint-review precedent.
pub const DEFAULT_SETUP_TIMEOUT_SECS: i64 = 600;

// ---------------------------------------------------------------------------
// Setting registry
// ---------------------------------------------------------------------------

/// The value type of a registered setting. Drives both validation and the
/// input control rendered in the settings pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    /// Free-form text (commands, names, …).
    String,
    /// A signed integer.
    Int,
    /// A boolean — stored as `true`/`false`.
    Bool,
    /// A choice from a fixed set of strings ([`SettingSpec::options`]). Renders
    /// as a dropdown; validated against the allowed values.
    Enum,
}

/// A statically declared setting: everything the UI and validator need to know
/// about one configuration key.
#[derive(Debug, Clone, Serialize)]
pub struct SettingSpec {
    /// Dotted key, e.g. `agent.default`.
    pub key: &'static str,
    /// Short human-readable name shown in the settings pane.
    pub label: &'static str,
    /// One- or two-sentence explanation of what the setting does.
    pub description: &'static str,
    /// Value type — determines validation and the input control.
    pub kind: SettingKind,
    /// Value used when the key is absent from the `settings` table.
    pub default: &'static str,
    /// Heading the setting is grouped under in the UI.
    pub group: &'static str,
    /// The allowed values for a [`SettingKind::Enum`] setting, in display
    /// order. Empty for every other kind.
    pub options: &'static [&'static str],
}

/// Every setting weaver knows about. Adding a row here is all it takes to make
/// a new option appear in the settings pane.
pub const REGISTRY: &[SettingSpec] = &[
    SettingSpec {
        key: "agent.default",
        label: "Default agent",
        description: "Agent launched in each new session's terminal when `loom \
            launch` is given no `--agent`.",
        kind: SettingKind::String,
        default: DEFAULT_AGENT,
        group: "Agents",
        options: &[],
    },
    SettingSpec {
        key: "agent.model",
        label: "Default model",
        description: "Model selector used for new sessions when launch/create \
            requests do not specify one.",
        kind: SettingKind::String,
        default: DEFAULT_AGENT_MODEL,
        group: "Agents",
        options: &[],
    },
    SettingSpec {
        key: "agent.effort",
        label: "Default effort",
        description: "Reasoning effort used for new sessions when launch/create \
            requests do not specify one.",
        kind: SettingKind::String,
        default: DEFAULT_AGENT_EFFORT,
        group: "Agents",
        options: &[],
    },
    SettingSpec {
        key: "concierge.runtime",
        label: "Concierge agent",
        description: "Which agent backs the fleet Chat concierge. `claude` runs \
            the Claude Code TUI with full weaver status hooks. `codex` runs Codex: \
            its conversation still renders in the Chat view, but Codex does not \
            fire weaver's lifecycle hooks, so the concierge shows no live \
            working/idle status and the Chat view won't auto-refresh on each \
            reply (use the Refresh button).",
        kind: SettingKind::String,
        default: DEFAULT_CONCIERGE_RUNTIME,
        group: "Agents",
        options: &[],
    },
    SettingSpec {
        key: "concierge.model",
        label: "Concierge model",
        description: "Model selector used when Chat starts or resets the fleet \
            concierge.",
        kind: SettingKind::String,
        default: DEFAULT_CONCIERGE_MODEL,
        group: "Agents",
        options: &[],
    },
    SettingSpec {
        key: "concierge.effort",
        label: "Concierge effort",
        description: "Reasoning effort used when Chat starts or resets the \
            fleet concierge.",
        kind: SettingKind::String,
        default: DEFAULT_CONCIERGE_EFFORT,
        group: "Agents",
        options: &[],
    },
    SettingSpec {
        key: "server.auto_adopt",
        label: "Auto-adopt on startup",
        description: "When enabled, the server recreates the terminal session for \
            every recoverable session on startup, rather than leaving them \
            `orphaned` for manual adoption.",
        kind: SettingKind::Bool,
        default: "false",
        group: "Server",
        options: &[],
    },
    SettingSpec {
        key: "github.poll",
        label: "Poll GitHub for PR status",
        description: "When enabled, loom uses the `gh` CLI to fetch each \
            active session's pull request — its link, review decision, and \
            check rollup — and surfaces it on the dashboard. A no-op for \
            repositories without a GitHub remote, or wherever `gh` is not \
            installed.",
        kind: SettingKind::Bool,
        default: "true",
        group: "GitHub",
        options: &[],
    },
    SettingSpec {
        key: "github.archive_on_merge",
        label: "Archive on PR merge",
        description: "When enabled, loom archives a session automatically once \
            its pull request is merged — tearing down the terminal session, \
            removing the worktree, and closing the weaver issues that session \
            was working, while keeping the branch and its history. Requires \
            GitHub polling.",
        kind: SettingKind::Bool,
        default: "true",
        group: "GitHub",
        options: &[],
    },
    SettingSpec {
        key: "github.trigger_phrase",
        label: "GitHub trigger phrase",
        description: "The phrase an issue comment must begin with for the GitHub \
            webhook to launch a session against that repo (default `@loom`). \
            Matched case-insensitively against the start of the comment; kept a \
            fixed prefix rather than free-text to limit the abuse surface. The \
            webhook is only active once `LOOM_GITHUB_WEBHOOK_SECRET` is configured.",
        kind: SettingKind::String,
        default: DEFAULT_GITHUB_TRIGGER_PHRASE,
        group: "GitHub",
        options: &[],
    },
    SettingSpec {
        key: "auth.trust_loopback",
        label: "Trust loopback requests",
        description: "When enabled, requests from 127.0.0.1/::1 are trusted as \
            the machine owner with no token or login — keeping the local CLI, \
            the agent, and watch scripts working with zero configuration. \
            Turn this OFF behind a same-host reverse proxy, where forwarded \
            requests appear to come from loopback; local automation then uses \
            the machine token loom injects.",
        kind: SettingKind::Bool,
        default: "true",
        group: "Authentication",
        options: &[],
    },
    SettingSpec {
        key: "auth.cookie_secure",
        label: "Secure login cookie",
        description: "When enabled, the login session cookie is marked `Secure` \
            so the browser only sends it over HTTPS. Enable this when loom is \
            served over HTTPS (typically behind a TLS-terminating proxy); leave \
            it off for plain-HTTP or direct-IP access, where a Secure cookie \
            would never be sent.",
        kind: SettingKind::Bool,
        default: "false",
        group: "Authentication",
        options: &[],
    },
    SettingSpec {
        key: "auth.base_url",
        label: "External base URL",
        description: "The public URL loom is reached at (e.g. \
            `https://loom.example.com`), used to build the GitHub OAuth callback. \
            Leave blank to derive it from each request's Host header (honouring \
            `X-Forwarded-Proto`); set it when that derivation is wrong behind a \
            proxy.",
        kind: SettingKind::String,
        default: "",
        group: "Authentication",
        options: &[],
    },
    SettingSpec {
        key: "terminal.theme",
        label: "Terminal theme",
        description: "Colour palette for the in-browser terminal. `dark` is \
            the classic black background; `light` swaps in a light, readable \
            palette. Takes effect the next time a terminal is opened.",
        kind: SettingKind::Enum,
        default: DEFAULT_TERMINAL_THEME,
        group: "Appearance",
        options: &["dark", "light"],
    },
    SettingSpec {
        key: "watch.enabled",
        label: "Enable watches",
        description: "Master switch for the Watch engine — the periodic / \
            triggered watch programs that survey the fleet and stamp triage \
            marks. On by default: turn it off to stop every watch cold, \
            regardless of the individual per-watch toggles.",
        kind: SettingKind::Bool,
        default: "true",
        group: "Watch",
        options: &[],
    },
    SettingSpec {
        key: "watch.default_timeout_secs",
        label: "Round timeout (seconds)",
        description: "Wall-clock budget for one watch round. A round that \
            overruns is killed and recorded as an error; the next trigger still \
            fires. Mirrors the lint-review 600s precedent.",
        kind: SettingKind::Int,
        default: "600",
        group: "Watch",
        options: &[],
    },
    SettingSpec {
        key: "watch.default_cooldown_secs",
        label: "Default cooldown (seconds)",
        description: "Minimum gap between two rounds of the same watch when \
            it does not set its own cooldown. A re-fire inside the gap is \
            skipped, so a chatty event stream can't hammer a watcher.",
        kind: SettingKind::Int,
        default: "0",
        group: "Watch",
        options: &[],
    },
    SettingSpec {
        key: "watch.adopt_warm",
        label: "Adopt warm sessions on startup",
        description: "When enabled, the server re-adopts each engine-managed \
            (warm) watch session whose terminal is gone on startup — recreating \
            it so a watcher resumes its across-round memory after a daemon \
            restart. Independent of the fleet-wide `server.auto_adopt`: warm \
            infrastructure is recovered even when ordinary sessions are left \
            orphaned. A warm session whose owning watch has been deleted is \
            archived instead of adopted.",
        kind: SettingKind::Bool,
        default: "true",
        group: "Watch",
        options: &[],
    },
    SettingSpec {
        key: "watch.stale_after_secs",
        label: "Stale-after (seconds)",
        description: "How long a non-terminal session may go without any activity \
            before the monitor emits a one-shot `stale` event into the stream — a \
            reactive trigger a watch can match. Edge-detected, so a session \
            that stays quiet is announced once, not every tick.",
        kind: SettingKind::Int,
        default: "1800",
        group: "Watch",
        options: &[],
    },
    SettingSpec {
        key: "ide.enabled",
        label: "Enable embedded editor",
        description: "Master switch for the per-session embedded VS Code \
            (code-server), reverse-proxied beside the terminal. On by default; \
            turn it off to hide the editor panel and stop the proxy. A no-op \
            wherever `code-server` is not installed (the panel reports that).",
        kind: SettingKind::Bool,
        default: "true",
        group: "Editor",
        options: &[],
    },
    SettingSpec {
        key: "ide.idle_timeout_secs",
        label: "Editor idle timeout (seconds)",
        description: "How long an embedded code-server may sit with no proxied \
            request before loom retires it. The next time the editor is opened \
            for that session a fresh one is spawned. Lower it to reclaim memory \
            sooner; raise it to keep editors warm across longer pauses.",
        kind: SettingKind::Int,
        default: "1800",
        group: "Editor",
        options: &[],
    },
    SettingSpec {
        key: "ide.command",
        label: "code-server command",
        description: "Override the command loom launches for the embedded editor \
            (leading arguments allowed). Empty uses `code-server` on `PATH`. The \
            `WEAVER_IDE_CMD` environment variable takes precedence over this.",
        kind: SettingKind::String,
        default: "",
        group: "Editor",
        options: &[],
    },
    SettingSpec {
        key: "setup.timeout_secs",
        label: "Repo setup timeout (seconds)",
        description: "Wall-clock budget for a repo's `.weaver/config.toml` \
            `[setup]` script, run in the worktree before the agent starts when a \
            session launches against an allowlisted (registered) repo. A run that \
            overruns is killed and the session is left in a visible error state \
            rather than launching a half-provisioned worktree. Setup only runs \
            for registered repos — the boundary that keeps it from executing \
            arbitrary code from an unknown repo.",
        kind: SettingKind::Int,
        default: "600",
        group: "Sessions",
        options: &[],
    },
    SettingSpec {
        key: "session.log_dir",
        label: "Session log directory",
        description: "Where the agent's conversation log is captured when a \
            session is archived: a normalized `chat.json` and a rendered \
            `chat.md` are written under `<dir>/<branch>/`. Empty uses \
            `~/.iris/logs/sessions`. Point it at a persistent path when running \
            in a container where the default home isn't a mounted volume.",
        kind: SettingKind::String,
        default: "",
        group: "Sessions",
        options: &[],
    },
];

/// Whether the Watch engine master switch is on. On by default.
pub const DEFAULT_WATCH_ENABLED: bool = true;

/// Whether the server re-adopts engine-managed (warm) watch sessions on
/// startup. On by default and independent of [`DEFAULT_AUTO_ADOPT`]: a warm
/// session is infrastructure a watcher depends on, so it is recovered across a
/// restart even when ordinary fleet sessions are left orphaned.
pub const DEFAULT_WATCH_ADOPT_WARM: bool = true;

/// How many seconds a non-terminal session may be idle before the monitor emits
/// a one-shot `stale` event. 30 minutes by default.
pub const DEFAULT_WATCH_STALE_AFTER_SECS: i64 = 1800;

/// Look up the [`SettingSpec`] for a key, if it is a registered setting.
pub fn spec(key: &str) -> Option<&'static SettingSpec> {
    REGISTRY.iter().find(|s| s.key == key)
}

/// Check that `value` is acceptable for `key`. Unregistered keys accept any
/// value; registered keys are checked against their [`SettingKind`]. The error
/// is a key-free reason (e.g. `expects an integer, got 'soon'`) so callers can
/// prefix it with whatever context — a bare key, a field path — they like.
pub fn validate(key: &str, value: &str) -> std::result::Result<(), String> {
    let Some(spec) = spec(key) else {
        return Ok(());
    };
    match spec.kind {
        SettingKind::String => Ok(()),
        SettingKind::Int => value
            .trim()
            .parse::<i64>()
            .map(|_| ())
            .map_err(|_| format!("expects an integer, got '{value}'")),
        SettingKind::Bool => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" | "false" | "0" | "no" | "off" => Ok(()),
            _ => Err(format!("expects true or false, got '{value}'")),
        },
        SettingKind::Enum => {
            if spec.options.contains(&value.trim()) {
                Ok(())
            } else {
                Err(format!(
                    "expects one of {}, got '{value}'",
                    spec.options.join(", ")
                ))
            }
        }
    }
}

/// A registered setting paired with its current effective value — what the
/// settings pane renders. `value` is the stored value, or the default when the
/// key is absent; `is_default` says which.
#[derive(Debug, Clone, Serialize)]
pub struct SettingView {
    #[serde(flatten)]
    pub spec: &'static SettingSpec,
    /// Effective value: the stored value, or `spec.default` when unset.
    pub value: String,
    /// True when no value is stored and `value` is the default.
    pub is_default: bool,
}

/// The full registry with each setting's current effective value, ordered as
/// declared in [`REGISTRY`].
pub async fn describe(db: &Db) -> Result<Vec<SettingView>> {
    let stored: std::collections::HashMap<String, String> = list(db).await?.into_iter().collect();
    Ok(REGISTRY
        .iter()
        .map(|spec| match stored.get(spec.key) {
            Some(value) => SettingView {
                spec,
                value: value.clone(),
                is_default: false,
            },
            None => SettingView {
                spec,
                value: spec.default.to_string(),
                is_default: true,
            },
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Raw key/value access
// ---------------------------------------------------------------------------

pub async fn get(db: &Db, key: &str) -> Option<String> {
    let value = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|r| r.get::<String, _>("value"));
    tracing::debug!(key, found = value.is_some(), "config get");
    value
}

pub async fn get_or(db: &Db, key: &str, default: &str) -> String {
    get(db, key).await.unwrap_or_else(|| default.to_string())
}

/// Read a boolean setting. Accepts `true`/`1`/`yes`/`on` (case-insensitively)
/// as true and `false`/`0`/`no`/`off` as false; anything else falls back to
/// `default`.
pub async fn get_bool(db: &Db, key: &str, default: bool) -> bool {
    match get(db, key).await {
        Some(v) => match v.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => true,
            "false" | "0" | "no" | "off" => false,
            _ => default,
        },
        None => default,
    }
}

/// One requested change: `Some(value)` writes a key, `None` clears it (so the
/// key falls back to its registered default).
pub type Change = (String, Option<String>);

/// Apply a batch of [`Change`]s atomically — either all writes land or none do.
/// Callers are expected to [`validate`] each value first; `apply` itself only
/// touches the database.
pub async fn apply(db: &Db, changes: &[Change]) -> Result<()> {
    let mut tx = db.begin().await?;
    let now = now_iso();
    for (key, value) in changes {
        match value {
            Some(value) => {
                tracing::debug!(key, value, "config set");
                sqlx::query(
                    "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)
                     ON CONFLICT(key) DO UPDATE
                       SET value = excluded.value, updated_at = excluded.updated_at",
                )
                .bind(key)
                .bind(value)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                tracing::debug!(key, "config reset to default");
                sqlx::query("DELETE FROM settings WHERE key = ?")
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn list(db: &Db) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query("SELECT key, value FROM settings ORDER BY key")
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("key"), r.get::<String, _>("value")))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keys_are_unique() {
        let mut keys: Vec<&str> = REGISTRY.iter().map(|s| s.key).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "duplicate key in REGISTRY");
    }

    #[test]
    fn registered_defaults_pass_their_own_validation() {
        for s in REGISTRY {
            assert!(
                validate(s.key, s.default).is_ok(),
                "default for '{}' fails validation",
                s.key
            );
        }
    }

    #[test]
    fn validate_checks_kinds_and_ignores_unknown_keys() {
        // Bool-kind validation: only true/false-ish values pass.
        assert!(validate("server.auto_adopt", "yes").is_ok());
        assert!(validate("server.auto_adopt", "maybe").is_err());
        // Unregistered keys are free-form.
        assert!(validate("some.future.key", "anything").is_ok());
    }

    #[test]
    fn validate_enum_accepts_only_listed_options() {
        assert!(validate("terminal.theme", "dark").is_ok());
        assert!(validate("terminal.theme", "light").is_ok());
        // Surrounding whitespace is tolerated, like the other kinds.
        assert!(validate("terminal.theme", " light ").is_ok());
        // Anything outside the option set is rejected, and the error lists them.
        let err = validate("terminal.theme", "solarized").unwrap_err();
        assert!(err.contains("dark"), "error should list the options: {err}");
        assert!(
            err.contains("light"),
            "error should list the options: {err}"
        );
    }

    #[test]
    fn enum_kind_iff_options_present() {
        // The two are coupled: an Enum must declare its choices, and only an
        // Enum may. This keeps the dropdown and validator in lockstep.
        for s in REGISTRY {
            let is_enum = s.kind == SettingKind::Enum;
            assert_eq!(
                is_enum,
                !s.options.is_empty(),
                "'{}': options must be non-empty iff kind is Enum",
                s.key
            );
        }
    }

    #[tokio::test]
    async fn describe_serializes_enum_kind_and_options_for_the_frontend() {
        // The settings pane keys off `kind` and `options` to render a dropdown,
        // so guard the JSON shape the API hands it.
        let db = crate::db::connect_in_memory().await.unwrap();
        let views = describe(&db).await.unwrap();
        let theme = views
            .iter()
            .find(|v| v.spec.key == "terminal.theme")
            .expect("terminal.theme should be registered");
        let json = serde_json::to_value(theme).unwrap();
        assert_eq!(json["kind"], "enum");
        assert_eq!(json["options"], serde_json::json!(["dark", "light"]));
        assert_eq!(json["value"], "dark");
        assert_eq!(json["is_default"], true);
    }

    #[tokio::test]
    async fn describe_reports_defaults_then_stored_values() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let before = describe(&db).await.unwrap();
        let auto_adopt = before
            .iter()
            .find(|v| v.spec.key == "server.auto_adopt")
            .unwrap();
        assert!(auto_adopt.is_default);
        assert_eq!(auto_adopt.value, "false");

        apply(&db, &[("server.auto_adopt".into(), Some("true".into()))])
            .await
            .unwrap();
        let after = describe(&db).await.unwrap();
        let auto_adopt = after
            .iter()
            .find(|v| v.spec.key == "server.auto_adopt")
            .unwrap();
        assert!(!auto_adopt.is_default);
        assert_eq!(auto_adopt.value, "true");
    }

    #[tokio::test]
    async fn apply_is_atomic_and_a_none_change_resets_to_default() {
        let db = crate::db::connect_in_memory().await.unwrap();
        apply(
            &db,
            &[(
                "unknown.legacy".into(),
                Some("kept but unregistered".into()),
            )],
        )
        .await
        .unwrap();
        assert_eq!(
            get(&db, "unknown.legacy").await.as_deref(),
            Some("kept but unregistered")
        );
        // A `None` change clears the row so the default applies again.
        apply(&db, &[("unknown.legacy".into(), None)])
            .await
            .unwrap();
        assert_eq!(get(&db, "unknown.legacy").await, None);
    }
}
