//! The Overlooker model: periodic / triggered watch programs over the fleet,
//! plus the round-execution audit (`overlooker_runs`).
//!
//! This module is **pure storage + parsing**. The engine that actually *runs* an
//! overlooker — the timer, the dispatcher, the program executor — lives in the
//! loom daemon, which stays the single owner of the terminal/session runtime. Here
//! we only describe *what* to watch (the trigger, scope, program, capabilities)
//! and record *what happened* (each round's outcome and actions).

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::branch::new_id;
use crate::db::{now_iso, Db};

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// One configured watch definition. `trigger_spec`, `scope`, `params`, and
/// `capabilities` are stored as JSON text; the typed accessors below parse them.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Overlooker {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger_spec: String,
    pub scope: String,
    /// `builtin:<name>` for a stock program, or a path under
    /// `~/.weaver/overlookers/` for a custom one.
    pub program: String,
    pub params: String,
    pub capabilities: String,
    pub model: String,
    pub effort: String,
    pub cooldown_secs: i64,
    pub last_run_at: Option<String>,
    pub next_run_at: Option<String>,
    pub warm_session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The parsed trigger — a watch's **subscription manifest**: what wakes a round.
/// A scheduled watch carries a `cron` (or `every`) cadence; a reactive one
/// subscribes to one or more normalized trigger events via `on` (e.g.
/// `["pr.merged", "session.exited=error"]`). An optional `repo` pins the watch
/// to one repository. The manifest is what the watch *script declares* (it is
/// emitted from the script's register mode and stored here), so the script —
/// not whoever wired it up — decides which events it cares about.
///
/// Stored as JSON, e.g. `{"cron":"0 * * * *"}` or
/// `{"on":["pr.merged","pr.opened"]}`. The legacy single-event shape
/// (`{"event":"attention","level":"blocked"}`) is still parsed and folded into
/// the subscription set, so watches predating the manifest keep working.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Trigger {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    /// The normalized trigger events this watch subscribes to. Each entry is an
    /// event name (`"pr.merged"`) or a `name=level` filter
    /// (`"session.attention=blocked"`). Empty means "no reactive subscription".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<String>,
    /// Legacy single-event subscription, kept for back-compat — folded into the
    /// effective set by [`Trigger::subscriptions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Map a legacy raw event kind to its normalized trigger-event name, so a watch
/// that predates the manifest (and named the raw stream kind) still matches the
/// normalized names the dispatcher now emits. An unknown name passes through.
fn normalize_event_name(name: &str) -> &str {
    match name {
        "attention" => "session.attention",
        "triage" => "triage.changed",
        "stale" => "session.stale",
        "pr_red" => "pr.checks_red",
        "pr_green" => "pr.checks_green",
        "pr_merged" => "pr.merged",
        "pr_opened" => "pr.opened",
        other => other,
    }
}

impl Trigger {
    /// A scheduled trigger fires on a clock cadence (the timer produces the
    /// event); a non-scheduled one is reactive (matches a stream event).
    pub fn is_scheduled(&self) -> bool {
        self.cron.is_some() || self.every.is_some()
    }

    /// The effective event subscriptions as borrowed `(event_name, opt_level)`
    /// pairs: the `on` list (each entry an event name or `name=level`) plus the
    /// legacy single `event`/`level`, with names normalized to the dispatcher's
    /// vocabulary. Borrowed, not collected, so matching an event needs no
    /// allocation.
    fn subscription_pairs(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        let on = self.on.iter().filter_map(|entry| {
            let (name, level) = match entry.split_once('=') {
                Some((name, level)) => (name.trim(), Some(level.trim())),
                None => (entry.trim(), None),
            };
            let name = normalize_event_name(name);
            (!name.is_empty()).then_some((name, level))
        });
        let legacy = self.event.as_deref().filter(|e| !e.is_empty()).map(|e| {
            (
                normalize_event_name(e),
                self.level.as_deref().filter(|l| !l.is_empty()),
            )
        });
        on.chain(legacy)
    }

    /// The effective subscriptions as owned `(event_name, optional_level)` pairs
    /// — the [`subscription_pairs`](Self::subscription_pairs) view, collected.
    pub fn subscriptions(&self) -> Vec<(String, Option<String>)> {
        self.subscription_pairs()
            .map(|(name, level)| (name.to_string(), level.map(str::to_string)))
            .collect()
    }

    /// Whether this trigger matches a normalized trigger `event` whose payload
    /// `level` and originating `repo` are as given. Matches when any
    /// subscription names this event and either declares no level or one equal
    /// to the event's. Scheduled triggers never match a stream event directly —
    /// they fire off their own `cron` tick.
    pub fn matches_event(&self, event: &str, level: Option<&str>, repo: Option<&str>) -> bool {
        if !self.repo_matches(repo) {
            return false;
        }
        self.subscription_pairs().any(|(name, want_level)| {
            name == event && want_level.is_none_or(|wl| level == Some(wl))
        })
    }

    /// Whether the trigger's optional `repo` filter admits an event from `repo`.
    /// No filter admits everything; a filter admits only an exact match.
    pub fn repo_matches(&self, repo: Option<&str>) -> bool {
        match self.repo.as_deref() {
            None => true,
            Some(want) => repo == Some(want),
        }
    }
}

/// The parsed fleet query a round surveys. Minimal by design: an `attention`
/// filter (`!ok` for "anything not ok", or an exact level) and an optional
/// `repo`. The engine widens this as stock programs grow.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Scope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

impl Scope {
    /// Whether a branch with the given `attention` level and `repo_root` is in
    /// scope for this query.
    pub fn admits(&self, attention: &str, repo_root: &str) -> bool {
        if let Some(want) = self.attention.as_deref() {
            let ok = match want.strip_prefix('!') {
                Some(excluded) => attention != excluded,
                None => attention == want,
            };
            if !ok {
                return false;
            }
        }
        if let Some(want) = self.repo.as_deref() {
            if repo_root != want {
                return false;
            }
        }
        true
    }
}

impl Overlooker {
    /// Parse the trigger; an unparseable spec yields the empty (never-matching,
    /// never-scheduled) trigger rather than erroring a whole round.
    pub fn trigger(&self) -> Trigger {
        serde_json::from_str(&self.trigger_spec).unwrap_or_default()
    }

    /// Parse the scope query; an unparseable scope yields the all-admitting
    /// empty query.
    pub fn scope(&self) -> Scope {
        serde_json::from_str(&self.scope).unwrap_or_default()
    }

    /// The raw `params` JSON (a stock program reads its `prompt` etc. here).
    pub fn params(&self) -> Value {
        serde_json::from_str(&self.params).unwrap_or(Value::Null)
    }

    /// Whether this overlooker runs in **warm mode** — the engine keeps one
    /// long-lived, engine-managed session for it (hidden from the fleet) so it
    /// has across-round memory. Opt in via `params.warm = true`; off by default,
    /// so an ordinary overlooker spawns no session. Carried in `params` rather
    /// than a dedicated column to keep the opt-in a zero-migration knob the
    /// existing `params` plumbing (REST, CLI, PyO3) already round-trips.
    pub fn warm(&self) -> bool {
        self.params()
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// The declared capability set (the intervention ladder).
    pub fn capabilities(&self) -> Vec<String> {
        serde_json::from_str(&self.capabilities).unwrap_or_default()
    }

    /// Whether the overlooker holds `cap` (gating the intervention ladder).
    /// `observe` is always granted.
    pub fn has_capability(&self, cap: &str) -> bool {
        cap == "observe" || self.capabilities().iter().any(|c| c == cap)
    }
}

/// The capabilities an overlooker can hold, calm → loud (the intervention
/// ladder). `observe` is implicit; the rest are explicit grants.
pub const CAPABILITIES: &[&str] = &[
    "observe",
    "mark",
    "escalate",
    "nudge",
    "interrupt",
    "launch",
];

/// Round outcomes recorded on `overlooker_runs.outcome`.
pub const OUTCOMES: &[&str] = &["ok", "noop", "skipped", "error"];

// ---------------------------------------------------------------------------
// Create / read / update / delete
// ---------------------------------------------------------------------------

/// The fields needed to register a new overlooker. JSON-bearing fields take the
/// already-serialized text; the typed `Trigger`/`Scope` serialize cleanly into
/// them at the call site.
#[derive(Debug, Clone)]
pub struct NewOverlooker {
    pub name: String,
    pub trigger_spec: String,
    pub scope: String,
    pub program: String,
    pub params: String,
    pub capabilities: Vec<String>,
    pub model: String,
    pub effort: String,
    pub cooldown_secs: i64,
}

impl Default for NewOverlooker {
    fn default() -> Self {
        Self {
            name: String::new(),
            trigger_spec: "{}".to_string(),
            scope: "{}".to_string(),
            program: "builtin:status".to_string(),
            params: "{}".to_string(),
            capabilities: vec![
                "observe".to_string(),
                "mark".to_string(),
                "escalate".to_string(),
            ],
            model: String::new(),
            effort: String::new(),
            cooldown_secs: 0,
        }
    }
}

pub async fn create(db: &Db, new: &NewOverlooker) -> Result<Overlooker> {
    let id = new_id();
    let now = now_iso();
    let caps = serde_json::to_string(&new.capabilities)?;
    sqlx::query(
        "INSERT INTO overlookers
           (id, name, enabled, trigger_spec, scope, program, params, capabilities,
            model, effort, cooldown_secs, created_at, updated_at)
         VALUES (?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.name)
    .bind(&new.trigger_spec)
    .bind(&new.scope)
    .bind(&new.program)
    .bind(&new.params)
    .bind(&caps)
    .bind(&new.model)
    .bind(&new.effort)
    .bind(new.cooldown_secs)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    get(db, &id)
        .await?
        .ok_or_else(|| anyhow!("overlooker vanished after insert"))
}

pub async fn get(db: &Db, id: &str) -> Result<Option<Overlooker>> {
    Ok(
        sqlx::query_as::<_, Overlooker>("SELECT * FROM overlookers WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn get_by_name(db: &Db, name: &str) -> Result<Option<Overlooker>> {
    Ok(
        sqlx::query_as::<_, Overlooker>("SELECT * FROM overlookers WHERE name = ?")
            .bind(name)
            .fetch_optional(db)
            .await?,
    )
}

/// Resolve an overlooker by id or by name (the operator CLI accepts either).
pub async fn resolve(db: &Db, key: &str) -> Result<Option<Overlooker>> {
    if let Some(o) = get(db, key).await? {
        return Ok(Some(o));
    }
    get_by_name(db, key).await
}

pub async fn list(db: &Db) -> Result<Vec<Overlooker>> {
    Ok(
        sqlx::query_as::<_, Overlooker>("SELECT * FROM overlookers ORDER BY name ASC")
            .fetch_all(db)
            .await?,
    )
}

pub async fn list_enabled(db: &Db) -> Result<Vec<Overlooker>> {
    Ok(sqlx::query_as::<_, Overlooker>(
        "SELECT * FROM overlookers WHERE enabled = 1 ORDER BY name ASC",
    )
    .fetch_all(db)
    .await?)
}

pub async fn set_enabled(db: &Db, id: &str, enabled: bool) -> Result<()> {
    sqlx::query("UPDATE overlookers SET enabled = ?, updated_at = ? WHERE id = ?")
        .bind(enabled)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// A partial update of an overlooker's mutable definition: every field is
/// optional, and only the `Some(_)` ones are written (`enabled` is handled by
/// [`set_enabled`], schedule bookkeeping by [`set_schedule`]). JSON-bearing
/// fields take the already-serialized text, mirroring [`NewOverlooker`].
#[derive(Debug, Clone, Default)]
pub struct OverlookerUpdate {
    pub trigger_spec: Option<String>,
    pub scope: Option<String>,
    pub program: Option<String>,
    pub params: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub cooldown_secs: Option<i64>,
}

impl OverlookerUpdate {
    /// Whether any field is set — lets a caller skip a no-op write.
    pub fn is_empty(&self) -> bool {
        self.trigger_spec.is_none()
            && self.scope.is_none()
            && self.program.is_none()
            && self.params.is_none()
            && self.capabilities.is_none()
            && self.model.is_none()
            && self.effort.is_none()
            && self.cooldown_secs.is_none()
    }
}

/// Apply a partial update to an overlooker's mutable fields. Each `Some(_)`
/// overwrites; `COALESCE(?, col)` leaves an absent field untouched. `updated_at`
/// always advances.
pub async fn update(db: &Db, id: &str, patch: &OverlookerUpdate) -> Result<()> {
    let caps = match &patch.capabilities {
        Some(c) => Some(serde_json::to_string(c)?),
        None => None,
    };
    sqlx::query(
        "UPDATE overlookers SET
           trigger_spec  = COALESCE(?, trigger_spec),
           scope         = COALESCE(?, scope),
           program       = COALESCE(?, program),
           params        = COALESCE(?, params),
           capabilities  = COALESCE(?, capabilities),
           model         = COALESCE(?, model),
           effort        = COALESCE(?, effort),
           cooldown_secs = COALESCE(?, cooldown_secs),
           updated_at    = ?
         WHERE id = ?",
    )
    .bind(&patch.trigger_spec)
    .bind(&patch.scope)
    .bind(&patch.program)
    .bind(&patch.params)
    .bind(&caps)
    .bind(&patch.model)
    .bind(&patch.effort)
    .bind(patch.cooldown_secs)
    .bind(now_iso())
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Record a round's schedule bookkeeping: when it last ran and when it is next
/// due (the timer advances `next_run_at`; the executor stamps `last_run_at`).
pub async fn set_schedule(
    db: &Db,
    id: &str,
    last_run_at: Option<&str>,
    next_run_at: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE overlookers SET last_run_at = COALESCE(?, last_run_at),
           next_run_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(last_run_at)
    .bind(next_run_at)
    .bind(now_iso())
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_warm_session(db: &Db, id: &str, session_id: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE overlookers SET warm_session_id = ?, updated_at = ? WHERE id = ?")
        .bind(session_id)
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: &Db, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM overlookers WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM overlooker_runs WHERE overlooker_id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Runs (the audit trail)
// ---------------------------------------------------------------------------

/// One execution of an overlooker — a "round". `actions` is the JSON list of
/// marks / nudges / etc. it took; `stdout`/`stderr`/`exit_code`/`duration_ms`
/// are the script's captured output (the execution log), and `trigger_event`
/// the normalized event that woke it (`cron` / `manual` / e.g. `pr.merged`).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct OverlookerRun {
    pub id: i64,
    pub overlooker_id: String,
    pub trigger_reason: String,
    pub trigger_event: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: String,
    pub summary: String,
    pub actions: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub created_at: String,
}

/// The closing record of a round: its outcome plus the captured execution log.
/// Grouped into a struct so [`finish_run`] doesn't grow an unreadable argument
/// list as the audit trail records more about each run.
#[derive(Debug, Clone)]
pub struct RunRecord<'a> {
    pub outcome: &'a str,
    pub summary: &'a str,
    pub actions: &'a Value,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// Open a run row at the start of a round; returns its id. `trigger_event` is
/// the normalized event that woke it. The executor closes it with
/// [`finish_run`].
pub async fn start_run(
    db: &Db,
    overlooker_id: &str,
    trigger_reason: &str,
    trigger_event: &str,
) -> Result<i64> {
    let row = sqlx::query(
        "INSERT INTO overlooker_runs (overlooker_id, trigger_reason, trigger_event, started_at)
         VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(overlooker_id)
    .bind(trigger_reason)
    .bind(trigger_event)
    .bind(now_iso())
    .fetch_one(db)
    .await?;
    Ok(sqlx::Row::get(&row, "id"))
}

/// Close a run row with its outcome, summary, actions, and captured output.
pub async fn finish_run(db: &Db, run_id: i64, rec: &RunRecord<'_>) -> Result<()> {
    sqlx::query(
        "UPDATE overlooker_runs SET finished_at = ?, outcome = ?, summary = ?, actions = ?,
           stdout = ?, stderr = ?, exit_code = ?, duration_ms = ?
         WHERE id = ?",
    )
    .bind(now_iso())
    .bind(rec.outcome)
    .bind(rec.summary)
    .bind(rec.actions.to_string())
    .bind(rec.stdout)
    .bind(rec.stderr)
    .bind(rec.exit_code)
    .bind(rec.duration_ms)
    .bind(run_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn recent_runs(db: &Db, overlooker_id: &str, limit: i64) -> Result<Vec<OverlookerRun>> {
    Ok(sqlx::query_as::<_, OverlookerRun>(
        "SELECT * FROM overlooker_runs WHERE overlooker_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(overlooker_id)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_resolve_by_id_or_name() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let o = create(
            &db,
            &NewOverlooker {
                name: "status-check".to_string(),
                trigger_spec: r#"{"cron":"0 * * * *"}"#.to_string(),
                scope: r#"{"attention":"!ok"}"#.to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(!o.enabled, "new overlookers start disabled");
        assert_eq!(
            resolve(&db, &o.id).await.unwrap().unwrap().name,
            "status-check"
        );
        assert_eq!(
            resolve(&db, "status-check").await.unwrap().unwrap().id,
            o.id
        );
    }

    #[tokio::test]
    async fn trigger_and_scope_parse_from_json() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let o = create(
            &db,
            &NewOverlooker {
                name: "blocked-watch".to_string(),
                trigger_spec: r#"{"event":"attention","level":"blocked","repo":"/r"}"#.to_string(),
                scope: r#"{"attention":"!ok","repo":"/r"}"#.to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let t = o.trigger();
        assert!(!t.is_scheduled());
        // The legacy `{event,level}` shape folds into the normalized
        // subscription `session.attention=blocked`.
        assert!(t.matches_event("session.attention", Some("blocked"), Some("/r")));
        assert!(!t.matches_event("session.attention", Some("ok"), Some("/r")));
        // The repo filter excludes other repos' events.
        assert!(!t.matches_event("session.attention", Some("blocked"), Some("/other")));

        let s = o.scope();
        assert!(s.admits("blocked", "/r"));
        assert!(!s.admits("ok", "/r")); // !ok excludes ok
        assert!(!s.admits("blocked", "/other")); // repo filter
    }

    #[test]
    fn on_list_subscriptions_match_per_event_and_level() {
        // A manifest `on` list with a bare event and a `name=level` filter, plus
        // a legacy `event`. `matches_event` and the collected `subscriptions()`
        // must agree on every entry — they share one parse path.
        let t: Trigger = serde_json::from_str(
            r#"{"on":["pr.merged","session.exited=error"],"event":"attention"}"#,
        )
        .unwrap();

        // Bare `on` entry: matches regardless of payload level.
        assert!(t.matches_event("pr.merged", None, None));
        assert!(t.matches_event("pr.merged", Some("whatever"), None));
        // `name=level` entry: matches only the named level.
        assert!(t.matches_event("session.exited", Some("error"), None));
        assert!(!t.matches_event("session.exited", Some("ok"), None));
        assert!(!t.matches_event("session.exited", None, None));
        // Legacy `event` folds in, normalized.
        assert!(t.matches_event("session.attention", Some("blocked"), None));
        // An unsubscribed event never matches.
        assert!(!t.matches_event("pr.opened", None, None));

        assert_eq!(
            t.subscriptions(),
            vec![
                ("pr.merged".to_string(), None),
                ("session.exited".to_string(), Some("error".to_string())),
                ("session.attention".to_string(), None),
            ]
        );
    }

    #[tokio::test]
    async fn runs_record_an_audit_trail() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let o = create(
            &db,
            &NewOverlooker {
                name: "auditor".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let run = start_run(&db, &o.id, "manual", "manual").await.unwrap();
        let actions = serde_json::json!([{ "session": "abc", "mark": "attention" }]);
        finish_run(
            &db,
            run,
            &RunRecord {
                outcome: "ok",
                summary: "marked 1 session",
                actions: &actions,
                stdout: "surveyed 3\n",
                stderr: "",
                exit_code: Some(0),
                duration_ms: Some(42),
            },
        )
        .await
        .unwrap();
        let runs = recent_runs(&db, &o.id, 10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].outcome, "ok");
        assert_eq!(runs[0].trigger_reason, "manual");
        assert_eq!(runs[0].trigger_event, "manual");
        assert_eq!(runs[0].stdout, "surveyed 3\n");
        assert_eq!(runs[0].exit_code, Some(0));
        assert!(runs[0].finished_at.is_some());
    }

    #[tokio::test]
    async fn update_overwrites_only_the_set_fields() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let o = create(
            &db,
            &NewOverlooker {
                name: "patchme".to_string(),
                program: "builtin:status".to_string(),
                cooldown_secs: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        update(
            &db,
            &o.id,
            &OverlookerUpdate {
                program: Some("/abs/path.py".to_string()),
                capabilities: Some(vec!["observe".to_string(), "nudge".to_string()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let after = get(&db, &o.id).await.unwrap().unwrap();
        assert_eq!(after.program, "/abs/path.py", "program overwritten");
        assert_eq!(after.cooldown_secs, 10, "untouched field is preserved");
        assert!(after.has_capability("nudge"), "capabilities overwritten");
    }

    #[test]
    fn capabilities_default_to_observe_plus_grants() {
        let o = Overlooker {
            id: "x".into(),
            name: "x".into(),
            enabled: false,
            trigger_spec: "{}".into(),
            scope: "{}".into(),
            program: "builtin:status".into(),
            params: "{}".into(),
            capabilities: r#"["mark"]"#.into(),
            model: String::new(),
            effort: String::new(),
            cooldown_secs: 0,
            last_run_at: None,
            next_run_at: None,
            warm_session_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(o.has_capability("observe"), "observe is implicit");
        assert!(o.has_capability("mark"));
        assert!(
            !o.has_capability("nudge"),
            "ungranted capabilities are denied"
        );
    }
}
