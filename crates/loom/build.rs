use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

/// Modification time of `path`, or the epoch when it cannot be read (so a
/// missing file always counts as "older" than anything that exists).
fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

// Builds the Vue frontend into `static/dist` as part of `cargo build`, so a
// successful build always leaves a ready-to-serve bundle (loom serves it from
// `static/dist` at runtime — see `web::static_dir`). `rerun-if-changed` keeps it
// cheap: rspack only re-runs when a frontend source changes, so backend-only
// edits don't pay for it. When npm or the frontend sources are missing (a
// Node-less, backend-only checkout), it degrades to a placeholder page instead
// of failing the build.
fn main() {
    // Every file that feeds the frontend build: changing any of them reruns
    // this script (and therefore rspack). `frontend/src` covers the Vue/TS
    // sources and the HTML template; the rest are build-config inputs.
    println!("cargo:rerun-if-changed=frontend/src");
    println!("cargo:rerun-if-changed=frontend/package.json");
    println!("cargo:rerun-if-changed=frontend/package-lock.json");
    println!("cargo:rerun-if-changed=frontend/rspack.config.js");
    println!("cargo:rerun-if-changed=frontend/postcss.config.mjs");
    println!("cargo:rerun-if-changed=frontend/tsconfig.json");

    let dist = Path::new("static/dist");
    let frontend = Path::new("frontend");

    let have_npm = Command::new("npm")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let have_sources = frontend.join("src/main.ts").exists();

    if !have_npm || !have_sources {
        std::fs::create_dir_all(dist).ok();
        let index = dist.join("index.html");
        if !index.exists() {
            std::fs::write(
                &index,
                "<!doctype html><meta charset=utf-8><title>weaver</title>\
                 <body style=\"font-family:sans-serif;padding:2rem\">\
                 <h1>weaver</h1><p>Frontend not built: npm or the frontend \
                 sources were unavailable at build time. Install Node + npm and \
                 rebuild, or run <code>npm --prefix crates/loom/frontend run \
                 build</code>.</p>",
            )
            .ok();
        }
        return;
    }

    // Install deps when `node_modules` is missing, or when `package-lock.json`
    // is newer than npm's record of the last install — so a dependency bump
    // is actually installed, not just rebuilt against stale `node_modules`.
    let installed_marker = frontend.join("node_modules/.package-lock.json");
    let lockfile = frontend.join("package-lock.json");
    if !frontend.join("node_modules").exists() || mtime(&lockfile) > mtime(&installed_marker) {
        let status = Command::new("npm")
            .arg("install")
            .current_dir(frontend)
            .status()
            .expect("npm install failed");
        assert!(status.success(), "npm install exited with {status}");
    }

    let status = Command::new("npx")
        .args(["rspack", "build"])
        .current_dir(frontend)
        .status()
        .expect("rspack build failed");
    assert!(status.success(), "rspack build exited with {status}");
}
