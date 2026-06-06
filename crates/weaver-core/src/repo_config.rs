//! Per-repo configuration: a committed `.weaver/config.toml` that overrides
//! tool defaults for one repository.
//!
//! This is deliberately separate from [`crate::config`], which is the *global*
//! (machine/user) settings store backed by the SQLite `settings` table. Repo
//! conventions — like where plans live — belong with the repo, travel with the
//! clone, and are reviewable in the diff. The precedent is the per-repo
//! `WEAVER.md` that overrides the builtin agent primer: a committed file beats a
//! builtin default.
//!
//! ```toml
//! # .weaver/config.toml
//! [plan]
//! dir = "design/plans"   # default: docs/plans
//! ```
//!
//! Today the only key is `[plan].dir`; this is the seed of the mechanism, kept
//! minimal until more conventions need it. Reads are best-effort — a missing,
//! blank, or malformed file falls back to the builtin defaults, never an error.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// Builtin default for `[plan].dir` — where plan markdown files live, relative
/// to the worktree root.
pub const DEFAULT_PLAN_DIR: &str = "docs/plans";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoConfig {
    #[serde(default)]
    pub plan: PlanConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlanConfig {
    /// Directory holding plan files, relative to the worktree root.
    pub dir: Option<String>,
}

/// Load `<dir>/.weaver/config.toml`. Best-effort: a missing or malformed file
/// yields the all-defaults config.
pub fn load(dir: &Path) -> RepoConfig {
    std::fs::read_to_string(dir.join(".weaver").join("config.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Resolve the plan directory, checking each candidate worktree/checkout in
/// order (a config committed on the base branch is picked up either way), and
/// falling back to [`DEFAULT_PLAN_DIR`] when none sets a usable value.
///
/// The configured value is **constrained to a worktree-relative path** with no
/// `..`/absolute components: callers join it onto the worktree and read/list it,
/// so an unconstrained value (e.g. `../../etc`) committed in a hostile repo's
/// `.weaver/config.toml` would let the server escape the worktree. An invalid
/// value falls back to the builtin default rather than erroring.
pub fn plan_dir(candidates: &[PathBuf]) -> String {
    for dir in candidates {
        if let Some(d) = load(dir).plan.dir {
            let d = d.trim();
            if !d.is_empty() && is_safe_relative(d) {
                return d.to_string();
            }
        }
    }
    DEFAULT_PLAN_DIR.to_string()
}

/// A path that stays inside the worktree: relative, with only normal segments
/// (no `.`/`..`/absolute/prefix).
fn is_safe_relative(s: &str) -> bool {
    let p = Path::new(s);
    !p.is_absolute() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_config(dir: &Path, body: &str) {
        let weaver = dir.join(".weaver");
        fs::create_dir_all(&weaver).unwrap();
        fs::write(weaver.join("config.toml"), body).unwrap();
    }

    #[test]
    fn absent_file_uses_builtin_default() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(plan_dir(&[tmp.path().to_path_buf()]), DEFAULT_PLAN_DIR);
    }

    #[test]
    fn explicit_dir_wins() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "[plan]\ndir = \"design/plans\"\n");
        assert_eq!(plan_dir(&[tmp.path().to_path_buf()]), "design/plans");
    }

    #[test]
    fn blank_value_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "[plan]\ndir = \"   \"\n");
        assert_eq!(plan_dir(&[tmp.path().to_path_buf()]), DEFAULT_PLAN_DIR);
    }

    #[test]
    fn malformed_toml_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "this is not = valid = toml [[[");
        assert_eq!(plan_dir(&[tmp.path().to_path_buf()]), DEFAULT_PLAN_DIR);
    }

    #[test]
    fn escaping_dir_falls_back_to_default() {
        // A hostile committed config must not steer reads outside the worktree.
        for bad in ["../../etc", "/etc/passwd", "foo/../../bar", "./docs/plans"] {
            let tmp = tempfile::tempdir().unwrap();
            write_config(tmp.path(), &format!("[plan]\ndir = \"{bad}\"\n"));
            assert_eq!(
                plan_dir(&[tmp.path().to_path_buf()]),
                DEFAULT_PLAN_DIR,
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn nested_relative_dir_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(tmp.path(), "[plan]\ndir = \"design/specs/plans\"\n");
        assert_eq!(plan_dir(&[tmp.path().to_path_buf()]), "design/specs/plans");
    }

    #[test]
    fn first_non_empty_candidate_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // `a` has no config; `b` sets one. The resolver should fall through to b.
        write_config(b.path(), "[plan]\ndir = \"plans\"\n");
        assert_eq!(
            plan_dir(&[a.path().to_path_buf(), b.path().to_path_buf()]),
            "plans"
        );
    }
}
