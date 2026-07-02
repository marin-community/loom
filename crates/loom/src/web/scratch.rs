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
use weaver_api::ScratchUpload;

use super::require_session;
use super::{ApiResult, AppError, AppState};

// ---------------------------------------------------------------------------
// Scratch files — drag-and-drop reference material dropped into the worktree's
// `scratch/` directory so the agent can read it (e.g. "see scratch/error.log").
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct ScratchQuery {
    name: String,
}

/// Validate a client-supplied scratch file name: a single path component, no
/// separators, no `.`/`..`. Returns the bare name on success.
fn scratch_name(raw: &str) -> ApiResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::bad_request("file name is required"));
    }
    let name = std::path::Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name != trimmed || name == "." || name == ".." {
        return Err(AppError::bad_request(
            "file name must be a single path component",
        ));
    }
    Ok(name.to_string())
}

/// Write launch-time scratch files into `<work_dir>/scratch/`, returning the
/// bare names written (sorted, de-duplicated). The directory is git-ignored
/// exactly as [`upload_scratch`] does it, so reference material never enters
/// the agent's diff. The whole batch is rejected if any name or body is
/// malformed — a launch shouldn't half-succeed.
pub(crate) async fn write_initial_scratch(
    work_dir: &std::path::Path,
    files: &[ScratchUpload],
) -> ApiResult<Vec<String>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let dir = work_dir.join("scratch");
    tokio::fs::create_dir_all(&dir).await?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        tokio::fs::write(&gitignore, "*\n").await?;
    }
    let mut names: Vec<String> = Vec::with_capacity(files.len());
    for f in files {
        let name = scratch_name(&f.name)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(f.content_base64.trim())
            .map_err(|e| {
                AppError::bad_request(format!("scratch file '{name}': invalid base64: {e}"))
            })?;
        tokio::fs::write(dir.join(&name), &bytes).await?;
        names.push(name);
    }
    names.sort();
    names.dedup();
    tracing::info!(files = ?names, "scratch files written");
    Ok(names)
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
                    // Hide housekeeping dotfiles (e.g. the .gitignore we write).
                    if name.starts_with('.') {
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
    let dir = PathBuf::from(&session.work_dir).join("scratch");
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
    let path = PathBuf::from(&session.work_dir).join("scratch").join(&name);
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
        // Malformed base64 is refused — a launch shouldn't half-write garbage.
        let bad_b64 = vec![ScratchUpload {
            name: "ok.txt".into(),
            content_base64: "not!base64!".into(),
        }];
        assert!(write_initial_scratch(dir.path(), &bad_b64).await.is_err());
        // Nothing to do for an empty batch.
        assert!(write_initial_scratch(dir.path(), &[])
            .await
            .unwrap()
            .is_empty());
    }
}
