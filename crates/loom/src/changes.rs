//! Bounded, typed worktree changes relative to a session's local branch base.
//!
//! Git supplies inventories and a no-driver patch. Parsing, bounds, path
//! display, and anchor validation are deterministic and side-effect free.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{CString, OsStr};
use std::io::ErrorKind;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::Instant;
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
const MAX_IDENTITY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CONFIG_BYTES: usize = 256 * 1024;
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
    timed_out: bool,
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

#[derive(Debug, Default)]
struct GitReadPolicy {
    filter_drivers: Vec<String>,
}

fn hardened_git(work_dir: &Path, policy: &GitReadPolicy) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(work_dir)
        .args([
            "-c",
            "color.ui=false",
            "-c",
            "diff.autoRefreshIndex=false",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "submodule.recurse=false",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIFF_OPTS")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for driver in &policy.filter_drivers {
        command
            .arg("-c")
            .arg(format!("filter.{driver}.clean="))
            .arg("-c")
            .arg(format!("filter.{driver}.process="))
            .arg("-c")
            .arg(format!("filter.{driver}.smudge="))
            .arg("-c")
            .arg(format!("filter.{driver}.required=false"));
    }
    command
}

async fn capture_git_status(
    work_dir: &Path,
    policy: &GitReadPolicy,
    args: &[&str],
    retain: usize,
) -> Result<(Capture, bool)> {
    let mut child = hardened_git(work_dir, policy)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    let stdout = child.stdout.take().context("capturing git stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::with_capacity(retain.min(64 * 1024));
    let mut chunk = vec![0_u8; 32 * 1024];
    let deadline = Instant::now() + GIT_COMMAND_TIMEOUT;
    let result = tokio::time::timeout_at(deadline, async {
        let mut truncated = false;
        loop {
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            let room = retain.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..read.min(room)]);
            if read > room {
                truncated = true;
                child.start_kill().context("stopping bounded git capture")?;
                break;
            }
        }
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status.success(), truncated))
    })
    .await;
    match result {
        Ok(result) => {
            let (success, truncated) = result?;
            Ok((
                Capture {
                    bytes,
                    truncated,
                    timed_out: false,
                },
                success,
            ))
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Ok((
                Capture {
                    bytes,
                    truncated: true,
                    timed_out: true,
                },
                false,
            ))
        }
    }
}

async fn capture_git(
    work_dir: &Path,
    policy: &GitReadPolicy,
    args: &[&str],
    retain: usize,
) -> Result<Capture> {
    let (capture, success) = capture_git_status(work_dir, policy, args, retain).await?;
    if !success && !capture.truncated {
        bail!("git {} failed", args.join(" "));
    }
    Ok(capture)
}

async fn capture_diff(
    work_dir: &Path,
    policy: &GitReadPolicy,
    args: &[&str],
    retain: usize,
) -> Result<Capture> {
    let mut full = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--ignore-submodules=all",
    ];
    full.extend_from_slice(args);
    capture_git(work_dir, policy, &full, retain).await
}

async fn git_text(
    work_dir: &Path,
    policy: &GitReadPolicy,
    args: &[&str],
) -> Result<Option<String>> {
    let (capture, success) = capture_git_status(work_dir, policy, args, 4 * 1024).await?;
    if capture.timed_out {
        bail!("git {} exceeded its deadline", args.join(" "));
    }
    if capture.truncated {
        bail!("git identity output exceeded its bound");
    }
    if !success {
        return Ok(None);
    }
    let value = String::from_utf8(capture.bytes).context("git identity was not UTF-8")?;
    let value = value.trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

impl GitReadPolicy {
    async fn load(work_dir: &Path) -> Result<Self> {
        let empty = Self::default();
        let capture = capture_git(
            work_dir,
            &empty,
            &["config", "--includes", "--null", "--name-only", "--list"],
            MAX_CONFIG_BYTES,
        )
        .await?;
        if capture.truncated {
            bail!("git configuration exceeded its safe read bound");
        }
        let mut filter_drivers = Vec::new();
        for raw in nul_records(&capture.bytes) {
            let key = std::str::from_utf8(raw).context("git configuration key was not UTF-8")?;
            let Some((driver, setting)) =
                key.strip_prefix("filter.").and_then(|v| v.rsplit_once('.'))
            else {
                continue;
            };
            if !driver.is_empty() && matches!(setting, "clean" | "process" | "required" | "smudge")
            {
                filter_drivers.push(driver.to_string());
            }
        }
        filter_drivers.sort();
        filter_drivers.dedup();
        Ok(Self { filter_drivers })
    }
}

fn hash_record(hasher: &mut Sha256, tag: &[u8], bytes: &[u8]) {
    hasher.update(b"weaver-change-record-v1");
    hasher.update((tag.len() as u64).to_be_bytes());
    hasher.update(tag);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
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

fn validate_path(raw: &[u8]) -> Result<()> {
    if raw.is_empty() || raw.contains(&0) {
        bail!("change path is empty or contains NUL");
    }
    let relative = Path::new(OsStr::from_bytes(raw));
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
    Ok(())
}

fn safe_file(work_dir: &Path, raw: &[u8]) -> Result<Option<(std::fs::File, std::fs::Metadata)>> {
    validate_path(raw)?;
    let mut directory = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(work_dir)
    {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let mut components = Path::new(OsStr::from_bytes(raw))
        .components()
        .filter_map(|part| match part {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .peekable();
    while let Some(component) = components.next() {
        let component = CString::new(component.as_bytes())?;
        let leaf = components.peek().is_none();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if leaf {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: the owned directory descriptor and CString remain valid.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return Ok(None);
        }
        // SAFETY: a successful openat returns a new owned descriptor.
        let opened = unsafe { std::fs::File::from_raw_fd(fd) };
        if leaf {
            let metadata = opened.metadata()?;
            return Ok(metadata.is_file().then_some((opened, metadata)));
        }
        directory = opened;
    }
    Ok(None)
}

fn metadata_identity(metadata: &std::fs::Metadata) -> Vec<u8> {
    let fields = [
        metadata.dev(),
        metadata.ino(),
        metadata.mode() as u64,
        metadata.len(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ];
    fields
        .iter()
        .flat_map(|field| field.to_be_bytes())
        .collect()
}

fn same_metadata(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    metadata_identity(left) == metadata_identity(right)
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

enum HashPathResult {
    Complete(Option<u32>),
    Incomplete,
}

async fn hash_path(
    work_dir: &Path,
    raw: &[u8],
    hasher: &mut Sha256,
    remaining: &mut u64,
) -> Result<HashPathResult> {
    hash_record(hasher, b"path", raw);
    match safe_file(work_dir, raw)? {
        None => Ok(HashPathResult::Incomplete),
        Some((file, initial)) => {
            if initial.len() > *remaining {
                return Ok(HashPathResult::Incomplete);
            }
            *remaining -= initial.len();
            hash_record(hasher, b"kind", b"file");
            hash_record(hasher, b"file-stat", &metadata_identity(&initial));
            hash_record(hasher, b"content-length", &initial.len().to_be_bytes());

            let mut file = tokio::fs::File::from_std(file);
            let mut chunk = [0_u8; 32 * 1024];
            let mut left = initial.len();
            let mut lines = 0_u64;
            let mut last = 0_u8;
            let mut binary = false;
            while left > 0 {
                let wanted = left.min(chunk.len() as u64) as usize;
                if file.read_exact(&mut chunk[..wanted]).await.is_err() {
                    return Ok(HashPathResult::Incomplete);
                }
                hash_record(hasher, b"file-chunk", &chunk[..wanted]);
                lines += chunk[..wanted]
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count() as u64;
                binary |= chunk[..wanted].contains(&0);
                last = chunk[wanted - 1];
                left -= wanted as u64;
            }
            let mut extra = [0_u8; 1];
            if file.read(&mut extra).await? != 0 {
                return Ok(HashPathResult::Incomplete);
            }
            let final_metadata = file.metadata().await?;
            if !same_metadata(&initial, &final_metadata) {
                return Ok(HashPathResult::Incomplete);
            }
            if initial.len() > 0 && last != b'\n' {
                lines += 1;
            }
            Ok(HashPathResult::Complete(
                (!binary).then_some(lines.min(u32::MAX as u64) as u32),
            ))
        }
    }
}

async fn untracked_file(
    work_dir: &Path,
    raw: &[u8],
    sources: Vec<ChangeSourceDto>,
    remaining_total: &mut usize,
) -> Result<ChangeFileDto> {
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
    let (handle, metadata) = match safe_file(work_dir, raw)? {
        Some(file) => file,
        None => return Ok(file),
    };
    if metadata.len() > MAX_UNTRACKED_RENDER_BYTES {
        file.content = ChangeContentDto::Oversize;
        file.truncated = true;
        return Ok(file);
    }
    let mut handle = tokio::fs::File::from_std(handle);
    let mut bytes = vec![0_u8; metadata.len() as usize];
    if handle.read_exact(&mut bytes).await.is_err() {
        file.truncated = true;
        return Ok(file);
    }
    let mut extra = [0_u8; 1];
    if handle.read(&mut extra).await? != 0 || !same_metadata(&metadata, &handle.metadata().await?) {
        file.truncated = true;
        return Ok(file);
    }
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

async fn hash_index(path: &Path, hasher: &mut Sha256) -> Result<bool> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            hash_record(hasher, b"index-state", b"absent");
            return Ok(true);
        }
        Err(_) => return Ok(false),
    };
    let initial = file.metadata()?;
    if !initial.is_file() || initial.len() > MAX_INDEX_BYTES {
        return Ok(false);
    }
    let mut file = tokio::fs::File::from_std(file);
    let mut bytes = vec![0_u8; initial.len() as usize];
    if file.read_exact(&mut bytes).await.is_err() {
        return Ok(false);
    }
    let mut extra = [0_u8; 1];
    if file.read(&mut extra).await? != 0 || !same_metadata(&initial, &file.metadata().await?) {
        return Ok(false);
    }
    hash_record(hasher, b"index-stat", &metadata_identity(&initial));
    hash_record(hasher, b"index-bytes", &bytes);
    Ok(true)
}

struct StateCapture {
    final_raw: Capture,
    numstat: Capture,
    committed: Capture,
    staged: Capture,
    unstaged: Capture,
    untracked_paths: Vec<Vec<u8>>,
    untracked_additions: u32,
    identity: Option<[u8; 32]>,
}

async fn capture_state(
    work_dir: &Path,
    policy: &GitReadPolicy,
    base_reference: &str,
    base_oid: &str,
    head_oid: &str,
    index_path: &Path,
) -> Result<StateCapture> {
    macro_rules! diff {
        ($args:expr) => {
            async { capture_diff(work_dir, policy, $args, MAX_INVENTORY_BYTES).await }
        };
    }
    let (final_raw, numstat, committed, staged, unstaged, untracked) = tokio::try_join!(
        diff!(&[
            "--raw",
            "--full-index",
            "--find-renames",
            "-z",
            base_oid,
            "--"
        ]),
        diff!(&["--numstat", "--find-renames", "-z", base_oid, "--"]),
        diff!(&[
            "--name-only",
            "--find-renames",
            "-z",
            base_oid,
            head_oid,
            "--"
        ]),
        diff!(&[
            "--cached",
            "--name-only",
            "--find-renames",
            "-z",
            head_oid,
            "--"
        ]),
        diff!(&["--name-only", "--find-renames", "-z", "--"]),
        capture_git(
            work_dir,
            policy,
            &["ls-files", "--others", "--exclude-standard", "-z", "--"],
            MAX_INVENTORY_BYTES,
        ),
    )?;
    let truncated = [
        &final_raw, &numstat, &committed, &staged, &unstaged, &untracked,
    ]
    .iter()
    .any(|capture| capture.truncated);
    let untracked_paths: Vec<Vec<u8>> = nul_records(&untracked.bytes)
        .map(ToOwned::to_owned)
        .collect();
    let mut untracked_additions = 0;
    let mut identity = None;
    if !truncated {
        let mut hasher = Sha256::new();
        hash_record(&mut hasher, b"schema", b"changes-state-v2");
        hash_record(&mut hasher, b"base-reference", base_reference.as_bytes());
        hash_record(&mut hasher, b"base-oid", base_oid.as_bytes());
        hash_record(&mut hasher, b"head-oid", head_oid.as_bytes());
        for (tag, capture) in [
            (b"final-raw".as_slice(), &final_raw),
            (b"numstat".as_slice(), &numstat),
            (b"committed".as_slice(), &committed),
            (b"staged".as_slice(), &staged),
            (b"unstaged".as_slice(), &unstaged),
            (b"untracked".as_slice(), &untracked),
        ] {
            hash_record(&mut hasher, tag, &capture.bytes);
        }
        let mut complete = hash_index(index_path, &mut hasher).await?;
        let mut remaining = MAX_IDENTITY_BYTES;
        let raw_files = parse_raw_files(&final_raw.bytes);
        for file in raw_files
            .iter()
            .filter(|file| file.status != ChangeFileStatusDto::Deleted)
        {
            match hash_path(work_dir, &file.path, &mut hasher, &mut remaining).await? {
                HashPathResult::Complete(_) => {}
                HashPathResult::Incomplete => complete = false,
            }
        }
        for path in &untracked_paths {
            match hash_path(work_dir, path, &mut hasher, &mut remaining).await? {
                HashPathResult::Complete(Some(lines)) => {
                    untracked_additions += lines;
                }
                HashPathResult::Complete(None) => {}
                HashPathResult::Incomplete => complete = false,
            }
        }
        if complete {
            identity = Some(hasher.finalize().into());
        }
    }
    Ok(StateCapture {
        final_raw,
        numstat,
        committed,
        staged,
        unstaged,
        untracked_paths,
        untracked_additions,
        identity,
    })
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

async fn load_once(
    work_dir: &Path,
    policy: &GitReadPolicy,
    base_reference: &str,
) -> Result<(ChangeSetDto, bool)> {
    let Some(head_oid) = git_text(work_dir, policy, &["rev-parse", "--verify", "HEAD"]).await?
    else {
        return Ok((
            unavailable(
                base_reference,
                ChangeBaseUnavailableReasonDto::UnbornHead,
                None,
            ),
            true,
        ));
    };
    let local_base = if base_reference.starts_with("refs/heads/") {
        base_reference.to_string()
    } else {
        format!("refs/heads/{base_reference}")
    };
    let base_spec = format!("{local_base}^{{commit}}");
    let Some(base_tip) = git_text(work_dir, policy, &["rev-parse", "--verify", &base_spec]).await?
    else {
        return Ok((
            unavailable(
                base_reference,
                ChangeBaseUnavailableReasonDto::MissingBase,
                Some(head_oid),
            ),
            true,
        ));
    };
    let Some(base_oid) = git_text(work_dir, policy, &["merge-base", &head_oid, &base_tip]).await?
    else {
        return Ok((
            unavailable(
                base_reference,
                ChangeBaseUnavailableReasonDto::NoMergeBase,
                Some(head_oid),
            ),
            true,
        ));
    };
    let index_path = git_text(work_dir, policy, &["rev-parse", "--git-path", "index"])
        .await?
        .map(PathBuf::from)
        .context("git index path is unavailable")?;
    let index_path = if index_path.is_absolute() {
        index_path
    } else {
        work_dir.join(index_path)
    };
    let state = capture_state(
        work_dir,
        policy,
        base_reference,
        &base_oid,
        &head_oid,
        &index_path,
    )
    .await?;
    let mut raw_files = parse_raw_files(&state.final_raw.bytes);

    let patch = capture_diff(
        work_dir,
        policy,
        &["--patch", "--find-renames", "--unified=3", &base_oid, "--"],
        MAX_PATCH_BYTES,
    )
    .await?;
    let mut sections = patch_sections(&patch.bytes);
    let stats = parse_numstat(&state.numstat.bytes);
    let committed = source_paths(&state.committed.bytes);
    let staged = source_paths(&state.staged.bytes);
    let unstaged = source_paths(&state.unstaged.bytes);
    let untracked_set: BTreeSet<Vec<u8>> = state.untracked_paths.iter().cloned().collect();

    for path in &state.untracked_paths {
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
            .chain(std::iter::once(state.untracked_additions))
            .sum(),
        deletions: stats.values().filter_map(|stat| stat.deletions).sum(),
        truncated: state.identity.is_none(),
    };
    let mut changes = ChangeSetDto {
        version: None,
        base: ChangeBaseDto::Available {
            reference: base_reference.to_string(),
            oid: base_oid.clone(),
        },
        head_oid: Some(head_oid.clone()),
        totals,
        truncated: state.identity.is_none()
            || patch.truncated
            || raw_files.len() > MAX_FILES
            || remaining_total == 0,
        files,
        limits: limits(),
    };
    let Some(identity) = state.identity.filter(|_| !patch.timed_out) else {
        return Ok((changes, true));
    };

    let post = capture_state(
        work_dir,
        policy,
        base_reference,
        &base_oid,
        &head_oid,
        &index_path,
    )
    .await?;
    let head_after = git_text(work_dir, policy, &["rev-parse", "--verify", "HEAD"]).await?;
    let base_after = git_text(work_dir, policy, &["rev-parse", "--verify", &base_spec]).await?;
    let stable = post.identity == Some(identity)
        && head_after.as_deref() == Some(head_oid.as_str())
        && base_after.as_deref() == Some(base_tip.as_str());
    if !stable {
        changes.truncated = true;
        changes.totals.truncated = true;
        return Ok((changes, false));
    }

    let rendered = serde_json::to_vec(&changes).context("serializing stable change response")?;
    let mut hasher = Sha256::new();
    hash_record(&mut hasher, b"state-identity", &identity);
    hash_record(&mut hasher, b"rendered-response", &rendered);
    changes.version = Some(format!("changes-v1:{}", hex::encode(hasher.finalize())));
    Ok((changes, true))
}

/// Read one session worktree without fetching or changing Git state.
pub async fn load(work_dir: &Path, base_reference: &str) -> Result<ChangeSetDto> {
    let policy = GitReadPolicy::load(work_dir).await?;
    for attempt in 0..2 {
        let (mut changes, stable) = load_once(work_dir, &policy, base_reference).await?;
        if stable {
            return Ok(changes);
        }
        if attempt == 1 {
            changes.version = None;
            changes.truncated = true;
            changes.totals.truncated = true;
            return Ok(changes);
        }
    }
    unreachable!("bounded change read always returns or retries once")
}

/// Validate a mutable comment anchor against the exact snapshot on screen.
pub fn validate_anchor(
    changes: &ChangeSetDto,
    version: &str,
    anchor: &ChangeAnchorDto,
) -> Result<ChangeAnchorDto> {
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
    let count = (anchor.end_line - anchor.start_line + 1) as usize;
    for hunk in &file.hunks {
        let eligible: Vec<_> = hunk
            .lines
            .iter()
            .filter(|line| line_number(line, anchor.side).is_some())
            .collect();
        let Some(first) = eligible
            .iter()
            .position(|line| line_number(line, anchor.side) == Some(anchor.start_line))
        else {
            continue;
        };
        let Some(selected) = eligible.get(first..first.saturating_add(count)) else {
            continue;
        };
        let contiguous = selected.iter().enumerate().all(|(offset, line)| {
            let expected = anchor.start_line.checked_add(offset as u32);
            line_number(line, anchor.side) == expected
        });
        if contiguous {
            return Ok(ChangeAnchorDto {
                path: file.path.clone(),
                side: anchor.side,
                start_line: anchor.start_line,
                end_line: anchor.end_line,
                hunk_header: hunk.header.clone(),
                context_before: eligible[first.saturating_sub(2)..first]
                    .iter()
                    .map(|line| line.text.clone())
                    .collect(),
                selected: selected.iter().map(|line| line.text.clone()).collect(),
                context_after: eligible[first + count..]
                    .iter()
                    .take(2)
                    .map(|line| line.text.clone())
                    .collect(),
            });
        }
    }
    bail!("change anchor does not name a contiguous visible line range")
}

fn line_number(line: &ChangeLineDto, side: ChangeSideDto) -> Option<u32> {
    match side {
        ChangeSideDto::Old => line.old_line,
        ChangeSideDto::New => line.new_line,
    }
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
            path: ChangePathDto {
                bytes: path.bytes,
                display: "spoofed.rs".to_string(),
            },
            side: ChangeSideDto::New,
            start_line: 3,
            end_line: 4,
            hunk_header: "@@ spoofed @@".to_string(),
            context_before: vec!["spoofed before".to_string()],
            selected: vec!["spoofed selection".to_string()],
            context_after: vec!["spoofed after".to_string()],
        };
        let canonical = validate_anchor(&changes, "changes-v1:test", &anchor).unwrap();
        assert_eq!(canonical.path.display, "src/a.rs");
        assert_eq!(canonical.hunk_header, "@@ -2,2 +2,3 @@ fn main() {");
        assert_eq!(canonical.context_before, vec!["same"]);
        assert_eq!(canonical.selected, vec!["new", "next"]);
        assert!(canonical.context_after.is_empty());
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

    #[test]
    fn framed_identity_separates_cross_file_boundaries() {
        fn legacy(files: &[(&[u8], &[u8])]) -> Vec<u8> {
            let mut bytes = Vec::new();
            for (path, content) in files {
                bytes.extend_from_slice(path);
                bytes.extend_from_slice(b"file\0");
                bytes.extend_from_slice(content);
            }
            bytes
        }

        fn framed(files: &[(&[u8], &[u8])]) -> Vec<u8> {
            let mut hasher = Sha256::new();
            for (path, content) in files {
                hash_record(&mut hasher, b"path", path);
                hash_record(&mut hasher, b"kind", b"file");
                hash_record(
                    &mut hasher,
                    b"content-length",
                    &(content.len() as u64).to_be_bytes(),
                );
                hash_record(&mut hasher, b"file-chunk", content);
            }
            hasher.finalize().to_vec()
        }

        let combined = b"xbfile\0y";
        let split = [(b"a".as_slice(), b"x".as_slice()), (b"b", b"y")];
        let joined = [(b"a".as_slice(), combined.as_slice())];
        assert_eq!(legacy(&split), legacy(&joined));
        assert_ne!(framed(&split), framed(&joined));
    }
}
