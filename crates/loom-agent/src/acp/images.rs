use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use tokio::io::AsyncReadExt;

use super::wire::{ContentBlock, ToolCallContent};

const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

// Codex sends viewed images as resource links rather than image bytes. Capture
// them at completion because screenshots in temporary directories are ephemeral.
pub(super) async fn capture_linked_images(content: &mut [ToolCallContent], work_dir: &Path) {
    for item in content {
        let ToolCallContent::Content {
            content: ContentBlock::ResourceLink { uri, mime_type },
        } = item
        else {
            continue;
        };
        let extension = Path::new(uri)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !mime_type
            .as_deref()
            .is_some_and(|mime| mime.starts_with("image/"))
            && !matches!(
                extension.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif"
            )
        {
            continue;
        }
        let image = match snapshot_image(uri, work_dir).await {
            Ok(image) => image,
            Err(error) => ContentBlock::Text {
                text: format!("Image preview unavailable for {uri}: {error}"),
            },
        };
        *item = ToolCallContent::Content { content: image };
    }
}

async fn snapshot_image(uri: &str, work_dir: &Path) -> Result<ContentBlock> {
    let path = if let Some(encoded) = uri.strip_prefix("file:///") {
        PathBuf::from(format!(
            "/{}",
            percent_encoding::percent_decode_str(encoded).decode_utf8()?
        ))
    } else {
        ensure!(
            !uri.contains("://"),
            "only local image files can be previewed"
        );
        work_dir.join(uri)
    };
    let path = tokio::fs::canonicalize(path)
        .await
        .context("image file is no longer available")?;
    let root = tokio::fs::canonicalize(work_dir).await?;
    let temp = tokio::fs::canonicalize(std::env::temp_dir()).await?;
    if !path.starts_with(root) && !path.starts_with(temp) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "image is outside the session worktree and temporary directory",
        )
        .into());
    }
    let metadata = tokio::fs::metadata(&path).await?;
    ensure!(metadata.is_file(), "image is not a regular file");
    ensure!(
        metadata.len() <= MAX_IMAGE_BYTES,
        "image exceeds the 10 MiB preview limit"
    );
    let mut bytes = Vec::new();
    tokio::fs::File::open(&path)
        .await?
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        bytes.len() as u64 <= MAX_IMAGE_BYTES,
        "image exceeds the 10 MiB preview limit"
    );
    let mime_type = image_mime_type(&bytes).context("unsupported image format")?;
    Ok(ContentBlock::Image {
        data: STANDARD.encode(bytes),
        mime_type: mime_type.to_string(),
        uri: Some(uri.to_string()),
    })
}

fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some("image/webp");
    }
    if matches!(bytes.get(4..12), Some(b"ftypavif" | b"ftypavis")) {
        return Some("image/avif");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[tokio::test]
    async fn captures_relative_absolute_and_file_resource_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("screen shot.png");
        tokio::fs::write(&path, STANDARD.decode(PNG).unwrap())
            .await
            .unwrap();
        for uri in [
            path.to_string_lossy().into_owned(),
            format!("file://{}", path.to_string_lossy().replace(' ', "%20")),
            "screen shot.png".to_string(),
        ] {
            let mut content: Vec<ToolCallContent> = serde_json::from_value(serde_json::json!([{
                "type": "content",
                "content": { "type": "resource_link", "uri": uri, "name": "screenshot" }
            }]))
            .unwrap();
            capture_linked_images(&mut content, dir.path()).await;
            let ToolCallContent::Content {
                content:
                    ContentBlock::Image {
                        data, mime_type, ..
                    },
            } = &content[0]
            else {
                panic!("expected captured image: {:?}", content[0]);
            };
            assert_eq!(data, PNG);
            assert_eq!(mime_type, "image/png");
        }
    }

    #[tokio::test]
    async fn rejects_missing_oversized_and_non_image_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(snapshot_image("missing.png", dir.path()).await.is_err());
        tokio::fs::write(dir.path().join("text.png"), b"not an image")
            .await
            .unwrap();
        assert!(snapshot_image("text.png", dir.path()).await.is_err());
        let path = dir.path().join("large.png");
        tokio::fs::File::create(&path)
            .await
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .await
            .unwrap();
        assert!(snapshot_image(path.to_str().unwrap(), dir.path())
            .await
            .is_err());
        assert!(snapshot_image("https://example.com/image.png", dir.path())
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlinks_outside_the_preview_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escape.png");
        std::os::unix::fs::symlink("/etc/hosts", &path).unwrap();
        let error = snapshot_image(path.to_str().unwrap(), dir.path())
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
