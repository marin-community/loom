//! Text rendering for watches — the periodic / triggered programs over the fleet.

use serde_json::Value;

use crate::dto::{ProgramView, WatchRunView, WatchView};
use crate::operations::watches;
use crate::operations::{NoView, Render};
use crate::render::truncate;

/// A compact human summary of a `WatchView`'s parsed `trigger` object.
fn trigger_summary(trigger: &Value) -> String {
    if let Some(cron) = trigger.get("cron").and_then(Value::as_str) {
        return format!("cron {cron}");
    }
    if let Some(every) = trigger.get("every").and_then(Value::as_str) {
        return format!("every {every}");
    }
    // `on` is how every reactive watch's trigger is stored — a list of event
    // names, each optionally `name=level`.
    if let Some(events) = trigger.get("on").and_then(Value::as_array) {
        let names: Vec<&str> = events.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() {
            return format!("on {}", names.join(","));
        }
    }
    if let Some(event) = trigger.get("event").and_then(Value::as_str) {
        return match trigger.get("level").and_then(Value::as_str) {
            Some(level) => format!("on {event}={level}"),
            None => format!("on {event}"),
        };
    }
    "—".to_string()
}

/// The granted capability set, comma-joined. `observe` is implicit, so an empty
/// grant list still reads as that baseline.
fn capabilities_summary(capabilities: &[String]) -> String {
    if capabilities.is_empty() {
        return "observe".to_string();
    }
    capabilities.join(",")
}

/// One line of a round's action log. A mutating action carries `action`; a
/// dry-run stub carries `would`.
fn action_summary(action: &Value) -> String {
    let verb = action
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            action
                .get("would")
                .and_then(Value::as_str)
                .map(|would| format!("would {would}"))
        })
        .unwrap_or_else(|| "?".to_string());
    let session = action.get("session").and_then(Value::as_str).unwrap_or("");
    let detail = action
        .get("level")
        .and_then(Value::as_str)
        .map(|level| {
            let note = action.get("note").and_then(Value::as_str).unwrap_or("");
            if note.is_empty() {
                level.to_string()
            } else {
                format!("{level} — {note}")
            }
        })
        .or_else(|| {
            action
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    if detail.is_empty() {
        format!("{verb} {session}")
    } else {
        format!("{verb} {session}: {detail}")
    }
}

fn row(watch: &WatchView) -> String {
    format!(
        "{:<18}  {:<8}  {:<22}  {:<18}  {}",
        truncate(&watch.name, 18),
        if watch.enabled { "yes" } else { "no" },
        truncate(&trigger_summary(&watch.trigger), 22),
        truncate(&watch.program, 18),
        watch.last_outcome.as_deref().unwrap_or("—"),
    )
}

/// What a watch is, as its own fields say it: what wakes it, what it runs, and
/// how far up the intervention ladder it may go.
pub fn detail(watch: &WatchView) -> String {
    let mut lines = vec![
        format!("  trigger: {}", trigger_summary(&watch.trigger)),
        format!("  program: {}", watch.program),
        format!("  caps:    {}", capabilities_summary(&watch.capabilities)),
        format!("  profile: {}", watch.profile),
        format!("  enabled: {}", if watch.enabled { "yes" } else { "no" }),
    ];
    if let Some(last) = &watch.last_run_at {
        lines.push(format!("  last:    {last}"));
    }
    if let Some(next) = &watch.next_run_at {
        lines.push(format!("  next:    {next}"));
    }
    lines.join("\n")
}

impl Render for watches::list::Op {
    fn text(output: &Vec<WatchView>, _: &NoView) -> String {
        if output.is_empty() {
            return "no watches — scaffold one with `loom watch new <name>`".to_string();
        }
        let header = format!(
            "{:<18}  {:<8}  {:<22}  {:<18}  LAST",
            "NAME", "ENABLED", "TRIGGER", "PROGRAM"
        );
        std::iter::once(header)
            .chain(output.iter().map(row))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for watches::get::Op {
    fn text(output: &WatchView, _: &NoView) -> String {
        format!("{}  ({})\n{}", output.name, output.id, detail(output))
    }
}

impl Render for watches::create::Op {
    fn text(output: &WatchView, _: &NoView) -> String {
        format!(
            "registered watch {}  ({})\n{}",
            output.name,
            output.id,
            detail(output)
        )
    }
}

impl Render for watches::programs::Op {
    fn text(output: &Vec<ProgramView>, _: &NoView) -> String {
        let header = format!("{:<26}  TITLE", "PROGRAM");
        std::iter::once(header)
            .chain(
                output
                    .iter()
                    .map(|program| format!("{:<26}  {}", program.program, program.title)),
            )
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for watches::runs::Op {
    /// The round history. `--verbose` prints each round on its own lines with
    /// the actions it took, rather than one row each.
    fn text(output: &Vec<WatchRunView>, view: &watches::runs::View) -> String {
        if output.is_empty() {
            return "no rounds yet — fire one with `loom watch run <name>`".to_string();
        }
        if view.verbose {
            let mut lines = Vec::new();
            for run in output {
                lines.push(format!(
                    "{}  [{}]  {}",
                    run.started_at, run.trigger_reason, run.outcome
                ));
                if !run.summary.is_empty() {
                    lines.push(format!("  {}", run.summary));
                }
                if let Some(actions) = run.actions.as_array() {
                    for action in actions {
                        lines.push(format!("    - {}", action_summary(action)));
                    }
                }
            }
            return lines.join("\n");
        }
        let header = format!(
            "{:<24}  {:<14}  {:<8}  SUMMARY",
            "WHEN", "REASON", "OUTCOME"
        );
        std::iter::once(header)
            .chain(output.iter().map(|run| {
                format!(
                    "{:<24}  {:<14}  {:<8}  {}",
                    run.started_at,
                    truncate(&run.trigger_reason, 14),
                    run.outcome,
                    truncate(&run.summary, 60),
                )
            }))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trigger_summary_reads_each_shape() {
        let cron = json!({ "cron": "0 * * * *" });
        assert_eq!(trigger_summary(&cron), "cron 0 * * * *");
        let every = json!({ "every": "30m" });
        assert_eq!(trigger_summary(&every), "every 30m");
        let event = json!({ "event": "attention", "level": "blocked" });
        assert_eq!(trigger_summary(&event), "on attention=blocked");
        let on = json!({ "on": ["pr.merged", "pr.opened"] });
        assert_eq!(trigger_summary(&on), "on pr.merged,pr.opened");
        let on_empty = json!({ "on": [] });
        assert_eq!(trigger_summary(&on_empty), "—");
        let empty = json!({});
        assert_eq!(trigger_summary(&empty), "—");
    }
    #[test]
    fn action_summary_renders_marks_nudges_and_would_dos() {
        let mark =
            json!({ "action": "mark", "session": "s1", "level": "blocked", "note": "stuck" });
        assert_eq!(action_summary(&mark), "mark s1: blocked — stuck");
        let would = json!({ "would": "mark", "session": "s1", "level": "ok" });
        assert_eq!(action_summary(&would), "would mark s1: ok");
        let nudge = json!({ "action": "nudge", "session": "s1", "text": "try again" });
        assert_eq!(action_summary(&nudge), "nudge s1: try again");
    }

    /// `observe` is the implicit baseline, not an empty grant.
    #[test]
    fn capabilities_summary_names_the_implicit_baseline() {
        assert_eq!(capabilities_summary(&[]), "observe");
        assert_eq!(
            capabilities_summary(&["judge".to_string(), "mark".to_string()]),
            "judge,mark"
        );
    }
}
