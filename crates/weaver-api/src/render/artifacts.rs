//! Text rendering for versioned deliverables.

use super::truncate;
use crate::dto::{ArtifactMeta, ArtifactVersion, ArtifactView, ThreadDto};
use crate::operations::artifacts;
use crate::operations::{NoView, Render};

/// A thread's anchor is a span of the artifact, which can be a paragraph; one
/// line of it is enough to recognise which passage is under discussion.
const QUOTE_WIDTH: usize = 70;

/// Where an artifact lives, as the listing prefixes it: the owning branch id
/// for a branch-scoped copy, `repo:` for the shared one. A branch sees both at
/// once, so the scope has to be legible without a second lookup.
fn scope(branch_id: Option<&str>) -> String {
    match branch_id {
        Some(branch) => format!("{branch}/"),
        None => "repo:".to_string(),
    }
}

fn row(artifact: &ArtifactMeta) -> String {
    let title = if artifact.title.is_empty() {
        String::new()
    } else {
        format!("  {}", artifact.title)
    };
    format!(
        "{}{:<24} [rev {}] {}{title}",
        scope(artifact.branch_id.as_deref()),
        artifact.name,
        artifact.rev,
        artifact.kind
    )
}

/// The envelope: everything about the artifact except the bytes themselves.
fn envelope(artifact: &ArtifactMeta) -> String {
    let mut lines = vec![
        format!("id:      {}", artifact.id),
        format!("name:    {}", artifact.name),
        format!("kind:    {}", artifact.kind),
    ];
    if !artifact.title.is_empty() {
        lines.push(format!("title:   {}", artifact.title));
    }
    lines.push(format!(
        "scope:   {}",
        match &artifact.branch_id {
            Some(branch) => format!("branch {branch}"),
            None => "repo-shared".to_string(),
        }
    ));
    lines.push(format!("rev:     {}", artifact.rev));
    lines.push(format!("created: {}", artifact.created_at));
    lines.push(format!("updated: {}", artifact.updated_at));
    lines.join("\n")
}

fn version(version: &ArtifactVersion) -> String {
    format!(
        "rev {}  {}  {}",
        version.rev, version.created_at, version.author
    )
}

/// One thread, with its comments indented beneath the passage they discuss.
fn thread(thread: &ThreadDto) -> String {
    let mut lines = vec![format!(
        "#{} [{}] \"{}\"",
        thread.id,
        thread.status,
        truncate(&thread.anchor.quote, QUOTE_WIDTH)
    )];
    for comment in &thread.comments {
        lines.push(format!("    {}: {}", comment.author, comment.body));
    }
    lines.join("\n")
}

impl Render for artifacts::list::Op {
    fn text(output: &Vec<ArtifactMeta>, _: &NoView) -> String {
        if output.is_empty() {
            return "(no artifacts)".to_string();
        }
        output.iter().map(row).collect::<Vec<_>>().join("\n")
    }
}

impl Render for artifacts::get::Op {
    fn text(output: &ArtifactView, view: &artifacts::get::View) -> String {
        if view.meta {
            envelope(&output.meta)
        } else {
            output.content.clone()
        }
    }
}

impl Render for artifacts::history::Op {
    fn text(output: &Vec<ArtifactVersion>, _: &NoView) -> String {
        if output.is_empty() {
            return "(no revisions)".to_string();
        }
        output.iter().map(version).collect::<Vec<_>>().join("\n")
    }
}

impl Render for artifacts::threads::list::Op {
    fn text(output: &Vec<ThreadDto>, view: &artifacts::threads::list::View) -> String {
        let shown: Vec<&ThreadDto> = output
            .iter()
            .filter(|item| view.all || item.status == "open")
            .collect();
        if shown.is_empty() {
            let scope = if view.all { "" } else { "open " };
            return format!("(no {scope}threads)");
        }
        shown.into_iter().map(thread).collect::<Vec<_>>().join("\n")
    }
}

impl Render for artifacts::threads::resolve::Op {
    // The full thread, now showing `[resolved]` — not the bare id the caller passed in.
    fn text(output: &ThreadDto, _: &NoView) -> String {
        thread(output)
    }
}
