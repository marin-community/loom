//! Text rendering for work-item operations.

use crate::dto::{IssueActionsResult, IssueView};
use crate::operations::issues;
use crate::operations::{NoView, Render};

fn row(issue: &IssueView) -> String {
    let claim = issue.claimed_branch.as_deref().unwrap_or("(backlog)");
    format!("#{:<5} {:<9} {:<24} {}", issue.id, issue.status, claim, issue.title)
}

fn detail(issue: &IssueView) -> String {
    let mut lines = vec![
        format!("#{} {}", issue.id, issue.title),
        format!("  status:  {}", issue.status),
        format!(
            "  claimed: {}",
            issue.claimed_branch.as_deref().unwrap_or("(backlog)")
        ),
    ];
    if !issue.body.trim().is_empty() {
        lines.push(String::new());
        lines.push(issue.body.trim().to_string());
    }
    lines.join("\n")
}

fn applied(result: &IssueActionsResult, verb: &str) -> String {
    let mut parts = Vec::new();
    if !result.issues.is_empty() {
        let ids: Vec<_> = result.issues.iter().map(|i| format!("#{}", i.id)).collect();
        parts.push(format!("{verb} {}", ids.join(" ")));
    }
    if !result.deleted_ids.is_empty() {
        let ids: Vec<_> = result.deleted_ids.iter().map(|id| format!("#{id}")).collect();
        parts.push(format!("deleted {}", ids.join(" ")));
    }
    if parts.is_empty() {
        return "no work items changed".to_string();
    }
    parts.join("; ")
}

impl Render for issues::list::List {
    fn text(output: &Vec<IssueView>, view: &issues::list::View) -> String {
        let mut items: Vec<&IssueView> = output.iter().collect();
        if view.mine {
            items.retain(|issue| issue.claimed_branch.is_some());
        }
        if items.is_empty() {
            return "no work items".to_string();
        }
        items
            .iter()
            .map(|issue| row(issue))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Render for issues::get::Get {
    fn text(output: &IssueView, _: &NoView) -> String {
        detail(output)
    }
}

impl Render for issues::create::Create {
    fn text(output: &IssueView, _: &NoView) -> String {
        format!("created work item #{}", output.id)
    }
}

impl Render for issues::backlog::create::Create {
    fn text(output: &IssueView, _: &NoView) -> String {
        format!("created backlog item #{}", output.id)
    }
}

impl Render for issues::close::Close {
    fn text(output: &IssueActionsResult, _: &NoView) -> String {
        applied(output, "closed")
    }
}

impl Render for issues::reopen::Reopen {
    fn text(output: &IssueActionsResult, _: &NoView) -> String {
        applied(output, "reopened")
    }
}

impl Render for issues::delete::Delete {
    fn text(output: &IssueActionsResult, _: &NoView) -> String {
        applied(output, "deleted")
    }
}

impl Render for issues::tags::set::Set {
    fn text(output: &IssueView, _: &NoView) -> String {
        format!("tagged work item #{}", output.id)
    }
}

impl Render for issues::tags::delete::Delete {
    fn text(output: &IssueView, _: &NoView) -> String {
        format!("removed tag from work item #{}", output.id)
    }
}

impl Render for issues::actions::Actions {
    fn text(output: &IssueActionsResult, _: &NoView) -> String {
        applied(output, "updated")
    }
}
