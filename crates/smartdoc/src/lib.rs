//! smartdoc — the markdown-convention layer.
//!
//! A thin, dependency-free reader for the conventions weaver documents share:
//! references to other nouns (`#123` → an issue, `artifact:<name>` → an
//! artifact, optionally a session), GitHub-style checklist items, and a leading
//! YAML-ish frontmatter block. It does three things and nothing else:
//!
//! * [`parse`] turns markdown source into a [`Doc`] — its frontmatter, its
//!   references, and its checklist items, in document order;
//! * [`refs`] collects the distinct [`Ref`]s a doc names;
//! * [`project`] joins a doc against a caller-supplied status map (the *probes*)
//!   into a [`Projection`] the renderer stamps live chips from.
//!
//! The crate is deliberately generic: it knows the *syntax* of a reference but
//! nothing about where its status comes from. weaver-core supplies the probe
//! data (issue title/status/claim, …); the artifact `GET` returns the resulting
//! projection so the SPA chips and a terminal `weaver artifact show` render the
//! exact same join — render-time projection, structure in the doc, state in the
//! DB.
//!
//! Parsing is tolerant by design (it never errors) and *code-aware*: a `#123`
//! or `artifact:x` inside a fenced ```` ``` ```` block or an inline `` `code` ``
//! span is left alone, so documentation of the syntax doesn't trip the parser.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// A reference to another weaver noun, named inline in a document.
///
/// * `Issue(123)` — written `#123`; resolves against the issue ledger.
/// * `Artifact(name)` — written `artifact:<name>`; resolves to another artifact.
/// * `Session(id)` — written `session:<id>`; resolves to a session/branch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ref {
    Issue(u64),
    Artifact(String),
    Session(String),
}

/// A GitHub-style checklist item (`- [ ] …` / `- [x] …`). The checked state
/// here is what the *text* says; when an item references an issue the renderer
/// projects the live state from the ledger instead (the GitHub-task-list shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    /// Whether the box is ticked in the source text.
    pub checked: bool,
    /// The item's text, with the `- [ ] ` marker stripped.
    pub text: String,
    /// References found in this item, if any — usually the issue the item is
    /// *about* (`- #41 Index layer`).
    pub refs: Vec<Ref>,
}

/// A parsed document: its frontmatter, the references it names (in document
/// order, with duplicates), and its checklist items.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Doc {
    /// Leading `---`-delimited `key: value` frontmatter, if present.
    pub frontmatter: HashMap<String, String>,
    /// Every reference in the body, in document order (duplicates kept — a doc
    /// may legitimately mention `#41` more than once).
    pub refs: Vec<Ref>,
    /// The checklist items, in document order.
    pub checklist: Vec<ChecklistItem>,
}

/// The resolved status of one reference — the probe result the renderer stamps
/// into a chip. Generic over the noun: an issue fills `title`/`status`/
/// `claimed_branch`; an artifact or session fills what it has. `exists = false`
/// is a dangling reference (the chip renders muted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefStatus {
    /// Whether the referenced thing was found by the probe.
    pub exists: bool,
    /// Human title (issue title, artifact title, …).
    pub title: String,
    /// State string (`open` / `closed` for an issue, etc.). Empty when n/a.
    pub status: String,
    /// The branch working it, for an issue (`null` = unclaimed backlog).
    pub claimed_branch: Option<String>,
}

/// A reference with its resolved status — one entry in a [`Projection`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedRef {
    pub reference: Ref,
    pub status: RefStatus,
}

/// The result of [`project`]: every distinct reference in a doc paired with its
/// probed status, in first-seen order. The renderer walks this to stamp chips;
/// an unresolved reference carries a `RefStatus { exists: false, .. }`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Projection {
    pub refs: Vec<ProjectedRef>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a markdown document into a [`Doc`]. Tolerant and code-aware: never
/// errors, and skips references inside fenced code blocks and inline code spans.
pub fn parse(src: &str) -> Doc {
    let lines: Vec<&str> = src.lines().collect();
    let mut doc = Doc::default();
    let mut i = 0;

    // Optional leading YAML-ish frontmatter delimited by `---` lines.
    if lines.first().map(|l| l.trim()) == Some("---") {
        i = 1;
        while i < lines.len() && lines[i].trim() != "---" {
            if let Some((k, v)) = lines[i].split_once(':') {
                let k = k.trim();
                if !k.is_empty() {
                    doc.frontmatter.insert(k.to_string(), v.trim().to_string());
                }
            }
            i += 1;
        }
        if i < lines.len() {
            i += 1; // skip the closing `---`
        }
    }

    let mut in_fence = false;
    let mut fence_marker = "";
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // Fenced code blocks: ``` or ~~~ toggle a verbatim region whose contents
        // never yield references. Match the marker so a ``` inside a ~~~ block
        // doesn't close it.
        if let Some(marker) = fence_open(trimmed) {
            if !in_fence {
                in_fence = true;
                fence_marker = marker;
            } else if trimmed.starts_with(fence_marker) {
                in_fence = false;
                fence_marker = "";
            }
            i += 1;
            continue;
        }
        if in_fence {
            i += 1;
            continue;
        }

        // A checklist item carries its own refs and checked state.
        if let Some(item) = checklist_item(trimmed) {
            doc.refs.extend(item.refs.iter().cloned());
            doc.checklist.push(item);
            i += 1;
            continue;
        }

        doc.refs.extend(scan_refs(line));
        i += 1;
    }

    doc
}

/// The distinct references a doc names, in first-seen order.
pub fn refs(doc: &Doc) -> Vec<Ref> {
    let mut seen = Vec::new();
    for r in &doc.refs {
        if !seen.contains(r) {
            seen.push(r.clone());
        }
    }
    seen
}

/// Join a doc's references against a status map into a [`Projection`]. Every
/// distinct reference (first-seen order) gets an entry; one missing from `status`
/// projects as non-existent (`RefStatus::default()`), the muted-chip case.
pub fn project(doc: &Doc, status: &HashMap<Ref, RefStatus>) -> Projection {
    let refs = refs(doc)
        .into_iter()
        .map(|reference| {
            let status = status.get(&reference).cloned().unwrap_or_default();
            ProjectedRef { reference, status }
        })
        .collect();
    Projection { refs }
}

/// If `trimmed` opens or closes a code fence, return its marker (` ``` ` or
/// `~~~`). A fence is three or more of the same char at the line start.
fn fence_open(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Parse a GitHub-style checklist line (`- [ ] …`, `- [x] …`, also `*`/`+`
/// bullets), returning its checked state, text, and any refs. `None` when the
/// line isn't a checklist item.
fn checklist_item(trimmed: &str) -> Option<ChecklistItem> {
    let after_bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    let rest = after_bullet.trim_start();
    let (checked, body) = if let Some(b) = rest.strip_prefix("[ ]") {
        (false, b)
    } else if let Some(b) = rest
        .strip_prefix("[x]")
        .or_else(|| rest.strip_prefix("[X]"))
    {
        (true, b)
    } else {
        return None;
    };
    let text = body.trim().to_string();
    Some(ChecklistItem {
        checked,
        refs: scan_refs(&text),
        text,
    })
}

/// Scan one line for references, skipping inline code spans (backtick-delimited).
/// Recognizes `#<digits>`, `artifact:<name>`, and `session:<id>`.
fn scan_refs(line: &str) -> Vec<Ref> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i < bytes.len() {
        let c = bytes[i];
        // A backtick toggles an inline code span; its contents are verbatim.
        if c == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if in_code {
            i += 1;
            continue;
        }
        // `#<digits>` — an issue reference. Require the `#` to start a token
        // (not be mid-word, e.g. a `foo#3` plan key remnant; not a `##5` run),
        // and the digits to not run into an identifier char (`#3abc`).
        if c == b'#' && starts_token(bytes, i) && (i == 0 || bytes[i - 1] != b'#') {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > start && !ends_in_ident(bytes, j) {
                if let Ok(n) = line[start..j].parse::<u64>() {
                    out.push(Ref::Issue(n));
                }
                i = j;
                continue;
            }
        }
        // `artifact:<name>` / `session:<id>` — a prefixed reference.
        if (c == b'a' || c == b's') && starts_token(bytes, i) {
            if let Some((kind, name, end)) = scan_prefixed(line, i) {
                match kind {
                    "artifact" => out.push(Ref::Artifact(name)),
                    "session" => out.push(Ref::Session(name)),
                    _ => {}
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// True when the byte at `i` begins a token — it is at line start or preceded by
/// a non-identifier byte. Stops `bar#3` (an embedded `#`) from reffing.
fn starts_token(bytes: &[u8], i: usize) -> bool {
    i == 0 || !is_ident_byte(bytes[i - 1])
}

/// True when the token ending at `j` is immediately followed by an identifier
/// byte — i.e. it ran into more text (`#3abc`), so it isn't a clean reference.
fn ends_in_ident(bytes: &[u8], j: usize) -> bool {
    j < bytes.len() && is_ident_byte(bytes[j])
}

/// Identifier bytes for token boundaries: alphanumerics plus `_`/`-`.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// At byte `i`, try to read a `artifact:<name>` or `session:<name>` reference.
/// Returns `(kind, name, end_index)`. The name runs over identifier bytes plus
/// `.`/`/` (so `artifact:plan.v2` and `session:repo/branch` parse), and must be
/// non-empty.
fn scan_prefixed(line: &str, i: usize) -> Option<(&'static str, String, usize)> {
    let rest = &line[i..];
    let kind: &'static str = if rest.starts_with("artifact:") {
        "artifact"
    } else if rest.starts_with("session:") {
        "session"
    } else {
        return None;
    };
    let name_start = i + kind.len() + 1; // +1 for the ':'
    let bytes = line.as_bytes();
    let mut j = name_start;
    while j < bytes.len() && (is_ident_byte(bytes[j]) || bytes[j] == b'.' || bytes[j] == b'/') {
        j += 1;
    }
    // Trailing `.`/`/` are sentence punctuation, not part of the name
    // (`see artifact:plan.` ends a sentence; `artifact:plan.v2` keeps the dot).
    while j > name_start && matches!(bytes[j - 1], b'.' | b'/') {
        j -= 1;
    }
    if j == name_start {
        return None;
    }
    Some((kind, line[name_start..j].to_string(), j))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let doc = parse("---\ntitle: Search rewrite\nstatus: active\n---\n\n# Body\n");
        assert_eq!(
            doc.frontmatter.get("title").map(String::as_str),
            Some("Search rewrite")
        );
        assert_eq!(
            doc.frontmatter.get("status").map(String::as_str),
            Some("active")
        );
    }

    #[test]
    fn extracts_issue_artifact_and_session_refs() {
        let doc = parse("See #41 and #42, design in artifact:plan, child session:ab12cd34.");
        assert_eq!(
            refs(&doc),
            vec![
                Ref::Issue(41),
                Ref::Issue(42),
                Ref::Artifact("plan".into()),
                Ref::Session("ab12cd34".into()),
            ]
        );
    }

    #[test]
    fn skips_refs_inside_inline_code_spans() {
        // A `#123` inside backticks is documentation, not a reference.
        let doc = parse("Reference an issue with `#123`; this one is live: #41.");
        assert_eq!(refs(&doc), vec![Ref::Issue(41)]);
    }

    #[test]
    fn skips_refs_inside_fenced_code_blocks() {
        let src = "Before #1\n```\nnot a ref: #999 artifact:nope\n```\nAfter #2\n";
        let doc = parse(src);
        assert_eq!(refs(&doc), vec![Ref::Issue(1), Ref::Issue(2)]);
    }

    #[test]
    fn tilde_fence_is_honored_and_not_closed_by_backticks() {
        let src = "~~~\n#1 ```still inside``` #2\n~~~\n#3\n";
        let doc = parse(src);
        assert_eq!(refs(&doc), vec![Ref::Issue(3)]);
    }

    #[test]
    fn ignores_embedded_and_malformed_hashes() {
        // `bar#3` is mid-token; `#3abc` runs into an identifier; `#` alone is bare.
        let doc = parse("bar#3 and #3abc and # and ##5");
        assert!(refs(&doc).is_empty());
    }

    #[test]
    fn checklist_items_carry_checked_state_and_refs() {
        let src = "- [ ] #41 Index layer\n- [x] decide single vs distributed\n- not a checklist\n";
        let doc = parse(src);
        assert_eq!(doc.checklist.len(), 2);
        assert!(!doc.checklist[0].checked);
        assert_eq!(doc.checklist[0].text, "#41 Index layer");
        assert_eq!(doc.checklist[0].refs, vec![Ref::Issue(41)]);
        assert!(doc.checklist[1].checked);
        assert!(doc.checklist[1].refs.is_empty());
        // The item's ref is also in the doc-level ref list.
        assert_eq!(refs(&doc), vec![Ref::Issue(41)]);
    }

    #[test]
    fn refs_dedupe_in_first_seen_order() {
        let doc = parse("#41 then #42 then #41 again");
        assert_eq!(refs(&doc), vec![Ref::Issue(41), Ref::Issue(42)]);
    }

    #[test]
    fn project_joins_status_and_marks_missing_as_nonexistent() {
        let doc = parse("Tasks: #41 and #99");
        let mut status = HashMap::new();
        status.insert(
            Ref::Issue(41),
            RefStatus {
                exists: true,
                title: "Index layer".into(),
                status: "open".into(),
                claimed_branch: Some("weaver/index".into()),
            },
        );
        let proj = project(&doc, &status);
        assert_eq!(proj.refs.len(), 2);
        assert_eq!(proj.refs[0].reference, Ref::Issue(41));
        assert!(proj.refs[0].status.exists);
        assert_eq!(proj.refs[0].status.title, "Index layer");
        assert_eq!(
            proj.refs[0].status.claimed_branch.as_deref(),
            Some("weaver/index")
        );
        // #99 wasn't probed → muted chip.
        assert_eq!(proj.refs[1].reference, Ref::Issue(99));
        assert!(!proj.refs[1].status.exists);
    }

    #[test]
    fn prefixed_names_allow_dots_and_slashes() {
        let doc = parse("artifact:plan.v2 and session:repo/branch");
        assert_eq!(
            refs(&doc),
            vec![
                Ref::Artifact("plan.v2".into()),
                Ref::Session("repo/branch".into()),
            ]
        );
    }

    #[test]
    fn bare_file_is_tolerated() {
        let doc = parse("just some prose\nno structure at all");
        assert!(doc.refs.is_empty());
        assert!(doc.checklist.is_empty());
        assert!(doc.frontmatter.is_empty());
    }
}
