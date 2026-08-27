//! `loom watch` — scheduled and reactive programs, and their runs.
//!
//! `new` scaffolds a local file and never touches the server. `add` builds the
//! `trigger` and `scope` JSON out of flag sugar (`--cron`, `--every`,
//! `--on-event`, `--repo`). `run` names the watch and says whether the round was
//! a dry run, neither of which `WatchRunResult` carries. `enable` and `disable`
//! are two spellings of `watches.update` differing only in the `enabled` they
//! send. `programs` takes `--source <name>`, a lookup that can miss, and a
//! renderer has no way to report a failure. `ls`, `runs` and `get` are declared,
//! rendered by `weaver_api::render::watches`.

use crate::client;
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use weaver_api::operations::watches;
use weaver_api::operations::{NoView, Render};

/// The `loom watch` subcommands that are not one declared operation each.
#[derive(Subcommand)]
pub enum WatchCmd {
    /// Scaffold a starter program file at `~/.weaver/watches/<name>.py`.
    ///
    /// Writes a commented Python template against the program contract (the
    /// fleet over `$WEAVER_API`, round config in `$WEAVER_WATCH`, result
    /// JSON on stdout), then prints the path. Edit it, then register it with
    /// `loom watch add <name> --program <path>`.
    New {
        /// The watch name; also the file stem (`<name>.py`).
        name: String,
    },
    /// List the builtin programs that ship with loom (`watches.programs`).
    Programs {
        /// Print one program's script source instead of the table, e.g.
        /// `--source builtin:archive-merged` — a working example to start from.
        #[arg(long)]
        source: Option<String>,
    },
    /// Register a watch from flags (`watches.create`).
    Add(Box<AddOpts>),
    /// Remove a watch.
    Rm {
        /// Watch id or name.
        name: String,
    },
    /// Enable a watch (arm it).
    Enable {
        /// Watch id or name.
        name: String,
    },
    /// Disable a watch (stop it cold, no redeploy).
    Disable {
        /// Watch id or name.
        name: String,
    },
    /// Fire a round now and print its outcome + summary.
    Run {
        /// Watch id or name.
        name: String,
        /// Simulate: every mutating action is stubbed and logged as "would do
        /// X", nothing is performed. Safe to repeat — the iteration primitive.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Options for `loom watch add` — the flags build the trigger / scope /
/// program / capability set the `watches.create` operation takes.
#[derive(Args)]
pub struct AddOpts {
    /// The watch name (unique).
    name: String,
    /// Cron trigger: a standard 5-field crontab expression (e.g. "0 * * * *").
    #[arg(long, group = "trigger")]
    cron: Option<String>,
    /// Interval trigger sugar: a duration like `30m`, `2h`, `45s`.
    #[arg(long, group = "trigger")]
    every: Option<String>,
    /// Reactive trigger: fire on an event of this kind (e.g. `attention`).
    #[arg(long, group = "trigger")]
    on_event: Option<String>,
    /// With `--on-event`, narrow to a single level (e.g. `blocked`).
    #[arg(long)]
    level: Option<String>,
    /// Pin the watch to one repository (filters the trigger + scope).
    #[arg(long)]
    repo: Option<String>,
    /// Raw scope JSON, merged over the repo filter (e.g. '{"attention":"!ok"}').
    #[arg(long)]
    scope: Option<String>,
    /// The program: `builtin:<name>` (default `builtin:status`) or an absolute
    /// path to a custom program file.
    #[arg(long)]
    program: Option<String>,
    /// The stock-program judgement prompt; stored as `params.prompt`.
    #[arg(long)]
    prompt: Option<String>,
    /// Comma-separated capability set (default `observe,judge,mark` for
    /// `builtin:status`).
    /// Drawn from observe, judge, mark, escalate, nudge, interrupt, launch.
    #[arg(long, value_delimiter = ',')]
    capabilities: Option<Vec<String>>,
    /// Automation-safe ACP profile for agent judgements (default `watch`).
    #[arg(long)]
    profile: Option<String>,
    /// Model override for agent judgement calls.
    #[arg(long)]
    model: Option<String>,
    /// Reasoning effort for judgement calls.
    #[arg(long)]
    effort: Option<String>,
    /// Minimum gap between rounds, in seconds (a non-manual re-fire inside the
    /// gap is skipped).
    #[arg(long)]
    cooldown: Option<i64>,
}

/// Dispatch the `loom watch <verb>` subcommands.
pub async fn run_watch(cmd: WatchCmd) -> Result<()> {
    match cmd {
        WatchCmd::New { name } => cmd_watch_new(name).await,
        WatchCmd::Programs { source } => cmd_watch_programs(source).await,
        WatchCmd::Add(opts) => cmd_watch_add(*opts).await,
        WatchCmd::Rm { name } => cmd_watch_rm(name).await,
        WatchCmd::Enable { name } => cmd_watch_set_enabled(name, true).await,
        WatchCmd::Disable { name } => cmd_watch_set_enabled(name, false).await,
        WatchCmd::Run { name, dry_run } => cmd_watch_run(name, dry_run).await,
    }
}

/// The starter program a `loom watch new` scaffolds: a small, runnable
/// template against the `weaver_loom` API layer and the program contract the
/// engine speaks — the same contract the builtin scripts implement
/// (`loom watch programs --source <name>` prints one as a fuller
/// example). Plain `replace` rather than `format!`, so the template's literal
/// braces (JSON, f-strings) stay readable.
pub(crate) fn scaffold_template(name: &str) -> String {
    const TEMPLATE: &str = r##"# /// script
# requires-python = ">=3.9"
# dependencies = []
# ///
"""__NAME__ — a weaver watch program.

The engine runs this as a subprocess with WEAVER_API (the loom REST base URL)
and WEAVER_WATCH (the round config JSON) set; `weaver_loom` is on
PYTHONPATH. `Round.finish` prints the result the engine reads from stdout.

Register:   loom watch add __NAME__ --program __PATH__ --every 15m
Try it:     loom watch run __NAME__ --dry-run
"""

from weaver_loom import Round


def main():
    rnd = Round()
    for session in rnd.sessions():
        # Decide per session and record findings, e.g.:
        #     rnd.would("mark", session=session["id"], note="one line on why")
        pass
    rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} finding(s)")


if __name__ == "__main__":
    main()
"##;
    TEMPLATE
        .replace("__NAME__", name)
        .replace("__PATH__", &watch_path(name).display().to_string())
}

/// The conventional path for a watch's program file:
/// `~/.weaver/watches/<name>.py`.
pub(crate) fn watch_path(name: &str) -> std::path::PathBuf {
    crate::db::weaver_home()
        .join("watches")
        .join(format!("{name}.py"))
}

/// `loom watch new` — scaffold a starter program file and print its path.
/// Touches no server, so it works before the Python binding exists.
pub(crate) async fn cmd_watch_new(name: String) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("name must not be empty");
    }
    let dir = crate::db::weaver_home().join("watches");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = watch_path(name);
    if path.exists() {
        bail!(
            "{} already exists — edit it, or pick another name",
            path.display()
        );
    }
    std::fs::write(&path, scaffold_template(name))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("scaffolded {}", path.display());
    println!("  edit it, then register:");
    println!(
        "    loom watch add {name} --program {} --cron \"0 * * * *\"",
        path.display()
    );
    Ok(())
}

/// Build the trigger JSON from the `add` flags. clap's `group = "trigger"`
/// already makes cron/every/on-event mutually exclusive; `repo` is folded in
/// when present. An empty trigger (`{}`) is a valid, never-firing default.
pub(crate) fn build_trigger(opts: &AddOpts) -> Value {
    let mut t = serde_json::Map::new();
    if let Some(cron) = &opts.cron {
        t.insert("cron".into(), json!(cron));
    }
    if let Some(every) = &opts.every {
        t.insert("every".into(), json!(every));
    }
    if let Some(event) = &opts.on_event {
        t.insert("event".into(), json!(event));
        if let Some(level) = &opts.level {
            t.insert("level".into(), json!(level));
        }
    }
    if let Some(repo) = &opts.repo {
        t.insert("repo".into(), json!(repo));
    }
    Value::Object(t)
}

/// Build the scope JSON: the explicit `--scope` JSON if given (parsed), with the
/// `--repo` filter folded in so a repo-pinned watch only surveys its repo.
pub(crate) fn build_scope(opts: &AddOpts) -> Result<Value> {
    let mut scope = match &opts.scope {
        Some(raw) => serde_json::from_str::<Value>(raw)
            .with_context(|| format!("--scope is not valid JSON: {raw}"))?,
        None => json!({}),
    };
    if let Some(repo) = &opts.repo {
        if let Some(obj) = scope.as_object_mut() {
            obj.entry("repo").or_insert_with(|| json!(repo));
        }
    }
    Ok(scope)
}

/// `loom watch add` — register a watch (`watches.create`).
pub(crate) async fn cmd_watch_add(opts: AddOpts) -> Result<()> {
    let client = client::default()?;
    let params = opts
        .prompt
        .as_ref()
        .map(|p| json!({ "prompt": p }))
        .unwrap_or_else(|| json!({}));

    let o = client
        .invoke::<watches::create::Op>(&watches::create::Input {
            name: opts.name.clone(),
            trigger: Some(build_trigger(&opts)),
            scope: Some(build_scope(&opts)?),
            params: Some(params),
            program: opts.program.clone(),
            capabilities: opts.capabilities.clone(),
            profile: opts.profile.clone(),
            model: opts.model.clone(),
            effort: opts.effort.clone(),
            cooldown_secs: opts.cooldown,
            // `add` leaves the watch disarmed and prints how to arm it, so it
            // takes the server's default rather than stating one.
            enabled: None,
        })
        .await?;
    // The watch itself is described by `watches.create`'s own renderer, so
    // `add`, `loom watch get` and the MCP tool all read a watch the same way.
    // The arming hint is the CLI's alone: it names a command.
    println!("{}", <watches::create::Op as Render>::text(&o, &NoView));
    println!("  arm it with `loom watch enable {}`", opts.name);
    Ok(())
}

/// `loom watch programs` — the builtin program table, or one program's source.
///
/// The table is `watches.programs`' own renderer. `--source` is the reason this
/// is not a declared command: naming a program that does not exist has
/// to exit non-zero, and a `Render` has no way to say so.
pub(crate) async fn cmd_watch_programs(source: Option<String>) -> Result<()> {
    let client = client::default()?;
    let programs = client
        .invoke::<watches::programs::Op>(&watches::programs::Input {})
        .await?;
    let Some(want) = source else {
        println!("{}", watches::programs::Op::text(&programs, &NoView));
        return Ok(());
    };
    let Some(program) = programs.iter().find(|program| program.program == want) else {
        bail!("no builtin program '{want}' — `loom watch programs` lists them");
    };
    print!("{}", program.source);
    Ok(())
}

pub(crate) async fn cmd_watch_rm(name: String) -> Result<()> {
    let client = client::default()?;
    client
        .invoke::<watches::delete::Op>(&watches::delete::Input { key: name.clone() })
        .await?;
    println!("removed watch {name}");
    Ok(())
}

/// `loom watch enable|disable` — flip the `enabled` toggle
/// (`watches.update`).
pub(crate) async fn cmd_watch_set_enabled(name: String, enabled: bool) -> Result<()> {
    let client = client::default()?;
    let o = client
        .invoke::<watches::update::Op>(&watches::update::Input {
            key: name,
            enabled: Some(enabled),
            ..Default::default()
        })
        .await?;
    println!(
        "{} watch {}",
        if enabled { "enabled" } else { "disabled" },
        o.name
    );
    Ok(())
}

/// `loom watch run` — fire a round now and print outcome + summary.
pub(crate) async fn cmd_watch_run(name: String, dry_run: bool) -> Result<()> {
    let client = client::default()?;
    let res = client
        .invoke::<watches::run::Op>(&watches::run::Input {
            key: name.clone(),
            dry_run,
        })
        .await?;
    let kind = if dry_run { "dry run" } else { "run" };
    println!("{name} {kind}: {}", res.outcome);
    if !res.summary.is_empty() {
        println!("  {}", res.summary);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_add(name: &str) -> AddOpts {
        AddOpts {
            name: name.to_string(),
            cron: None,
            every: None,
            on_event: None,
            level: None,
            repo: None,
            scope: None,
            program: None,
            prompt: None,
            capabilities: None,
            profile: None,
            model: None,
            effort: None,
            cooldown: None,
        }
    }
    /// The scaffolded program carries the pieces an author starts from: the
    /// PEP 723 block (uv-runnable), a docstring documenting the contract, and
    /// the `weaver_loom` round context.
    #[test]
    fn scaffold_template_is_well_formed() {
        let out = scaffold_template("test-watch");
        assert!(out.starts_with("# /// script"), "PEP 723 block leads");
        // The docstring opens with exactly three quotes (a malformed `""` would
        // be the most likely raw-string bug).
        assert!(out.contains("\"\"\"test-watch — "));
        assert!(out.contains("WEAVER_WATCH"));
        assert!(out.contains("from weaver_loom import Round"));
        assert!(out.contains("loom watch add test-watch"));
    }
    /// The scaffold must honor the contract it documents — at minimum, be
    /// valid Python with the placeholders filled in. Skips without `python3`
    /// (the same degradation the engine applies).
    #[test]
    fn scaffold_template_is_valid_python() {
        if !crate::builtins::python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let rendered = scaffold_template("test-watch");
        assert!(rendered.contains("test-watch"), "the name is filled in");
        assert!(!rendered.contains("__NAME__"), "no placeholder survives");
        assert!(!rendered.contains("__PATH__"), "no placeholder survives");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-watch.py");
        std::fs::write(&path, rendered).unwrap();
        let out = std::process::Command::new("python3")
            .args(["-m", "py_compile"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the scaffold does not compile: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    /// `loom watch new` writes the file under `~/.weaver/watches/`,
    /// creating the dir, and refuses to clobber an existing one.
    #[tokio::test]
    #[serial_test::serial]
    async fn watch_new_scaffolds_under_weaver_home() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("WEAVER_HOME", home.path());
        cmd_watch_new("scaffolded".to_string()).await.unwrap();
        let path = home.path().join("watches").join("scaffolded.py");
        assert!(path.exists(), "the program file was written");
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("\"\"\"scaffolded — "));
        // A second `new` of the same name refuses rather than clobbering.
        assert!(cmd_watch_new("scaffolded".to_string()).await.is_err());
        std::env::remove_var("WEAVER_HOME");
    }
    #[test]
    fn build_trigger_maps_each_flag() {
        let cron = build_trigger(&AddOpts {
            cron: Some("0 * * * *".into()),
            ..empty_add("a")
        });
        assert_eq!(cron, json!({ "cron": "0 * * * *" }));

        let every = build_trigger(&AddOpts {
            every: Some("30m".into()),
            repo: Some("/r".into()),
            ..empty_add("a")
        });
        assert_eq!(every, json!({ "every": "30m", "repo": "/r" }));

        let event = build_trigger(&AddOpts {
            on_event: Some("attention".into()),
            level: Some("blocked".into()),
            ..empty_add("a")
        });
        assert_eq!(event, json!({ "event": "attention", "level": "blocked" }));
    }
    #[test]
    fn build_scope_folds_in_the_repo_filter() {
        // `--repo` alone becomes a repo-scoped query.
        let s = build_scope(&AddOpts {
            repo: Some("/r".into()),
            ..empty_add("a")
        })
        .unwrap();
        assert_eq!(s, json!({ "repo": "/r" }));

        // An explicit `--scope` is merged with the repo filter, not clobbered.
        let s = build_scope(&AddOpts {
            scope: Some(r#"{"attention":"!ok"}"#.into()),
            repo: Some("/r".into()),
            ..empty_add("a")
        })
        .unwrap();
        assert_eq!(s, json!({ "attention": "!ok", "repo": "/r" }));

        // Bad scope JSON is an error.
        assert!(build_scope(&AddOpts {
            scope: Some("not json".into()),
            ..empty_add("a")
        })
        .is_err());
    }
}
