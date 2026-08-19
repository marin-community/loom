//! Pulled in with `#[path]` by every suite that spawns a real supervisor —
//! separate integration-test targets can't share code any other way.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `tapestry` binary built alongside this test binary.
///
/// A sibling two levels up from `<dir>/<profile>/deps/<test>` only holds while
/// cargo's build and artifact directories are the same tree. `build.build-dir`
/// splits them: test binaries stay in the build dir, `tapestry` is uplifted
/// into `<workspace>/target/<profile>/`.
pub fn tapestry_bin() -> PathBuf {
    if let Some(bin) = std::env::var_os("WEAVER_TAPESTRY_BIN").map(PathBuf::from) {
        if bin.is_file() {
            return bin;
        }
    }

    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("<profile>/deps/<test>");
    let profile = profile_dir.file_name().expect("profile directory name");

    let mut tried = Vec::new();
    let mut probe = |dir: PathBuf| {
        let bin = dir.join("tapestry");
        if bin.is_file() {
            return Some(bin);
        }
        tried.push(bin.display().to_string());
        None
    };

    if let Some(bin) = probe(profile_dir.to_path_buf()) {
        return bin;
    }
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        if let Some(bin) = probe(Path::new(&dir).join(profile)) {
            return bin;
        }
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace) = manifest_dir.parent().and_then(Path::parent) {
        if let Some(bin) = probe(workspace.join("target").join(profile)) {
            return bin;
        }
    }
    // Last: the only candidate that catches a config-file `build.target-dir`,
    // and the only one that costs a subprocess.
    if let Some(dir) = metadata_target_dir(manifest_dir) {
        if let Some(bin) = probe(dir.join(profile)) {
            return bin;
        }
    }

    panic!(
        "tapestry binary missing — run `cargo build -p tapestry` (or `cargo test --workspace`) first.\nLooked in:\n  {}",
        tried.join("\n  ")
    );
}

fn metadata_target_dir(manifest_dir: &Path) -> Option<PathBuf> {
    let out = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(manifest_dir.join("Cargo.toml"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(PathBuf::from(meta.get("target_directory")?.as_str()?))
}
