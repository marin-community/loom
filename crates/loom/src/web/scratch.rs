use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use weaver_api::{ScratchLimitsView, ScratchUpload};

use super::require_session;
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Scratch files — drag-and-drop reference material dropped into the worktree's
// `scratch/` directory so the agent can read it (e.g. "see scratch/error.log").
// ---------------------------------------------------------------------------

pub(crate) const MAX_SCRATCH_FILES: usize = 20;
pub(crate) const MAX_SCRATCH_FILE_BYTES: usize = 25 * 1024 * 1024;
pub(crate) const MAX_SCRATCH_TOTAL_BYTES: usize = 50 * 1024 * 1024;
/// Portable single-component bound below common 255-byte filesystem limits.
pub(crate) const MAX_SCRATCH_NAME_BYTES: usize = 240;
/// JSON base64 expands a valid 50 MiB batch by roughly one third. Keep this
/// route envelope explicit and comfortably above that encoded payload while
/// retaining the smaller protected-router default for unrelated endpoints.
pub(crate) const MAX_SESSION_CREATE_BODY_BYTES: usize = 72 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct ScratchQuery {
    name: String,
}

/// Validate a client-supplied scratch file name: a single path component, no
/// separators, no `.`/`..`. Returns the bare name on success.
fn scratch_name(raw: &str) -> ApiResult<String> {
    if raw.is_empty() {
        return Err(AppError::bad_request("file name is required"));
    }
    if raw != raw.trim()
        || raw == "."
        || raw == ".."
        || raw.len() > MAX_SCRATCH_NAME_BYTES
        || raw.contains(['/', '\\'])
        || raw.chars().any(char::is_control)
    {
        return Err(AppError::bad_request(
            format!(
                "file name must be one control-free path component of at most {MAX_SCRATCH_NAME_BYTES} UTF-8 bytes"
            ),
        ));
    }
    if raw == ".gitignore" {
        return Err(AppError::bad_request(
            "'.gitignore' is reserved for Scratch housekeeping",
        ));
    }
    Ok(raw.to_string())
}

pub(super) async fn scratch_limits() -> Json<ScratchLimitsView> {
    Json(ScratchLimitsView {
        max_files: MAX_SCRATCH_FILES,
        max_file_bytes: MAX_SCRATCH_FILE_BYTES,
        max_total_bytes: MAX_SCRATCH_TOTAL_BYTES,
        max_name_bytes: MAX_SCRATCH_NAME_BYTES,
    })
}

fn validate_scratch_size(name: &str, bytes: usize) -> ApiResult<()> {
    if bytes > MAX_SCRATCH_FILE_BYTES {
        return Err(AppError::bad_request(format!(
            "scratch file '{name}' is larger than the {} byte limit",
            MAX_SCRATCH_FILE_BYTES
        )));
    }
    Ok(())
}

/// A fully decoded and validated launch-time batch. Construct this before any
/// repository, branch, issue, or session side effect; writing it later cannot
/// discover malformed client input.
pub(crate) struct PreparedScratch {
    files: std::collections::BTreeMap<String, Vec<u8>>,
}

async fn scratch_inventory(
    dir: &std::path::Path,
) -> ApiResult<std::collections::BTreeMap<String, usize>> {
    let mut files = std::collections::BTreeMap::new();
    match tokio::fs::read_dir(dir).await {
        Ok(mut entries) => {
            while let Some(entry) = entries.next_entry().await? {
                let entry_name = entry.file_name();
                let entry_name = entry_name.to_str().ok_or_else(|| {
                    AppError::bad_request("Scratch contains a non-UTF-8 file name")
                })?;
                if entry_name == ".gitignore" {
                    continue;
                }
                let file_type = entry.file_type().await?;
                if file_type.is_symlink() {
                    return Err(AppError::bad_request(format!(
                        "scratch entry '{entry_name}' must be a regular file"
                    )));
                }
                if file_type.is_file() {
                    let metadata = entry.metadata().await?;
                    scratch_name(entry_name)?;
                    let bytes = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
                    validate_scratch_size(entry_name, bytes)?;
                    files.insert(entry_name.to_string(), bytes);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(files)
}

pub(crate) fn prepare_initial_scratch(files: &[ScratchUpload]) -> ApiResult<PreparedScratch> {
    if files.is_empty() {
        return Ok(PreparedScratch {
            files: std::collections::BTreeMap::new(),
        });
    }
    let mut decoded = std::collections::BTreeMap::<String, Vec<u8>>::new();
    for file in files {
        let name = scratch_name(&file.name)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(file.content_base64.trim())
            .map_err(|error| {
                AppError::bad_request(format!("scratch file '{name}': invalid base64: {error}"))
            })?;
        validate_scratch_size(&name, bytes.len())?;
        decoded.insert(name, bytes);
    }
    if decoded.len() > MAX_SCRATCH_FILES {
        return Err(AppError::bad_request(format!(
            "a launch may attach at most {MAX_SCRATCH_FILES} scratch files"
        )));
    }
    let total: usize = decoded.values().map(Vec::len).sum();
    if total > MAX_SCRATCH_TOTAL_BYTES {
        return Err(AppError::bad_request(format!(
            "launch scratch files exceed the {} byte total limit",
            MAX_SCRATCH_TOTAL_BYTES
        )));
    }

    Ok(PreparedScratch { files: decoded })
}

/// Write a batch that has already passed [`prepare_initial_scratch`] into
/// `<work_dir>/scratch/`, returning sorted, de-duplicated bare names.
pub(crate) async fn write_prepared_initial_scratch(
    work_dir: &std::path::Path,
    prepared: &PreparedScratch,
) -> ApiResult<Vec<String>> {
    let dir = work_dir.join("scratch");
    let mut merged = scratch_inventory(&dir).await?;
    for (name, bytes) in &prepared.files {
        merged.insert(name.clone(), bytes.len());
    }
    if merged.len() > MAX_SCRATCH_FILES {
        return Err(AppError::bad_request(format!(
            "session scratch files exceed the {MAX_SCRATCH_FILES} file limit"
        )));
    }
    let total = merged.values().copied().fold(0usize, usize::saturating_add);
    if total > MAX_SCRATCH_TOTAL_BYTES {
        return Err(AppError::bad_request(format!(
            "session scratch files exceed the {} byte total limit",
            MAX_SCRATCH_TOTAL_BYTES
        )));
    }
    if prepared.files.is_empty() {
        return Ok(merged.into_keys().collect());
    }
    tokio::fs::create_dir_all(&dir).await?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        tokio::fs::write(&gitignore, "*\n").await?;
    }
    for (name, bytes) in &prepared.files {
        tokio::fs::write(dir.join(name), bytes).await?;
    }
    let names: Vec<String> = merged.into_keys().collect();
    tracing::info!(files = ?names, "scratch files written");
    Ok(names)
}

#[cfg(test)]
async fn write_initial_scratch(
    work_dir: &std::path::Path,
    files: &[ScratchUpload],
) -> ApiResult<Vec<String>> {
    let prepared = prepare_initial_scratch(files)?;
    write_prepared_initial_scratch(work_dir, &prepared).await
}

/// A sentence telling the agent about its launch-time scratch files, or `None`
/// when none were attached. Appended to the launch prompt so a fresh agent
/// knows the reference material exists without the user having to mention it.
pub(crate) fn scratch_note(names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let list = names
        .iter()
        .map(|n| format!("scratch/{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "Reference files have been attached for this task in the `scratch/` \
         directory of your worktree (it is kept out of git): {list}. \
         Read them as needed."
    ))
}

pub(super) async fn list_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let dir = PathBuf::from(&session.work_dir).join("scratch");
    let mut out: Vec<Value> = Vec::new();
    match tokio::fs::read_dir(&dir).await {
        Ok(mut rd) => {
            while let Some(entry) = rd.next_entry().await? {
                let meta = entry.metadata().await?;
                if !meta.is_file() {
                    continue;
                }
                if let Some(name) = entry.file_name().to_str() {
                    // The server-owned exclusion guard is housekeeping. Other
                    // dotfiles are user attachments and remain visible/countable.
                    if name == ".gitignore" {
                        continue;
                    }
                    out.push(json!({ "name": name, "bytes": meta.len() }));
                }
            }
        }
        // No scratch directory yet just means nothing has been dropped.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    out.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    Ok(Json(out))
}

pub(super) async fn upload_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<ScratchQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let (session, _) = require_session(&st.db, &key).await?;
    let name = scratch_name(&q.name)?;
    validate_scratch_size(&name, body.len())?;
    let work_dir = PathBuf::from(&session.work_dir);
    let _permit = st.launch_gate.acquire_scratch(&work_dir).await;
    let dir = work_dir.join("scratch");
    let mut inventory = scratch_inventory(&dir).await?;
    inventory.insert(name.clone(), body.len());
    if inventory.len() > MAX_SCRATCH_FILES {
        return Err(AppError::bad_request(format!(
            "a session may attach at most {MAX_SCRATCH_FILES} scratch files"
        )));
    }
    let total = inventory
        .values()
        .copied()
        .fold(0usize, usize::saturating_add);
    if total > MAX_SCRATCH_TOTAL_BYTES {
        return Err(AppError::bad_request(format!(
            "session scratch files exceed the {} byte total limit",
            MAX_SCRATCH_TOTAL_BYTES
        )));
    }
    tokio::fs::create_dir_all(&dir).await?;
    // Reference material isn't meant to be committed; keep the whole directory
    // out of git so it never shows up in the agent's diff.
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        tokio::fs::write(&gitignore, "*\n").await?;
    }
    tokio::fs::write(dir.join(&name), &body).await?;
    tracing::info!(session = %session.id, file = %name, bytes = body.len(), "scratch file written");
    Ok(Json(json!({
        "name": name,
        "bytes": body.len(),
        "path": format!("scratch/{name}"),
    })))
}

pub(super) async fn delete_scratch(
    State(st): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<ScratchQuery>,
) -> ApiResult<StatusCode> {
    let (session, _) = require_session(&st.db, &key).await?;
    let name = scratch_name(&q.name)?;
    let work_dir = PathBuf::from(&session.work_dir);
    let _permit = st.launch_gate.acquire_scratch(&work_dir).await;
    let path = work_dir.join("scratch").join(&name);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            tracing::info!(session = %session.id, file = %name, "scratch file deleted");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::not_found("scratch file"))
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn scratch_note_lists_files_or_is_empty() {
        assert!(scratch_note(&[]).is_none());
        let note = scratch_note(&["error.log".into(), "design.png".into()]).unwrap();
        assert!(note.contains("scratch/error.log"));
        assert!(note.contains("scratch/design.png"));
        // Mentions the directory so the agent knows where to look.
        assert!(note.contains("scratch/"));
    }

    #[tokio::test]
    async fn write_initial_scratch_drops_files_and_gitignores() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![
            ScratchUpload {
                name: "notes.txt".into(),
                content_base64: b64("hello scratch"),
            },
            ScratchUpload {
                name: "trace.log".into(),
                content_base64: b64("panic"),
            },
        ];
        let names = write_initial_scratch(dir.path(), &files).await.unwrap();
        assert_eq!(
            names,
            vec!["notes.txt".to_string(), "trace.log".to_string()]
        );

        let scratch = dir.path().join("scratch");
        assert_eq!(
            std::fs::read_to_string(scratch.join("notes.txt")).unwrap(),
            "hello scratch"
        );
        // The directory is kept out of git so reference material never enters
        // the agent's diff.
        assert_eq!(
            std::fs::read_to_string(scratch.join(".gitignore")).unwrap(),
            "*\n"
        );
    }

    #[tokio::test]
    async fn write_initial_scratch_rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        // A path-traversal name is refused (same rule as the upload endpoint).
        let bad_name = vec![ScratchUpload {
            name: "../escape".into(),
            content_base64: b64("x"),
        }];
        assert!(write_initial_scratch(dir.path(), &bad_name).await.is_err());
        let housekeeping = vec![ScratchUpload {
            name: ".gitignore".into(),
            content_base64: b64("not an exclusion guard"),
        }];
        assert!(write_initial_scratch(dir.path(), &housekeeping)
            .await
            .is_err());
        for name in [
            "nul\0name.txt".to_string(),
            format!("{}.txt", "x".repeat(MAX_SCRATCH_NAME_BYTES + 1)),
        ] {
            assert!(write_initial_scratch(
                dir.path(),
                &[ScratchUpload {
                    name,
                    content_base64: b64("x"),
                }],
            )
            .await
            .is_err());
        }
        // Malformed base64 is refused — a launch shouldn't half-write garbage.
        let bad_b64 = vec![
            ScratchUpload {
                name: "would-have-been-written.txt".into(),
                content_base64: b64("valid"),
            },
            ScratchUpload {
                name: "bad.txt".into(),
                content_base64: "not!base64!".into(),
            },
        ];
        assert!(write_initial_scratch(dir.path(), &bad_b64).await.is_err());
        assert!(
            !dir.path().join("scratch").exists(),
            "the batch is validated before any attachment is written"
        );
        // Nothing to do for an empty batch.
        assert!(write_initial_scratch(dir.path(), &[])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn initial_scratch_validates_the_merged_existing_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        tokio::fs::create_dir_all(&scratch).await.unwrap();
        tokio::fs::write(scratch.join(".gitignore"), "*\n")
            .await
            .unwrap();
        for index in 0..19 {
            tokio::fs::write(scratch.join(format!("seed-{index:02}.txt")), b"x")
                .await
                .unwrap();
        }
        let overflow = vec![
            ScratchUpload {
                name: "new-a.txt".into(),
                content_base64: b64("a"),
            },
            ScratchUpload {
                name: "new-b.txt".into(),
                content_base64: b64("b"),
            },
        ];
        assert!(write_initial_scratch(dir.path(), &overflow).await.is_err());
        assert!(!scratch.join("new-a.txt").exists());
        assert!(!scratch.join("new-b.txt").exists());

        let replacement = vec![
            ScratchUpload {
                name: "seed-00.txt".into(),
                content_base64: b64("replacement"),
            },
            ScratchUpload {
                name: "new.txt".into(),
                content_base64: b64("new"),
            },
        ];
        let merged = write_initial_scratch(dir.path(), &replacement)
            .await
            .unwrap();
        assert_eq!(merged.len(), MAX_SCRATCH_FILES);
        assert_eq!(
            tokio::fs::read_to_string(scratch.join("seed-00.txt"))
                .await
                .unwrap(),
            "replacement"
        );
        assert_eq!(
            tokio::fs::read_to_string(scratch.join("new.txt"))
                .await
                .unwrap(),
            "new"
        );

        let bytes_dir = tempfile::tempdir().unwrap();
        let bytes_scratch = bytes_dir.path().join("scratch");
        tokio::fs::create_dir_all(&bytes_scratch).await.unwrap();
        for name in ["first.bin", "second.bin"] {
            let file = tokio::fs::File::create(bytes_scratch.join(name))
                .await
                .unwrap();
            file.set_len(MAX_SCRATCH_FILE_BYTES as u64).await.unwrap();
        }
        let one_more = PreparedScratch {
            files: std::collections::BTreeMap::from([("overflow.bin".to_string(), vec![1])]),
        };
        assert!(
            write_prepared_initial_scratch(bytes_dir.path(), &one_more)
                .await
                .is_err(),
            "existing bytes participate in the merged total"
        );
        assert!(!bytes_scratch.join("overflow.bin").exists());
    }
}
