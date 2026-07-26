//! Bounded, typed worktree changes relative to a session's local branch base.
//!
//! Git supplies inventories and a no-driver patch. Parsing, bounds, path
//! display, and anchor validation are deterministic and side-effect free.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use weaver_api::{
    ChangeAnchorDto, ChangeBaseDto, ChangeBaseUnavailableReasonDto, ChangeContentDto,
    ChangeFileDto, ChangeFileStatusDto, ChangeHunkDto, ChangeLimitsDto, ChangeLineDto,
    ChangeLineKindDto, ChangePathDto, ChangeSetDto, ChangeSideDto, ChangeSourceDto,
    ChangeTotalsDto,
};

pub const MAX_FILES: usize = 100;
pub const MAX_HUNKS_PER_FILE: usize = 50;
pub const MAX_LINES_PER_FILE: usize = 1_000;
pub const MAX_TOTAL_LINES: usize = 8_000;
pub const MAX_LINE_BYTES: usize = 2_048;
const MAX_INVENTORY_BYTES: usize = 8 * 1024 * 1024;
const MAX_PATCH_BYTES: usize = 2 * 1024 * 1024;
const MAX_UNTRACKED_RENDER_BYTES: u64 = 512 * 1024;

#[derive(Debug)]
struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug)]
struct RawFile {
    status: ChangeFileStatusDto,
    path: Vec<u8>,
    old_path: Option<Vec<u8>>,
}

#[derive(Debug, Default, Clone, Copy)]
struct NumStat {
    additions: Option<u32>,
    deletions: Option<u32>,
}

fn hardened_git(work_dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(work_dir)
        .args(["-c", "color.ui=false"])
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIFF_OPTS")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}

async fn capture_git(work_dir: &Path, args: &[&str], retain: usize) -> Result<Capture> {
    let mut child = hardened_git(work_dir)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    let stdout = child.stdout.take().context("capturing git stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::with_capacity(retain.min(64 * 1024));
    let mut chunk = [0_u8; 32 * 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let room = retain.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..read.min(room)]);
        truncated |= read > room;
    }
    let status = child.wait().await?;
    if !status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(Capture { bytes, truncated })
}

async fn capture_diff(work_dir: &Path, args: &[&str], retain: usize) -> Result<Capture> {
    let mut full = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    full.extend_from_slice(args);
    capture_git(work_dir, &full, retain).await
}

async fn git_text(work_dir: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = hardened_git(work_dir)
        .args(args)
        .output()
        .await
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !output.status.success() {
        return Ok(None);
    }
    if output.stdout.len() > 4 * 1024 {
        bail!("git identity output exceeded its bound");
    }
    let value = String::from_utf8(output.stdout).context("git identity was not UTF-8")?;
    let value = value.trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

fn path_dto(raw: &[u8]) -> ChangePathDto {
    let mut display = String::new();
    for byte in raw {
        match byte {
            b' '..=b'~' if *byte != b'\\' => display.push(*byte as char),
            b'\\' => display.push_str("\\\\"),
            _ => display.push_str(&format!("\\x{byte:02x}")),
        }
    }
    ChangePathDto {
        bytes: URL_SAFE_NO_PAD.encode(raw),
        display,
    }
}

fn decode_path(path: &ChangePathDto) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(&path.bytes)
        .context("change path identity is not valid base64url")
}

fn worktree_path(work_dir: &Path, raw: &[u8]) -> Result<PathBuf> {
    if raw.is_empty() || raw.contains(&0) {
        bail!("change path is empty or contains NUL");
    }
    let relative = PathBuf::from(OsString::from_vec(raw.to_vec()));
    if relative.is_absolute()
        || relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("change path escapes the worktree");
    }
    Ok(work_dir.join(relative))
}

fn nul_records(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
}

fn parse_raw_files(bytes: &[u8]) -> Vec<RawFile> {
    let mut records = nul_records(bytes);
    let mut files = Vec::new();
    while let Some(header) = records.next() {
        let Some(code) = header.rsplit(|byte| *byte == b' ').next() else {
            continue;
        };
        let letter = code.first().copied().unwrap_or(b'M');
        let Some(first_path) = records.next() else {
            break;
        };
        let (old_path, path) = if matches!(letter, b'R' | b'C') {
            let Some(new_path) = records.next() else {
                break;
            };
            (Some(first_path.to_vec()), new_path.to_vec())
        } else {
            (None, first_path.to_vec())
        };
        let status = match letter {
            b'A' => ChangeFileStatusDto::Added,
            b'D' => ChangeFileStatusDto::Deleted,
            b'R' => ChangeFileStatusDto::Renamed,
            b'C' => ChangeFileStatusDto::Copied,
            b'T' => ChangeFileStatusDto::TypeChanged,
            _ => ChangeFileStatusDto::Modified,
        };
        files.push(RawFile {
            status,
            path,
            old_path,
        });
    }
    files
}

fn parse_numstat(bytes: &[u8]) -> BTreeMap<Vec<u8>, NumStat> {
    let mut records = bytes.split(|byte| *byte == 0);
    let mut result = BTreeMap::new();
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next().unwrap_or_default();
        let deleted = fields.next().unwrap_or_default();
        let mut path = fields.next().unwrap_or_default();
        if path.is_empty() {
            // Rename/copy under `-z`: the following two records are old/new.
            let _old = records.next();
            path = records.next().unwrap_or_default();
        }
        result.insert(
            path.to_vec(),
            NumStat {
                additions: std::str::from_utf8(added).ok().and_then(|v| v.parse().ok()),
                deletions: std::str::from_utf8(deleted)
                    .ok()
                    .and_then(|v| v.parse().ok()),
            },
        );
    }
    result
}

fn source_paths(bytes: &[u8]) -> BTreeSet<Vec<u8>> {
    nul_records(bytes).map(ToOwned::to_owned).collect()
}

fn sanitize_text(value: &str, limit: usize) -> (String, bool) {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch == '\t' || !ch.is_control() {
                ch
            } else {
                '\u{fffd}'
            }
        })
        .collect();
    if cleaned.len() <= limit {
        return (cleaned, false);
    }
    let mut end = limit;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &cleaned[..end]), true)
}

fn parse_range(value: &str) -> Option<(u32, u32)> {
    let value = value.trim_start_matches(['-', '+']);
    let mut parts = value.splitn(2, ',');
    let start = parts.next()?.parse().ok()?;
    let count = parts.next().and_then(|part| part.parse().ok()).unwrap_or(1);
    Some((start, count))
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let mut ranges = rest[..end].split_whitespace();
    let (old_start, _) = parse_range(ranges.next()?)?;
    let (new_start, _) = parse_range(ranges.next()?)?;
    Some((old_start, new_start))
}

fn patch_sections(patch: &[u8]) -> VecDeque<&[u8]> {
    let marker = b"diff --git ";
    let mut starts = Vec::new();
    let mut offset = 0;
    while let Some(index) = patch[offset..]
        .windows(marker.len())
        .position(|window| window == marker)
    {
        let start = offset + index;
        if start == 0 || patch[start - 1] == b'\n' {
            starts.push(start);
        }
        offset = start + marker.len();
    }
    let mut sections = VecDeque::new();
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(patch.len());
        sections.push_back(&patch[*start..end]);
    }
    sections
}

fn parse_hunks(
    section: &[u8],
    remaining_total: &mut usize,
) -> (ChangeContentDto, Vec<ChangeHunkDto>, bool) {
    let text = String::from_utf8_lossy(section);
    if text.contains("\nBinary files ") || text.contains("\nGIT binary patch") {
        return (ChangeContentDto::Binary, Vec::new(), false);
    }
    let mut hunks = Vec::new();
    let mut current: Option<ChangeHunkDto> = None;
    let mut old_line = 0;
    let mut new_line = 0;
    let mut file_lines = 0;
    let mut truncated = false;
    for line in text.lines() {
        if let Some((old_start, new_start)) = parse_hunk_header(line) {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            if hunks.len() >= MAX_HUNKS_PER_FILE {
                truncated = true;
                break;
            }
            old_line = old_start;
            new_line = new_start;
            let (header, cut) = sanitize_text(line, MAX_LINE_BYTES);
            truncated |= cut;
            current = Some(ChangeHunkDto {
                header,
                lines: Vec::new(),
                truncated: false,
            });
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            continue;
        };
        if line == "\\ No newline at end of file" {
            continue;
        }
        let Some(prefix) = line.as_bytes().first().copied() else {
            continue;
        };
        let (kind, old, new) = match prefix {
            b'+' => {
                let current_new = new_line;
                new_line += 1;
                (ChangeLineKindDto::Addition, None, Some(current_new))
            }
            b'-' => {
                let current_old = old_line;
                old_line += 1;
                (ChangeLineKindDto::Deletion, Some(current_old), None)
            }
            b' ' => {
                let current_old = old_line;
                let current_new = new_line;
                old_line += 1;
                new_line += 1;
                (
                    ChangeLineKindDto::Context,
                    Some(current_old),
                    Some(current_new),
                )
            }
            _ => continue,
        };
        if file_lines >= MAX_LINES_PER_FILE || *remaining_total == 0 {
            hunk.truncated = true;
            truncated = true;
            break;
        }
        let body = line.get(1..).unwrap_or_default();
        let (text, cut) = sanitize_text(body, MAX_LINE_BYTES);
        hunk.truncated |= cut;
        truncated |= cut;
        hunk.lines.push(ChangeLineDto {
            kind,
            old_line: old,
            new_line: new,
            text,
        });
        file_lines += 1;
        *remaining_total -= 1;
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    (ChangeContentDto::Text, hunks, truncated)
}

async fn hash_path(work_dir: &Path, raw: &[u8], hasher: &mut Sha256) -> Result<Option<u32>> {
    let path = worktree_path(work_dir, raw)?;
    let metadata = tokio::fs::symlink_metadata(&path).await?;
    hasher.update(raw);
    if metadata.file_type().is_symlink() {
        hasher.update(b"symlink\0");
        let target = tokio::fs::read_link(path).await?;
        hasher.update(target.as_os_str().as_bytes());
        Ok(None)
    } else if metadata.is_file() {
        hasher.update(b"file\0");
        let mut file = tokio::fs::File::open(path).await?;
        let mut chunk = [0_u8; 32 * 1024];
        let mut bytes = 0_u64;
        let mut lines = 0_u64;
        let mut last = 0_u8;
        let mut binary = false;
        loop {
            let read = file.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            hasher.update(&chunk[..read]);
            bytes += read as u64;
            lines += chunk[..read].iter().filter(|byte| **byte == b'\n').count() as u64;
            binary |= chunk[..read].contains(&0);
            last = chunk[read - 1];
        }
        if bytes > 0 && last != b'\n' {
            lines += 1;
        }
        Ok((!binary).then_some(lines.min(u32::MAX as u64) as u32))
    } else {
        hasher.update(b"unsupported\0");
        Ok(None)
    }
}

async fn untracked_file(
    work_dir: &Path,
    raw: &[u8],
    sources: Vec<ChangeSourceDto>,
    remaining_total: &mut usize,
) -> Result<ChangeFileDto> {
    let path = worktree_path(work_dir, raw)?;
    let metadata = tokio::fs::symlink_metadata(&path).await?;
    let mut file = ChangeFileDto {
        status: ChangeFileStatusDto::Untracked,
        path: path_dto(raw),
        old_path: None,
        sources,
        additions: None,
        deletions: None,
        content: ChangeContentDto::Unsupported,
        hunks: Vec::new(),
        truncated: false,
    };
    if !metadata.is_file() {
        return Ok(file);
    }
    if metadata.len() > MAX_UNTRACKED_RENDER_BYTES {
        file.content = ChangeContentDto::Oversize;
        file.truncated = true;
        return Ok(file);
    }
    let bytes = tokio::fs::read(path).await?;
    if bytes.contains(&0) {
        file.content = ChangeContentDto::Binary;
        return Ok(file);
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    file.additions = Some(lines.len().min(u32::MAX as usize) as u32);
    let mut rendered = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index >= MAX_LINES_PER_FILE || *remaining_total == 0 {
            file.truncated = true;
            break;
        }
        let (text, cut) = sanitize_text(line, MAX_LINE_BYTES);
        file.truncated |= cut;
        rendered.push(ChangeLineDto {
            kind: ChangeLineKindDto::Addition,
            old_line: None,
            new_line: Some((index + 1) as u32),
            text,
        });
        *remaining_total -= 1;
    }
    file.content = ChangeContentDto::Text;
    if !rendered.is_empty() {
        file.hunks.push(ChangeHunkDto {
            header: format!("@@ -0,0 +1,{} @@", lines.len()),
            lines: rendered,
            truncated: file.truncated,
        });
    }
    Ok(file)
}

fn limits() -> ChangeLimitsDto {
    ChangeLimitsDto {
        max_files: MAX_FILES as u32,
        max_hunks_per_file: MAX_HUNKS_PER_FILE as u32,
        max_lines_per_file: MAX_LINES_PER_FILE as u32,
        max_total_lines: MAX_TOTAL_LINES as u32,
        max_line_bytes: MAX_LINE_BYTES as u32,
    }
}

fn unavailable(
    reference: &str,
    reason: ChangeBaseUnavailableReasonDto,
    head_oid: Option<String>,
) -> ChangeSetDto {
    ChangeSetDto {
        version: None,
        base: ChangeBaseDto::Unavailable {
            reference: reference.to_string(),
            reason,
        },
        head_oid,
        totals: ChangeTotalsDto::default(),
        files: Vec::new(),
        truncated: false,
        limits: limits(),
    }
}

/// Read one session worktree without fetching or changing Git state.
pub async fn load(work_dir: &Path, base_reference: &str) -> Result<ChangeSetDto> {
    let Some(head_oid) = git_text(work_dir, &["rev-parse", "--verify", "HEAD"]).await? else {
        return Ok(unavailable(
            base_reference,
            ChangeBaseUnavailableReasonDto::UnbornHead,
            None,
        ));
    };
    let local_base = if base_reference.starts_with("refs/heads/") {
        base_reference.to_string()
    } else {
        format!("refs/heads/{base_reference}")
    };
    let base_spec = format!("{local_base}^{{commit}}");
    let Some(base_tip) = git_text(work_dir, &["rev-parse", "--verify", &base_spec]).await? else {
        return Ok(unavailable(
            base_reference,
            ChangeBaseUnavailableReasonDto::MissingBase,
            Some(head_oid),
        ));
    };
    let Some(base_oid) = git_text(work_dir, &["merge-base", &head_oid, &base_tip]).await? else {
        return Ok(unavailable(
            base_reference,
            ChangeBaseUnavailableReasonDto::NoMergeBase,
            Some(head_oid),
        ));
    };

    let final_raw = capture_diff(
        work_dir,
        &[
            "--raw",
            "--full-index",
            "--find-renames",
            "-z",
            &base_oid,
            "--",
        ],
        MAX_INVENTORY_BYTES,
    )
    .await?;
    let numstat = capture_diff(
        work_dir,
        &["--numstat", "--find-renames", "-z", &base_oid, "--"],
        MAX_INVENTORY_BYTES,
    )
    .await?;
    let committed = capture_diff(
        work_dir,
        &[
            "--name-only",
            "--find-renames",
            "-z",
            &base_oid,
            "HEAD",
            "--",
        ],
        MAX_INVENTORY_BYTES,
    )
    .await?;
    let staged = capture_diff(
        work_dir,
        &[
            "--cached",
            "--name-only",
            "--find-renames",
            "-z",
            "HEAD",
            "--",
        ],
        MAX_INVENTORY_BYTES,
    )
    .await?;
    let unstaged = capture_diff(
        work_dir,
        &["--name-only", "--find-renames", "-z", "--"],
        MAX_INVENTORY_BYTES,
    )
    .await?;
    let untracked = capture_git(
        work_dir,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        MAX_INVENTORY_BYTES,
    )
    .await?;
    let inventory_truncated = [
        &final_raw, &numstat, &committed, &staged, &unstaged, &untracked,
    ]
    .iter()
    .any(|capture| capture.truncated);
    let mut raw_files = parse_raw_files(&final_raw.bytes);

    let mut hasher = Sha256::new();
    hasher.update(b"weaver-changes-v1\0");
    hasher.update(base_oid.as_bytes());
    hasher.update([0]);
    hasher.update(head_oid.as_bytes());
    hasher.update([0]);
    hasher.update(&final_raw.bytes);
    for capture in [&committed, &staged, &unstaged, &untracked] {
        hasher.update([0]);
        hasher.update(&capture.bytes);
    }
    let untracked_paths: Vec<Vec<u8>> = nul_records(&untracked.bytes)
        .map(ToOwned::to_owned)
        .collect();
    let mut untracked_additions = BTreeMap::new();
    if !inventory_truncated {
        for file in raw_files
            .iter()
            .filter(|file| file.status != ChangeFileStatusDto::Deleted)
        {
            hash_path(work_dir, &file.path, &mut hasher).await?;
        }
        for path in &untracked_paths {
            if let Some(lines) = hash_path(work_dir, path, &mut hasher).await? {
                untracked_additions.insert(path.clone(), lines);
            }
        }
    }
    let version =
        (!inventory_truncated).then(|| format!("changes-v1:{}", hex::encode(hasher.finalize())));

    let patch = capture_diff(
        work_dir,
        &["--patch", "--find-renames", "--unified=3", &base_oid, "--"],
        MAX_PATCH_BYTES,
    )
    .await?;
    let mut sections = patch_sections(&patch.bytes);
    let stats = parse_numstat(&numstat.bytes);
    let committed = source_paths(&committed.bytes);
    let staged = source_paths(&staged.bytes);
    let unstaged = source_paths(&unstaged.bytes);
    let untracked_set: BTreeSet<Vec<u8>> = untracked_paths.iter().cloned().collect();

    for path in &untracked_paths {
        if !raw_files.iter().any(|file| file.path == *path) {
            raw_files.push(RawFile {
                status: ChangeFileStatusDto::Untracked,
                path: path.clone(),
                old_path: None,
            });
        }
    }
    let total_files = raw_files.len().min(u32::MAX as usize) as u32;
    let mut remaining_total = MAX_TOTAL_LINES;
    let mut files = Vec::new();
    for raw in raw_files.iter().take(MAX_FILES) {
        let mut sources = Vec::new();
        if committed.contains(&raw.path) {
            sources.push(ChangeSourceDto::Committed);
        }
        if staged.contains(&raw.path) {
            sources.push(ChangeSourceDto::Staged);
        }
        if unstaged.contains(&raw.path) {
            sources.push(ChangeSourceDto::Unstaged);
        }
        if untracked_set.contains(&raw.path) {
            sources.push(ChangeSourceDto::Untracked);
        }
        if raw.status == ChangeFileStatusDto::Untracked {
            files.push(untracked_file(work_dir, &raw.path, sources, &mut remaining_total).await?);
            continue;
        }
        let section = sections.pop_front().unwrap_or_default();
        let (content, hunks, section_truncated) = parse_hunks(section, &mut remaining_total);
        let stat = stats.get(&raw.path).copied().unwrap_or_default();
        files.push(ChangeFileDto {
            status: raw.status,
            path: path_dto(&raw.path),
            old_path: raw.old_path.as_deref().map(path_dto),
            sources,
            additions: stat.additions,
            deletions: stat.deletions,
            content,
            hunks,
            truncated: section_truncated || (patch.truncated && sections.is_empty()),
        });
    }
    let totals = ChangeTotalsDto {
        files: total_files,
        additions: stats
            .values()
            .filter_map(|stat| stat.additions)
            .chain(untracked_additions.values().copied())
            .sum(),
        deletions: stats.values().filter_map(|stat| stat.deletions).sum(),
        truncated: numstat.truncated || inventory_truncated,
    };
    Ok(ChangeSetDto {
        version,
        base: ChangeBaseDto::Available {
            reference: base_reference.to_string(),
            oid: base_oid,
        },
        head_oid: Some(head_oid),
        totals,
        truncated: inventory_truncated
            || patch.truncated
            || raw_files.len() > MAX_FILES
            || remaining_total == 0,
        files,
        limits: limits(),
    })
}

/// Validate a mutable comment anchor against the exact snapshot on screen.
pub fn validate_anchor(
    changes: &ChangeSetDto,
    version: &str,
    anchor: &ChangeAnchorDto,
) -> Result<()> {
    if changes.version.as_deref() != Some(version) {
        bail!("change-set version moved; refresh before anchoring");
    }
    if anchor.start_line == 0 || anchor.end_line < anchor.start_line {
        bail!("change anchor range is invalid");
    }
    let identity = decode_path(&anchor.path)?;
    let file = changes
        .files
        .iter()
        .find(|file| decode_path(&file.path).ok().as_deref() == Some(identity.as_slice()))
        .context("change anchor path is not present in this version")?;
    let mut expected = anchor.start_line;
    for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
        let (number, allowed) = match anchor.side {
            ChangeSideDto::Old => (
                line.old_line,
                matches!(
                    line.kind,
                    ChangeLineKindDto::Deletion | ChangeLineKindDto::Context
                ),
            ),
            ChangeSideDto::New => (
                line.new_line,
                matches!(
                    line.kind,
                    ChangeLineKindDto::Addition | ChangeLineKindDto::Context
                ),
            ),
        };
        if allowed && number == Some(expected) {
            expected += 1;
            if expected > anchor.end_line {
                return Ok(());
            }
        }
    }
    bail!("change anchor does not name a contiguous visible line range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_hunks_and_enforces_side_ranges() {
        let patch = b"diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -2,2 +2,3 @@ fn main() {\n same\n-old\n+new\n+next\n";
        let mut remaining = 100;
        let (content, hunks, truncated) =
            parse_hunks(patch_sections(patch).pop_front().unwrap(), &mut remaining);
        assert_eq!(content, ChangeContentDto::Text);
        assert!(!truncated);
        assert_eq!(hunks[0].lines[1].old_line, Some(3));
        assert_eq!(hunks[0].lines[2].new_line, Some(3));

        let path = path_dto(b"src/a.rs");
        let changes = ChangeSetDto {
            version: Some("changes-v1:test".to_string()),
            base: ChangeBaseDto::Available {
                reference: "main".to_string(),
                oid: "base".to_string(),
            },
            head_oid: Some("head".to_string()),
            totals: ChangeTotalsDto::default(),
            files: vec![ChangeFileDto {
                status: ChangeFileStatusDto::Modified,
                path: path.clone(),
                old_path: None,
                sources: vec![ChangeSourceDto::Unstaged],
                additions: Some(2),
                deletions: Some(1),
                content,
                hunks,
                truncated: false,
            }],
            truncated: false,
            limits: limits(),
        };
        let anchor = ChangeAnchorDto {
            path,
            side: ChangeSideDto::New,
            start_line: 3,
            end_line: 4,
            hunk_header: "@@ -2,2 +2,3 @@".to_string(),
            context_before: vec![],
            selected: vec!["new".to_string(), "next".to_string()],
            context_after: vec![],
        };
        validate_anchor(&changes, "changes-v1:test", &anchor).unwrap();
        let wrong_side = ChangeAnchorDto {
            side: ChangeSideDto::Old,
            ..anchor
        };
        assert!(validate_anchor(&changes, "changes-v1:test", &wrong_side).is_err());
    }

    #[test]
    fn paths_round_trip_without_control_or_utf8_loss() {
        let raw = b"bad\n\xff.rs";
        let path = path_dto(raw);
        assert_eq!(decode_path(&path).unwrap(), raw);
        assert_eq!(path.display, "bad\\x0a\\xff.rs");
    }

    #[test]
    fn bounds_long_lines_and_file_line_count() {
        let long = "x".repeat(MAX_LINE_BYTES + 50);
        let patch = format!("diff --git a/a b/a\n@@ -1 +1 @@\n-{long}\n+{long}\n");
        let mut remaining = 1;
        let (_, hunks, truncated) = parse_hunks(patch.as_bytes(), &mut remaining);
        assert!(truncated);
        assert!(hunks[0].truncated);
        assert!(hunks[0].lines[0].text.ends_with('…'));
        assert_eq!(hunks[0].lines.len(), 1);
    }
}
