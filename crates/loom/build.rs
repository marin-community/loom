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

// Builds the Vue frontend into `static/dist`, but only under the `embed-frontend`
// cargo feature (and when npm + the frontend sources are present). The SPA is
// served from `static/dist` at runtime (see `web::static_dir`), not embedded in
// the binary, so the bundle is independent of the Rust compile: a plain
// `cargo build` / `cargo test` skips it — the fast, Node-free path for backend
// work — and writes a placeholder page instead. Build a UI-bearing tree with
// `cargo build --features embed-frontend`, or run the SPA build directly with
// `npm --prefix crates/loom/frontend run build`.
fn main() {
    // Every file that feeds the frontend build: changing any of them reruns
    // this script (and therefore rspack). `frontend/src` covers the Vue/TS
    // sources and the HTML template; the rest are build-config inputs. (Cargo
    // already reruns build scripts when the active feature set changes, so the
    // `embed-frontend` toggle needs no explicit rerun directive.)
    println!("cargo:rerun-if-changed=frontend/src");
    println!("cargo:rerun-if-changed=frontend/package.json");
    println!("cargo:rerun-if-changed=frontend/package-lock.json");
    println!("cargo:rerun-if-changed=frontend/rspack.config.js");
    println!("cargo:rerun-if-changed=frontend/postcss.config.mjs");
    println!("cargo:rerun-if-changed=frontend/tsconfig.json");

    let dist = Path::new("static/dist");
    let frontend = Path::new("frontend");

    let embed = std::env::var_os("CARGO_FEATURE_EMBED_FRONTEND").is_some();
    let have_npm = Command::new("npm")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let have_sources = frontend.join("src/main.ts").exists();

    if !embed || !have_npm || !have_sources {
        std::fs::create_dir_all(dist).ok();
        let index = dist.join("index.html");
        if !index.exists() {
            std::fs::write(
                &index,
                "<!doctype html><meta charset=utf-8><title>weaver</title>\
                 <body style=\"font-family:sans-serif;padding:2rem\">\
                 <h1>weaver</h1><p>Frontend not built. Build it with \
                 <code>cargo build --features embed-frontend</code> \
                 (needs Node + npm), or \
                 <code>npm --prefix crates/loom/frontend run build</code>.</p>",
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
