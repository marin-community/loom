//! Layered key/value settings.
//!
//! Runtime overrides live in `settings`, infrastructure-provided defaults live
//! in `deployment_settings`, and reads resolve in that order before the
//! built-in default. Every setting weaver knows about is declared in
//! [`REGISTRY`] — a single source of truth that gives each key a label, help
//! text, type, default, and group. The registry drives validation
//! ([`validate`]) and the settings pane in the web UI ([`describe`]); the raw
//! [`get`]/[`apply`] helpers still accept arbitrary runtime keys so nothing
//! breaks if a setting is read before it is registered.

use anyhow::Result;
use serde::Serialize;
use sqlx::Row;

use crate::db::{now_iso, Db};

pub const DEFAULT_AGENT: &str = "claude";
pub const DEFAULT_AGENT_MODEL: &str = "";
pub const DEFAULT_AGENT_EFFORT: &str = "";
/// Provider-neutral permission posture for new ACP sessions. Claude uses this
/// value directly; Codex maps it onto its own mode vocabulary at launch.
pub const DEFAULT_AGENT_MODE: &str = "auto";
/// Whether the server adopts orphaned sessions on startup. Off by default:
/// the operator opts in via `loom config set server.auto_adopt true`.
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
/// Named launch profile selected by GitHub-triggered sessions.
pub const DEFAULT_GITHUB_PROFILE: &str = "default";
/// Reasoning effort for Slack-origin sessions. Slack conversations usually
/// expect a prompt answer, so they use a cheaper/faster tier than long-form
/// workspace sessions by default.
pub const DEFAULT_SLACK_EFFORT: &str = "medium";
/// Named launch profile selected by Slack-triggered sessions.
pub const DEFAULT_SLACK_PROFILE: &str = "default";
/// Sentinel that leaves a Slack-origin session's profile effort unchanged.
pub const SLACK_AGENT_DEFAULT_EFFORT: &str = "agent-default";
/// Whether Slack status cards include this session's progress trail.
pub const DEFAULT_SLACK_STATUS_UPDATES: bool = true;
/// Whether Slack status cards link session artifacts.
pub const DEFAULT_SLACK_STATUS_ARTIFACTS: bool = false;
/// Slack mrkdwn template for a status card's first line.
pub const DEFAULT_SLACK_STATUS_HEADER_TEMPLATE: &str = "On it — <{session_url}>";
/// Bound organization prompt additions so a mistaken setting cannot dominate
/// every Slack launch payload.
pub const MAX_SLACK_PROMPT_INSTRUCTIONS_BYTES: usize = 16 * 1024;
/// Slack messages are bounded; reserve most of that budget for status content.
pub const MAX_SLACK_STATUS_HEADER_TEMPLATE_BYTES: usize = 1024;
/// The palette the browser terminal (xterm.js) renders with. `dark` keeps the
/// classic black background; `light` swaps in a light, readable palette.
pub const DEFAULT_TERMINAL_THEME: &str = "dark";
/// The typeface the browser terminal renders with. A token, not a raw font
/// stack: the frontend maps it to a concrete `font-family` (`plex` → the
/// bundled IBM Plex Mono, `jetbrains` → the bundled JetBrains Mono, `system` →
/// the platform monospace stack). Keeping it a token keeps the stored value
/// stable and the CSS the frontend's concern.
pub const DEFAULT_TERMINAL_FONT: &str = "plex";
/// Pixel size the browser terminal renders at (xterm's `fontSize`, in CSS px).
/// The frontend clamps the applied value to a legible range (8–24) so a stray
/// edit can't make the terminal unusable.
pub const DEFAULT_TERMINAL_FONT_SIZE: i64 = 13;
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
/// Memory ceiling (GiB) applied to each terminal session via a per-session
/// cgroup, where the runtime provides a delegated subtree (see
/// `backend::new_session` in the `loom` crate). One runaway agent process then
/// OOMs alone instead of taking the whole host down. 0 disables the limit.
pub const DEFAULT_SESSION_MEMORY_MAX_GB: i64 = 8;

// ---------------------------------------------------------------------------
// Setting registry
// ---------------------------------------------------------------------------

/// The value type of a registered setting. Drives both validation and the
/// input control rendered in the settings pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    /// One-line free-form text (commands, names, …).
    String,
    /// Multi-line free-form text (prompts, templates, …).
    Text,
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
            live session's pull request — its link, review decision, and \
            check rollup — every minute while active and less often as the \
            session grows quiet. A no-op for \
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
        description: "The phrase that tags loom into an issue or PR comment and \
            launches a session against that repo (default `@loom`). Matched \
            case-insensitively anywhere in the comment, as a standalone mention: \
            quoted lines and code are ignored, and `@loom-bot` is a different \
            name. The webhook is only active once `LOOM_GITHUB_WEBHOOK_SECRET` \
            is configured.",
        kind: SettingKind::String,
        default: DEFAULT_GITHUB_TRIGGER_PHRASE,
        group: "GitHub",
        options: &[],
    },
    SettingSpec {
        key: "github.profile",
        label: "GitHub session profile",
        description: "Named launch profile for sessions created by the GitHub \
            trigger. Profile instructions, runtime, tools, and policy are \
            applied together. The profile must exist when a trigger launches.",
        kind: SettingKind::String,
        default: DEFAULT_GITHUB_PROFILE,
        group: "GitHub",
        options: &[],
    },
    SettingSpec {
        key: "slack.enabled",
        label: "Slack integration",
        description: "Master switch for the Slack Socket Mode integration. loom \
            only connects when both `LOOM_SLACK_APP_TOKEN` and \
            `LOOM_SLACK_BOT_TOKEN` are configured; turning this OFF closes any \
            live connection without removing the tokens. With it on and the \
            tokens present, `/marinbot` (and `@marinbot`) launch sessions and \
            mirror their status back to the Slack thread.",
        kind: SettingKind::Bool,
        default: "true",
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.allowed_users",
        label: "Restrict to Slack users",
        description: "Leave empty (the default) to let anyone in the installed \
            workspace launch a session from a conversation the bot has been \
            invited to — Slack's own workspace membership and channel invite are \
            the boundary. Fill in space- or comma-separated Slack user IDs (e.g. \
            `U0123ABCD`) to narrow it to those people. Three rules hold either \
            way: another workspace, including an externally-shared Slack Connect \
            channel, is always rejected; loom never triggers on its own posts; \
            and a message posted by another app counts only when its ID is \
            listed here.",
        kind: SettingKind::String,
        default: "",
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.default_repo",
        label: "Slack default repository",
        description: "The managed repository (`owner/name`) a `/marinbot` \
            launch targets when the command text carries no `owner/name:` \
            prefix. Slack conversations have no repo of their own, so without \
            this — or an explicit prefix — a trigger has nothing to work on and \
            replies asking for one.",
        kind: SettingKind::String,
        default: "",
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.profile",
        label: "Slack session profile",
        description: "Named launch profile for sessions created from Slack. \
            Profile instructions, runtime, tools, and policy are applied \
            together. The profile must exist when a trigger launches.",
        kind: SettingKind::String,
        default: DEFAULT_SLACK_PROFILE,
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.effort",
        label: "Slack reasoning effort",
        description: "Reasoning effort for Slack-origin sessions. Slack \
            conversations generally favor a faster direct answer, so the \
            default is `medium` rather than a deeper tier. Choose \
            `agent-default` to inherit the selected agent profile unchanged. \
            Profiles that lock or do not support this setting also keep their \
            configured effort.",
        kind: SettingKind::Enum,
        default: DEFAULT_SLACK_EFFORT,
        group: "Slack",
        options: &[
            SLACK_AGENT_DEFAULT_EFFORT,
            "low",
            "medium",
            "high",
            "xhigh",
            "max",
        ],
    },
    SettingSpec {
        key: "slack.status_updates",
        label: "Show Slack progress updates",
        description: "Include this session's `loom status` reports in the \
            editable Slack status card. Reports from an earlier session on the \
            same conversation are never repeated.",
        kind: SettingKind::Bool,
        default: "true",
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.status_artifacts",
        label: "Link Slack artifacts",
        description: "Include links to the session's published artifacts in \
            the Slack status card. Off by default: the thread should receive a \
            self-contained answer, and internal design documents are rarely \
            useful conversation links.",
        kind: SettingKind::Bool,
        default: "false",
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.status_header_template",
        label: "Slack status header",
        description: "Header template for the editable Slack status card. \
            `{session_url}` expands to the public session URL; Slack mrkdwn is \
            accepted. Keep the placeholder when readers should be able to open \
            the session.",
        kind: SettingKind::String,
        default: DEFAULT_SLACK_STATUS_HEADER_TEMPLATE,
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.prompt_instructions",
        label: "Legacy Slack instructions",
        description: "Compatibility overlay appended only to Slack launch \
            prompts. New deployments should put organization workflow and \
            response conventions on the profile selected by `slack.profile`, \
            which also works for other launch origins. Never put secrets here.",
        kind: SettingKind::Text,
        default: "",
        group: "Slack",
        options: &[],
    },
    SettingSpec {
        key: "slack.idle_archive_secs",
        label: "Slack idle archive (seconds)",
        description: "Archive a Slack-origin session after this many seconds \
            without session activity. The agent, worktree, and terminal are \
            removed while the branch, conversation, and history remain \
            recoverable. A live ACP turn is never interrupted. Set 0 to \
            disable; an individual session can opt out with auto-archive.",
        kind: SettingKind::Int,
        default: "86400",
        group: "Slack",
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
        key: "terminal.font",
        label: "Terminal font",
        description: "Typeface for the in-browser terminal. `plex` is the \
            bundled IBM Plex Mono; `jetbrains` is the bundled JetBrains Mono; \
            `system` uses the platform's own monospace font. Takes effect the \
            next time a terminal is opened.",
        kind: SettingKind::Enum,
        default: DEFAULT_TERMINAL_FONT,
        group: "Appearance",
        options: &["plex", "jetbrains", "system"],
    },
    SettingSpec {
        key: "terminal.font_size",
        label: "Terminal font size",
        description: "Pixel size for the in-browser terminal (CSS px). Clamped \
            to a legible 8–24 range when applied. Takes effect the next time a \
            terminal is opened.",
        kind: SettingKind::Int,
        default: "13",
        group: "Appearance",
        options: &[],
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
        key: "automation.turn_cap",
        label: "Turn cap",
        description: "Cap on agent turns for an automation-class session (one \
            turn per `working` edge). Past the cap the session's branch is \
            marked `blocked` and no new ACP turn is started — an in-flight turn \
            is never interrupted. Warm (watch-managed) sessions are exempt. \
            0 disables the cap.",
        kind: SettingKind::Int,
        default: "100",
        group: "Automation",
        options: &[],
    },
    SettingSpec {
        key: "automation.idle_archive_secs",
        label: "Idle archive (seconds)",
        description: "Idle TTL for an automation-class session: once it has \
            gone this long without any activity (and has no live turn), the \
            retention reaper archives it — tearing down the terminal and \
            worktree while keeping the branch and its history. Warm \
            (watch-managed) sessions are exempt. 0 disables the TTL; a closed \
            tracking issue still archives the session.",
        kind: SettingKind::Int,
        default: "28800",
        group: "Automation",
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
        key: "session.memory_max_gb",
        label: "Session memory limit (GiB)",
        description: "Memory ceiling for each terminal session — the agent and \
            everything it spawns — enforced through a per-session cgroup. When a \
            session crosses it, the kernel OOM-kills the biggest process inside \
            that session only; the host and the other sessions are untouched. \
            Applies where loom runs with a delegated cgroup subtree (the \
            standalone Docker deploy prepares one at boot); elsewhere sessions \
            run unlimited. 0 disables the limit. Takes effect for sessions \
            launched after the change.",
        kind: SettingKind::Int,
        default: "8",
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
    SettingSpec {
        key: "metadata.title_generation",
        label: "Generate task labels",
        description: "Asynchronously replace eligible deterministic task labels through a \
            bounded economy prompt on the session's ACP runtime. Launch never waits for this.",
        kind: SettingKind::Bool,
        default: "true",
        group: "Metadata",
        options: &[],
    },
    SettingSpec {
        key: "metadata.resumption_cues",
        label: "Generate resumption cues",
        description: "Allow explicit or inactivity-based on-return cues in Conversation. \
            Cues are cached by their conversation and artifact cursor.",
        kind: SettingKind::Bool,
        default: "true",
        group: "Metadata",
        options: &[],
    },
    SettingSpec {
        key: "metadata.resumption_inactivity_secs",
        label: "Cue inactivity (seconds)",
        description: "Minimum session inactivity before an on-return cue is due. Explicit \
            Generate requests do not wait for this threshold.",
        kind: SettingKind::Int,
        default: "3600",
        group: "Metadata",
        options: &[],
    },
    SettingSpec {
        key: "metadata.allow_restricted",
        label: "Allow restricted metadata",
        description: "Explicitly permit bounded title/cue excerpts from restricted sessions \
            to reach the session's isolated metadata prompt. Off by default.",
        kind: SettingKind::Bool,
        default: "false",
        group: "Metadata",
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

/// Cap on agent turns for an automation-class session (one turn per `working`
/// edge). Past the cap the branch is marked `blocked` and no new ACP turn is
/// started. 0 disables the cap.
pub const DEFAULT_AUTOMATION_TURN_CAP: i64 = 100;

/// Idle TTL after which the retention reaper archives an automation-class
/// session (8 hours). 0 disables the TTL trigger; a closed tracking issue
/// still archives the session.
pub const DEFAULT_AUTOMATION_IDLE_ARCHIVE_SECS: i64 = 28800;

/// Idle TTL after which the retention reaper archives an ordinary interactive
/// session (10 days). A profile may explicitly set 0 to disable the TTL.
pub const DEFAULT_INTERACTIVE_IDLE_ARCHIVE_SECS: i64 = 864000;

/// Idle TTL after which the retention reaper archives a Slack-origin session
/// (24 hours). 0 disables the TTL trigger.
pub const DEFAULT_SLACK_IDLE_ARCHIVE_SECS: i64 = 86400;

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
    match key {
        "slack.prompt_instructions" if value.len() > MAX_SLACK_PROMPT_INSTRUCTIONS_BYTES => {
            return Err(format!(
                "must be at most {MAX_SLACK_PROMPT_INSTRUCTIONS_BYTES} bytes"
            ));
        }
        "slack.status_header_template" if value.trim().is_empty() => {
            return Err("must not be empty".to_string());
        }
        "slack.status_header_template" if value.len() > MAX_SLACK_STATUS_HEADER_TEMPLATE_BYTES => {
            return Err(format!(
                "must be at most {MAX_SLACK_STATUS_HEADER_TEMPLATE_BYTES} bytes"
            ));
        }
        _ => {}
    }
    match spec.kind {
        SettingKind::String | SettingKind::Text => Ok(()),
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

/// Which configuration layer supplies a setting's effective value.
///
/// Runtime overrides are ordinary rows in `settings`; deployment defaults are
/// reconciled separately by infrastructure tooling; `default` is the immutable
/// value compiled into [`REGISTRY`]. Keeping the two mutable layers separate
/// lets an operator experiment live and later reset cleanly to the deployment's
/// declared policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    Default,
    Deployment,
    Runtime,
}

/// A registered setting paired with its current effective value — what the
/// settings pane renders.
#[derive(Debug, Clone, Serialize)]
pub struct SettingView {
    #[serde(flatten)]
    pub spec: &'static SettingSpec,
    /// Effective value, resolved runtime → deployment → built-in default.
    pub value: String,
    /// The layer supplying [`Self::value`].
    pub source: SettingSource,
    /// The deployment-provided fallback, if one is declared. The Settings UI
    /// uses this to explain what clearing a runtime override will reveal.
    pub deployment_value: Option<String>,
    /// Backward-compatible shorthand for `source == default`.
    pub is_default: bool,
}

/// The full registry with each setting's current effective value, ordered as
/// declared in [`REGISTRY`].
pub async fn describe(db: &Db) -> Result<Vec<SettingView>> {
    let runtime: std::collections::HashMap<String, String> = list(db).await?.into_iter().collect();
    let deployment: std::collections::HashMap<String, String> =
        list_deployment(db).await?.into_iter().collect();
    Ok(REGISTRY
        .iter()
        .map(|spec| {
            let deployment_value = deployment.get(spec.key).cloned();
            let (value, source) = if let Some(value) = runtime.get(spec.key) {
                (value.clone(), SettingSource::Runtime)
            } else if let Some(value) = &deployment_value {
                (value.clone(), SettingSource::Deployment)
            } else {
                (spec.default.to_string(), SettingSource::Default)
            };
            SettingView {
                spec,
                value,
                source,
                deployment_value,
                is_default: source == SettingSource::Default,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Raw key/value access
// ---------------------------------------------------------------------------

pub async fn get(db: &Db, key: &str) -> Option<String> {
    let runtime = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|r| r.get::<String, _>("value"));
    if runtime.is_some() {
        tracing::debug!(key, source = "runtime", "config get");
        return runtime;
    }
    let deployment = sqlx::query("SELECT value FROM deployment_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|r| r.get::<String, _>("value"));
    tracing::debug!(
        key,
        source = deployment
            .as_ref()
            .map_or("built-in default", |_| "deployment"),
        "config get"
    );
    deployment
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

/// One requested runtime change: `Some(value)` writes a key, `None` clears it
/// (so the key falls back to its deployment value, then its registered default).
pub type Change = (String, Option<String>);

/// Apply a batch of [`Change`]s atomically — either all writes land or none do.
/// Callers are expected to [`validate`] each value first; `apply` itself only
/// touches the database.
pub async fn apply(db: &Db, changes: &[Change]) -> Result<()> {
    let mut tx = crate::db::begin_immediate(db).await?;
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

/// Deployment-provided setting defaults, kept separate from live runtime
/// overrides so a Settings-pane reset has deterministic precedence semantics.
pub async fn list_deployment(db: &Db) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query("SELECT key, value FROM deployment_settings ORDER BY key")
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("key"), r.get::<String, _>("value")))
        .collect())
}

/// Reconcile the deployment-default layer atomically.
///
/// Values must already have passed [`validate`]. With `prune`, omitted
/// deployment keys are removed; runtime rows are never touched.
pub async fn reconcile_deployment(db: &Db, values: &[(String, String)], prune: bool) -> Result<()> {
    let mut tx = crate::db::begin_immediate(db).await?;
    let now = now_iso();
    for (key, value) in values {
        sqlx::query(
            "INSERT INTO deployment_settings (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE
               SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    if prune {
        let declared: std::collections::HashSet<&str> =
            values.iter().map(|(key, _)| key.as_str()).collect();
        let existing: Vec<String> =
            sqlx::query_scalar("SELECT key FROM deployment_settings ORDER BY key")
                .fetch_all(&mut *tx)
                .await?;
        for key in existing {
            if !declared.contains(key.as_str()) {
                sqlx::query("DELETE FROM deployment_settings WHERE key = ?")
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
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
    fn validate_bounds_slack_prompt_and_header_text() {
        assert!(validate("slack.status_header_template", " ").is_err());
        assert!(validate(
            "slack.status_header_template",
            &"x".repeat(MAX_SLACK_STATUS_HEADER_TEMPLATE_BYTES + 1)
        )
        .is_err());
        assert!(validate(
            "slack.prompt_instructions",
            &"x".repeat(MAX_SLACK_PROMPT_INSTRUCTIONS_BYTES + 1)
        )
        .is_err());
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
        assert_eq!(json["source"], "default");
        assert_eq!(json["deployment_value"], serde_json::Value::Null);
        assert_eq!(json["is_default"], true);

        assert!(views
            .iter()
            .all(|view| !view.spec.key.starts_with("agent.")));
    }

    #[test]
    fn terminal_appearance_settings_validate() {
        // Font is an enum: only the three declared tokens pass.
        assert!(validate("terminal.font", "plex").is_ok());
        assert!(validate("terminal.font", "jetbrains").is_ok());
        assert!(validate("terminal.font", "system").is_ok());
        assert!(validate("terminal.font", "comic-sans").is_err());
        // Font size is an int: numbers pass, prose does not. (Range clamping is
        // the frontend's job — the registry only guards the kind.)
        assert!(validate("terminal.font_size", "14").is_ok());
        assert!(validate("terminal.font_size", "large").is_err());
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
        assert_eq!(auto_adopt.source, SettingSource::Default);
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
        assert_eq!(auto_adopt.source, SettingSource::Runtime);
        assert_eq!(auto_adopt.value, "true");
    }

    #[tokio::test]
    async fn runtime_overrides_deployment_and_reset_reveals_it() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let key = "slack.status_updates";

        reconcile_deployment(&db, &[(key.into(), "false".into())], false)
            .await
            .unwrap();
        assert_eq!(get(&db, key).await.as_deref(), Some("false"));
        let deployed = describe(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|view| view.spec.key == key)
            .unwrap();
        assert_eq!(deployed.source, SettingSource::Deployment);
        assert_eq!(deployed.deployment_value.as_deref(), Some("false"));
        assert!(!deployed.is_default);

        apply(&db, &[(key.into(), Some("true".into()))])
            .await
            .unwrap();
        let runtime = describe(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|view| view.spec.key == key)
            .unwrap();
        assert_eq!(runtime.value, "true");
        assert_eq!(runtime.source, SettingSource::Runtime);
        assert_eq!(runtime.deployment_value.as_deref(), Some("false"));

        apply(&db, &[(key.into(), None)]).await.unwrap();
        assert_eq!(get(&db, key).await.as_deref(), Some("false"));

        reconcile_deployment(&db, &[], true).await.unwrap();
        assert_eq!(get(&db, key).await, None);
        let reset = describe(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|view| view.spec.key == key)
            .unwrap();
        assert_eq!(reset.value, "true");
        assert_eq!(reset.source, SettingSource::Default);
        assert_eq!(reset.deployment_value, None);
        assert!(reset.is_default);
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
