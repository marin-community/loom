use std::path::Path;

use serde_json::json;
use serial_test::serial;

use crate::fixtures::TestServer;

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_file_previews_and_raw_bytes_stay_inside_the_worktree() {
    let ts = TestServer::start().await;
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "file previews", "cwd": ts.cwd(), "agent": "shell"
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    let root = Path::new(session["work_dir"].as_str().unwrap());
    std::fs::create_dir(root.join("reports")).unwrap();
    let cases: &[(&str, &[u8], &str)] = &[
        ("reports/index.md", b"# Report\n", "markdown"),
        ("report.html", b"<h1>Report</h1>", "html"),
        ("script.rs", b"fn main() {}", "text"),
        ("results.json", b"{\"rows\": 42}", "text"),
        ("plot.APNG", b"image data", "image"),
        ("photo.jfif", b"image data", "image"),
        ("binary.dat", b"\0\xff", "download"),
        (
            "image.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            "image",
        ),
        ("report.pdf", b"%PDF-1.4", "download"),
    ];
    for (path, bytes, kind) in cases {
        std::fs::write(root.join(path), bytes).unwrap();
        let preview = ts
            .client
            .post("/api/sessions/file", json!({ "session": id, "path": path }))
            .await
            .unwrap();
        assert_eq!(preview["path"], *path);
        assert_eq!(preview["kind"], *kind);
        if matches!(*kind, "markdown" | "html" | "text") {
            assert_eq!(preview["content"], std::str::from_utf8(bytes).unwrap());
        } else {
            assert!(preview["content"].is_null());
        }
    }
    std::fs::write(root.join("large.txt"), vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();
    let large = ts
        .client
        .post(
            "/api/sessions/file",
            json!({ "session": id, "path": "large.txt" }),
        )
        .await
        .unwrap();
    assert_eq!(large["kind"], "download");
    assert!(large["content"].is_null());

    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("escape.md")).unwrap();
    std::os::unix::fs::symlink(root.join("reports/index.md"), root.join("inside.md")).unwrap();
    let inside = ts
        .client
        .post(
            "/api/sessions/file",
            json!({ "session": id, "path": "inside.md" }),
        )
        .await
        .unwrap();
    assert_eq!(inside["content"], "# Report\n");

    let http = reqwest::Client::new();
    for path in [
        "../outside",
        "/etc/passwd",
        "reports/../../outside",
        "reports\\..\\..\\outside",
        "escape.md",
        "reports",
    ] {
        let preview = http
            .post(format!("{}/api/sessions/file", ts.client.base()))
            .json(&json!({ "session": id, "path": path }))
            .send()
            .await
            .unwrap();
        assert_eq!(preview.status(), 400, "preview accepted {path}");
        let raw = http
            .get(format!("{}/api/sessions/raw", ts.client.base()))
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(raw.status(), 400, "raw accepted {path}");
    }
    let missing = http
        .post(format!("{}/api/sessions/file", ts.client.base()))
        .json(&json!({ "session": id, "path": "missing.md" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let raw = http
        .get(format!("{}/api/sessions/raw", ts.client.base()))
        .query(&[("session", id), ("path", "report.html")])
        .send()
        .await
        .unwrap();
    assert_eq!(raw.status(), 200);
    assert_eq!(raw.headers()["x-content-type-options"], "nosniff");
    assert_eq!(raw.headers()["cross-origin-resource-policy"], "same-origin");
    assert!(raw.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .split(';')
        .any(|directive| directive.trim() == "sandbox allow-scripts"));
    assert_eq!(raw.text().await.unwrap(), "<h1>Report</h1>");
    let image = http
        .get(format!("{}/api/sessions/raw", ts.client.base()))
        .query(&[("session", id), ("path", "plot.APNG")])
        .send()
        .await
        .unwrap();
    assert_eq!(image.status(), 200);
    assert_eq!(image.headers()["content-type"], "image/apng");
    assert_eq!(image.bytes().await.unwrap().as_ref(), b"image data");
}

/// `sessions.image` — chat prose embeds absolute paths; the conversation view
/// points the `<img src>` here. The path is confined to the requesting
/// session's own worktree — the same boundary `sessions.raw` holds — and only
/// magic-byte-verified raster bytes are served.
#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_paths_serve_sniffed_worktree_bytes_only() {
    let ts = TestServer::start().await;
    let session = ts
        .client
        .post(
            "/api/sessions/launch",
            json!({
                "goal": "inline images", "cwd": ts.cwd(), "agent": "shell"
            }),
        )
        .await
        .unwrap();
    let id = session["id"].as_str().unwrap();
    let png = b"\x89PNG\r\n\x1a\npayload";
    let root = Path::new(session["work_dir"].as_str().unwrap());
    std::fs::write(root.join("capture.png"), png).unwrap();
    std::fs::write(root.join(".agent-scratch").join("shot.png"), png).unwrap();
    // A symlink inside the worktree resolving to another file inside it
    // canonicalizes to its target and serves.
    std::os::unix::fs::symlink(root.join("capture.png"), root.join("alias.png")).unwrap();

    let http = reqwest::Client::new();
    let base = format!("{}/api/sessions/image", ts.client.base());
    for path in [
        root.join("capture.png").to_str().unwrap(),
        root.join(".agent-scratch")
            .join("shot.png")
            .to_str()
            .unwrap(),
        root.join("alias.png").to_str().unwrap(),
    ] {
        let image = http
            .get(&base)
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(image.status(), 200, "refused worktree image {path}");
        assert_eq!(image.headers()["content-type"], "image/png");
        assert_eq!(image.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            image.headers()["cross-origin-resource-policy"],
            "same-origin"
        );
        assert!(image.headers()["etag"].to_str().unwrap().starts_with('"'));
        assert_eq!(image.bytes().await.unwrap().as_ref(), png);
    }

    // The ETag answers a conditional request with 304, so re-entering a long
    // transcript does not re-fetch every screenshot.
    let first = http
        .get(&base)
        .query(&[
            ("session", id),
            ("path", root.join("capture.png").to_str().unwrap()),
        ])
        .send()
        .await
        .unwrap();
    let etag = first.headers()["etag"].clone();
    let fresh = http
        .get(&base)
        .query(&[
            ("session", id),
            ("path", root.join("capture.png").to_str().unwrap()),
        ])
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(fresh.status(), 304, "a matching ETag must short-circuit");

    // Confinement: files outside the session's own worktree — and symlinks
    // planted inside it that resolve outside — are refused, whatever they hold.
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("shot.png");
    std::fs::write(&secret, png).unwrap();
    std::os::unix::fs::symlink(&secret, root.join("escape.png")).unwrap();
    let outside_dir = outside.path().canonicalize().unwrap();
    for path in [
        secret.to_str().unwrap(),
        root.join("escape.png").to_str().unwrap(),
        outside_dir.to_str().unwrap(),
    ] {
        let image = http
            .get(&base)
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(image.status(), 400, "served {path}");
    }

    // The worktree itself is a directory, not a regular file.
    let worktree = http
        .get(&base)
        .query(&[
            ("session", id),
            ("path", root.canonicalize().unwrap().to_str().unwrap()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(worktree.status(), 400);

    // A FIFO must be refused without ever blocking on a writer: open(2) on a
    // read-only FIFO parks the calling thread until one appears.
    let mkfifo = std::process::Command::new("mkfifo")
        .arg(root.join("pipe.png"))
        .status()
        .unwrap();
    assert!(mkfifo.success(), "mkfifo must create the fixture");
    let pipe = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        http.get(&base)
            .query(&[
                ("session", id),
                ("path", root.join("pipe.png").to_str().unwrap()),
            ])
            .send(),
    )
    .await
    .expect("the FIFO request must not hang")
    .unwrap();
    assert_eq!(pipe.status(), 400, "served a FIFO");
    drop(pipe);

    // The size cap holds: a sparse 64 MiB + 1 file is refused before a read.
    let huge = std::fs::File::create(root.join("huge.png")).unwrap();
    huge.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(huge);
    let image = http
        .get(&base)
        .query(&[
            ("session", id),
            ("path", root.join("huge.png").to_str().unwrap()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(image.status(), 400, "served an over-cap image");

    // Anything that is not a sniffable raster is refused whatever the name
    // claims — the path is agent-chosen, so bytes are the only truth.
    std::fs::write(root.join("notes.txt"), b"definitely not an image").unwrap();
    std::fs::write(
        root.join("fake.png"),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    for path in [
        root.join("notes.txt").to_str().unwrap(),
        root.join("fake.png").to_str().unwrap(),
        "/nonexistent/missing.png",
    ] {
        let image = http
            .get(&base)
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(image.status(), 404, "served {path}");
    }

    // Shape violations are the caller's fault: relative paths (that is
    // `sessions.raw`'s job), backslashes, and the empty string.
    for path in ["shot.png", "reports/../shot.png", "C:\\shot.png", ""] {
        let image = http
            .get(&base)
            .query(&[("session", id), ("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(image.status(), 400, "accepted {path}");
    }
}
