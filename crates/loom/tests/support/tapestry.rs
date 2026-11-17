//! Pulled in with `#[path]` by every suite that spawns a real supervisor —
//! separate integration-test targets can't share code any other way.

use std::path::PathBuf;

/// The `tapestry` binary built alongside Loom's executable.
pub fn tapestry_bin() -> PathBuf {
    if let Some(bin) = std::env::var_os("WEAVER_TAPESTRY_BIN").map(PathBuf::from) {
        if bin.is_file() {
            return bin;
        }
    }

    // Cargo supplies the final artifact path even when its intermediate build
    // directory uses a separate tree or a different test executable layout.
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_loom")).with_file_name("tapestry");
    assert!(
        bin.is_file(),
        "tapestry binary missing — run `cargo build -p tapestry` (or `cargo test --workspace`) first.\nLooked in:\n  {}",
        bin.display()
    );
    bin
}
