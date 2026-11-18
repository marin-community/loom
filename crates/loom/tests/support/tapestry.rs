//! Pulled in with `#[path]` by every suite that spawns a real supervisor —
//! separate integration-test targets can't share code any other way.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The `tapestry` binary built alongside this test binary.
///
/// Cargo 1.100 runs integration-test binaries from the build side —
/// `<target>/<profile>/build/<pkg>/<hash>/…` — where older cargo ran them
/// from the artifact side, `<target>/<profile>/deps/`. In both layouts the
/// `build` (or, pre-1.100, `deps`) directory sits just under the profile
/// directory, and the uplifted `tapestry` lands at
/// `<target>/<profile>/tapestry` — or wherever a split `build.build-dir`
/// puts the artifacts, which the `CARGO_TARGET_DIR` and cargo-metadata
/// probes below cover.
pub fn tapestry_bin() -> PathBuf {
    if let Some(bin) = std::env::var_os("WEAVER_TAPESTRY_BIN").map(PathBuf::from) {
        if bin.is_file() {
            return bin;
        }
    }

    let exe = std::env::current_exe().expect("test executable path");
    let mut marker: Option<PathBuf> = None;
    for dir in exe.ancestors().skip(1) {
        let is = |name: &str| dir.file_name().and_then(OsStr::to_str) == Some(name);
        if is("build") {
            marker = Some(dir.to_path_buf());
            break;
        }
        // A `deps` directory may also appear inside the build side; the
        // `build` marker above replaces it when the split layout is present.
        if marker.is_none() && is("deps") {
            marker = Some(dir.to_path_buf());
        }
    }
    let Some(marker) = marker else {
        panic!(
            "tapestry binary missing — unrecognized test-binary layout: {}",
            exe.display()
        );
    };
    let profile_dir = marker.parent().expect("profile directory");
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
