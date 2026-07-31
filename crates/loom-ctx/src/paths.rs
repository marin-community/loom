//! Machine-local paths that do not depend on Loom's domain policy.

use std::path::PathBuf;

/// Path to the file holding the machine-local token plaintext (mode 0600).
pub fn local_token_path() -> PathBuf {
    weaver_core::db::weaver_home().join("loom-token")
}
