//! Installing agent skills into every installed harness's global skills
//! directory.
//!
//! A skill is an operator-supplied payload — a `SKILL.md` file or a directory
//! bundling one plus scripts — never something loom authors or embeds. This
//! module owns only *placement*: which harnesses are installed, where each
//! reads user-level skills, and an idempotent, fail-soft copy into those
//! directories. `loom skills install` (the host CLI) and the entrypoint's
//! `LOOM_INSTALL_SKILLS` loop are both thin shells over [`install_skill`].

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use loom_store::agent_kind::BuiltinAgentKind;

/// The harnesses this installer knows how to place skills for, in picker
/// order: the single home of the harness *names* — detection runs them, the
/// CLI error message prints them. Extending [`BuiltinAgentKind`] forces a new
/// arm in the exhaustive `skills_dir_for` match below; grow this list
/// alongside it (the unit test ties every entry to a builtin kind).
pub const SUPPORTED_HARNESSES: &[&str] = &["claude", "codex"];

/// One installed harness and the directory it reads user-level skills from.
#[derive(Debug, Clone)]
pub struct SkillTarget {
    pub harness: &'static str,
    pub skills_dir: PathBuf,
}

/// What happened for one harness during [`install_skill`]. Never an error on
/// its own: a failed harness is recorded and the rest are still tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillInstallStatus {
    /// Files were copied (or overwritten) for this harness.
    Installed,
    /// The destination already held byte-identical content.
    UpToDate,
    /// This harness's copy failed; the reason is in `error`.
    Failed,
}

#[derive(Debug, Serialize)]
pub struct HarnessSkillInstall {
    pub harness: &'static str,
    pub skills_dir: PathBuf,
    pub status: SkillInstallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SkillInstallReport {
    pub name: String,
    pub harnesses: Vec<HarnessSkillInstall>,
}

impl SkillInstallReport {
    /// Whether at least one harness ended up holding the skill.
    pub fn any_installed(&self) -> bool {
        self.harnesses
            .iter()
            .any(|h| h.status != SkillInstallStatus::Failed)
    }
}

/// Install `source` (a `SKILL.md` file, or a skill directory) as the skill
/// `name` into every **installed** harness's global skills directory.
///
/// Detection follows the picker's — a harness whose binary is not on `PATH`
/// is skipped, not errored, so a best-effort or partial setup degrades
/// quietly. Per-harness failures are recorded, not raised: one unwritable
/// skills dir must not keep the skill from the other harness. A missing
/// `source` *is* an error — the caller named a path that isn't there, which
/// no amount of continuing can fix.
pub async fn install_skill(name: &str, source: &Path) -> Result<SkillInstallReport> {
    validate_skill_name(name)?;
    let source = source.to_path_buf();
    // Fail fast with the operator's own path in the message before any
    // harness is touched.
    tokio::fs::metadata(&source)
        .await
        .with_context(|| format!("reading skill source {}", source.display()))?;

    let mut harnesses = Vec::new();
    for target in installed_skill_targets().await {
        let dest_root = target.skills_dir.join(name);
        let (status, error) = match copy_skill(&source, &dest_root).await {
            Ok(CopyStatus::Copied) => {
                tracing::info!(harness = target.harness, skill = name, "skill installed");
                (SkillInstallStatus::Installed, None)
            }
            Ok(CopyStatus::UpToDate) => (SkillInstallStatus::UpToDate, None),
            Err(e) => {
                tracing::warn!(harness = target.harness, skill = name, error = %e,
                    "skill install failed for this harness");
                (SkillInstallStatus::Failed, Some(e.to_string()))
            }
        };
        harnesses.push(HarnessSkillInstall {
            harness: target.harness,
            skills_dir: target.skills_dir,
            status,
            error,
        });
    }
    Ok(SkillInstallReport {
        name: name.to_string(),
        harnesses,
    })
}

/// A skill name becomes one path component inside every harness's skills dir,
/// so it must be a plain, non-hidden, path-separator-free name.
fn validate_skill_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(std::path::MAIN_SEPARATOR_STR)
        && !name.chars().any(char::is_whitespace);
    if valid {
        Ok(())
    } else {
        bail!("skill name '{name}' must be a plain directory name (no separators, spaces, or leading dot)");
    }
}

/// The skills directories of every installed harness, in picker order.
pub async fn installed_skill_targets() -> Vec<SkillTarget> {
    let home = env_home();
    let codex_home = env_dir("CODEX_HOME");
    let mut targets = Vec::new();
    for name in SUPPORTED_HARNESSES {
        // The unit test guarantees every entry names a builtin kind; parse
        // defensively anyway so a stray entry is skipped, not a panic.
        let Some(kind) = BuiltinAgentKind::parse(name) else {
            continue;
        };
        // The docker entrypoint refuses to boot without the pinned runtimes,
        // but the host CLI promises nothing — a host loom may have only one
        // harness installed, and the picker hides the rest. Installing for
        // what is actually on PATH keeps that contract and avoids reporting
        // success for a directory no harness will ever read.
        if !binary_on_path(name).await {
            continue;
        }
        targets.push(SkillTarget {
            harness: name,
            skills_dir: skills_dir_for(kind, &home, codex_home.as_deref()),
        });
    }
    targets
}

fn env_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `CODEX_HOME` when set to a non-empty value, else `None` (the caller falls
/// back to the conventional `~/.codex`).
fn env_dir(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// The user-level skills directory one harness reads. Pure: the env-derived
/// inputs are parameters so tests need not race env access.
///
/// Sources: Claude Code (personal `~/.claude/skills/`) and the Codex CLI
/// (`~/.codex/skills/`, overridable via `CODEX_HOME`). The two read no
/// shared directory, so each harness's own canonical location is written.
fn skills_dir_for(kind: BuiltinAgentKind, home: &Path, codex_home: Option<&Path>) -> PathBuf {
    match kind {
        BuiltinAgentKind::Claude => home.join(".claude").join("skills"),
        BuiltinAgentKind::Codex => codex_home.unwrap_or(&home.join(".codex")).join("skills"),
    }
}

/// Whether `bin` resolves to an executable file on `PATH`.
///
/// Reports presence, not health: a binary that exists but misbehaves still
/// counts as installed. The filesystem walk runs on a blocking thread so a
/// slow `PATH` entry cannot stall the async runtime.
async fn binary_on_path(bin: &str) -> bool {
    let bin = bin.to_string();
    tokio::task::spawn_blocking(move || {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|dir| is_executable_file(&dir.join(&bin)))
    })
    .await
    .unwrap_or(false)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Whether a [`copy_skill`] wrote anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyStatus {
    /// At least one file was written.
    Copied,
    /// Every destination file already held byte-identical content.
    UpToDate,
}

/// Copy `source` into `dest_root` (the harness's `<skills>/<name>` dir),
/// writing only when content differs so re-runs are no-ops.
///
/// A file source lands as `dest_root/SKILL.md` — the name every harness
/// resolves — while a directory source keeps its inner layout (a skill may
/// bundle scripts next to its `SKILL.md`). Extra files from a previous
/// version are left in place: the copy is additive, never a mirror.
async fn copy_skill(source: &Path, dest_root: &Path) -> Result<CopyStatus> {
    let source = source.to_path_buf();
    let dest_root = dest_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&source)
            .with_context(|| format!("reading skill source {}", source.display()))?;
        // A wrong-typed destination (stale skill version or stray operator
        // file) is replaced, not failed on.
        clear_type_conflict(&dest_root, true)?;
        if meta.is_dir() {
            copy_tree(&source, &dest_root)
        } else {
            let dest = dest_root.join("SKILL.md");
            clear_type_conflict(&dest, false)?;
            copy_file_if_changed(&source, &dest)
        }
    })
    .await
    .context("joining skill copy")?
}

/// Remove `dest` when its type (file, directory, or symlink) doesn't fit
/// what the source needs to place there, so a skill version that changed an
/// entry's type replaces the stale one. Symlinks always yield, so nothing
/// is ever written through.
fn clear_type_conflict(dest: &Path, needed_is_dir: bool) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(dest) else {
        return Ok(());
    };
    if meta.is_dir() {
        if !needed_is_dir {
            std::fs::remove_dir_all(dest)
                .with_context(|| format!("replacing {}", dest.display()))?;
        }
    } else if needed_is_dir || meta.is_symlink() {
        std::fs::remove_file(dest).with_context(|| format!("replacing {}", dest.display()))?;
    }
    Ok(())
}

/// Remove `path` whatever its type — used when replacing a stale entry.
fn remove_any(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
    .with_context(|| format!("replacing {}", path.display()))
}

/// Recursively copy `source` into `dest`, writing only differing files.
fn copy_tree(source: &Path, dest: &Path) -> Result<CopyStatus> {
    let mut status = CopyStatus::UpToDate;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("listing {}", source.display()))?
    {
        let entry = entry?;
        let entry_dest = dest.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            clear_type_conflict(&entry_dest, true)?;
            if copy_tree(&entry.path(), &entry_dest)? == CopyStatus::Copied {
                status = CopyStatus::Copied;
            }
        } else if file_type.is_file() {
            clear_type_conflict(&entry_dest, false)?;
            if copy_file_if_changed(&entry.path(), &entry_dest)? == CopyStatus::Copied {
                status = CopyStatus::Copied;
            }
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            // `exists()` follows links: a changed target would look "present"
            // and never propagate, and a broken dest link would make the
            // create below fail on EEXIST. Compare the links themselves.
            if std::fs::read_link(&entry_dest).is_ok_and(|t| t == target) {
                continue;
            }
            remove_any(&entry_dest)?;
            std::fs::create_dir_all(dest)?;
            create_symlink(&target, &entry_dest)
                .with_context(|| format!("linking {}", entry_dest.display()))?;
            status = CopyStatus::Copied;
        }
    }
    Ok(status)
}

/// A symlink in a skill directory (rare, but a bundled script may use one).
/// Windows needs the file-flavored call, and may still refuse without
/// developer mode — the caller records the failure for that harness only.
#[cfg(unix)]
fn create_symlink(target: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, dest)
}

#[cfg(windows)]
fn create_symlink(target: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, dest)
}

/// Copy one file, skipping the write when the destination already holds
/// byte-identical content (idempotent re-runs; boots rewrite every time).
fn copy_file_if_changed(source: &Path, dest: &Path) -> Result<CopyStatus> {
    if files_equal(source, dest)? {
        return Ok(CopyStatus::UpToDate);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::copy(source, dest).with_context(|| format!("writing {}", dest.display()))?;
    Ok(CopyStatus::Copied)
}

fn files_equal(a: &Path, b: &Path) -> Result<bool> {
    let (a_bytes, b_bytes) = match (std::fs::read(a), std::fs::read(b)) {
        (Ok(a), Ok(b)) => (a, b),
        // A missing/unreadable destination just means it must be written.
        (Ok(_), Err(_)) => return Ok(false),
        (Err(e), _) => return Err(e).with_context(|| format!("reading {}", a.display())),
    };
    Ok(a_bytes == b_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        (dir, home)
    }

    #[test]
    fn supported_harnesses_all_name_builtin_kinds() {
        for name in SUPPORTED_HARNESSES {
            assert!(
                BuiltinAgentKind::parse(name).is_some(),
                "{name} must name a builtin kind"
            );
        }
    }

    #[test]
    fn skill_name_must_be_a_plain_directory_name() {
        for good in ["open-code-review", "a", "skill_2"] {
            validate_skill_name(good).unwrap();
        }
        for bad in ["", ".", "..", ".hidden", "a/b", "a\\b", " a"] {
            assert!(
                validate_skill_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn skills_dir_maps_each_harness_to_its_documented_location() {
        let home = Path::new("/home/app");
        assert_eq!(
            skills_dir_for(BuiltinAgentKind::Claude, home, None),
            PathBuf::from("/home/app/.claude/skills")
        );
        // Codex honors its env-var home when set, and falls back to the
        // conventional dotdir under $HOME when not.
        assert_eq!(
            skills_dir_for(BuiltinAgentKind::Codex, home, None),
            PathBuf::from("/home/app/.codex/skills")
        );
        assert_eq!(
            skills_dir_for(
                BuiltinAgentKind::Codex,
                home,
                Some(Path::new("/custom/codex"))
            ),
            PathBuf::from("/custom/codex/skills")
        );
    }

    #[tokio::test]
    async fn file_source_lands_as_skill_md() {
        let (_dir, home) = temp_home();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("anything.md");
        std::fs::write(&file, "instructions").unwrap();

        copy_skill(&file, &home.join("skills/open-code-review"))
            .await
            .unwrap();
        let installed =
            std::fs::read_to_string(home.join("skills/open-code-review/SKILL.md")).unwrap();
        assert_eq!(installed, "instructions");
    }

    #[tokio::test]
    async fn directory_source_keeps_its_layout() {
        let (_dir, home) = temp_home();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("SKILL.md"), "instructions").unwrap();
        std::fs::create_dir(source.path().join("scripts")).unwrap();
        std::fs::write(source.path().join("scripts/run.sh"), "#!/bin/sh\n").unwrap();

        copy_skill(source.path(), &home.join("skills/packaged"))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("skills/packaged/SKILL.md")).unwrap(),
            "instructions"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("skills/packaged/scripts/run.sh")).unwrap(),
            "#!/bin/sh\n"
        );
    }

    #[tokio::test]
    async fn reruns_with_identical_content_write_nothing() {
        let (dir, home) = temp_home();
        let dest_root = home.join("skills/open-code-review");
        std::fs::create_dir_all(&dest_root).unwrap();
        std::fs::write(dest_root.join("SKILL.md"), "v1").unwrap();

        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("SKILL.md"), "v1").unwrap();
        // An identical payload must skip the write, not just happen to write
        // the same bytes: a rewrite would have to open the read-only
        // destination file, so success proves the skip.
        chmod_ro(&dest_root.join("SKILL.md"));
        let status = copy_skill(source.path().join("SKILL.md").as_path(), &dest_root)
            .await
            .unwrap();
        chmod_rw(&dest_root.join("SKILL.md"));
        assert_eq!(status, CopyStatus::UpToDate);
        drop(dir);
    }

    #[tokio::test]
    async fn changed_content_overwrites() {
        let (_dir, home) = temp_home();
        let dest_root = home.join("skills/open-code-review");
        std::fs::create_dir_all(&dest_root).unwrap();
        std::fs::write(dest_root.join("SKILL.md"), "v1").unwrap();

        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("SKILL.md"), "v2").unwrap();
        copy_skill(source.path().join("SKILL.md").as_path(), &dest_root)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dest_root.join("SKILL.md")).unwrap(),
            "v2"
        );
    }

    #[tokio::test]
    async fn missing_source_is_an_error() {
        let (_dir, home) = temp_home();
        assert!(
            copy_skill(Path::new("/nonexistent/skill.md"), &home.join("skills/x"))
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn entry_type_changes_between_skill_versions_replace_the_stale_one() {
        let (_dir, home) = temp_home();
        let dest_root = home.join("skills/packaged");
        let source = tempfile::tempdir().unwrap();

        // v1 ships `helper` as a file.
        std::fs::write(source.path().join("SKILL.md"), "v1").unwrap();
        std::fs::write(source.path().join("helper"), "old").unwrap();
        copy_skill(source.path(), &dest_root).await.unwrap();

        // v2 ships `helper` as a directory: the stale file is replaced, the
        // copy does not fail for the harness.
        std::fs::remove_file(source.path().join("helper")).unwrap();
        std::fs::create_dir(source.path().join("helper")).unwrap();
        std::fs::write(source.path().join("helper/data"), "new").unwrap();
        copy_skill(source.path(), &dest_root).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dest_root.join("helper/data")).unwrap(),
            "new"
        );

        // A file source replaces a dest whose SKILL.md became a directory.
        let file_source = tempfile::tempdir().unwrap();
        std::fs::write(file_source.path().join("skill.md"), "v3").unwrap();
        std::fs::remove_file(dest_root.join("SKILL.md")).unwrap();
        std::fs::create_dir(dest_root.join("SKILL.md")).unwrap();
        copy_skill(file_source.path().join("skill.md").as_path(), &dest_root)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dest_root.join("SKILL.md")).unwrap(),
            "v3"
        );

        // A dir source replaces a dest_root that is a plain file.
        std::fs::remove_dir_all(&dest_root).unwrap();
        std::fs::write(&dest_root, "stale").unwrap();
        copy_skill(source.path(), &dest_root).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dest_root.join("helper/data")).unwrap(),
            "new"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_source_replaces_a_destination_directory() {
        let (_dir, home) = temp_home();
        let dest_root = home.join("skills/packaged");
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("real"), "x").unwrap();
        std::os::unix::fs::symlink("real", source.path().join("link")).unwrap();

        // An older skill version shipped `link` as a directory; the link
        // replaces it instead of failing with EISDIR.
        std::fs::create_dir_all(dest_root.join("link")).unwrap();
        copy_skill(source.path(), &dest_root).await.unwrap();
        assert_eq!(
            std::fs::read_link(dest_root.join("link")).unwrap(),
            std::path::Path::new("real")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_sources_propagate_target_changes() {
        let (_dir, home) = temp_home();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("SKILL.md"), "x").unwrap();
        std::fs::write(source.path().join("real-a"), "a").unwrap();
        std::fs::write(source.path().join("real-b"), "b").unwrap();
        std::os::unix::fs::symlink("real-a", source.path().join("link")).unwrap();

        let dest_root = home.join("skills/packaged");
        copy_skill(source.path(), &dest_root).await.unwrap();
        assert_eq!(
            std::fs::read_link(dest_root.join("link")).unwrap(),
            std::path::Path::new("real-a")
        );

        // A changed link target is a content change: the copy must replace
        // the link, not decide it already exists (which follows links).
        std::fs::remove_file(source.path().join("link")).unwrap();
        std::os::unix::fs::symlink("real-b", source.path().join("link")).unwrap();
        copy_skill(source.path(), &dest_root).await.unwrap();
        assert_eq!(
            std::fs::read_link(dest_root.join("link")).unwrap(),
            std::path::Path::new("real-b")
        );
        // And a broken destination link is replaced, not EEXIST-fatal.
        std::fs::remove_file(source.path().join("link")).unwrap();
        std::os::unix::fs::symlink("real-c", source.path().join("link")).unwrap();
        std::fs::remove_file(dest_root.join("link")).unwrap();
        std::os::unix::fs::symlink("gone", dest_root.join("link")).unwrap();
        copy_skill(source.path(), &dest_root).await.unwrap();
        assert_eq!(
            std::fs::read_link(dest_root.join("link")).unwrap(),
            std::path::Path::new("real-c")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unwritable_harness_dir_fails_that_copy_only() {
        let (dir, home) = temp_home();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("SKILL.md"), "x").unwrap();

        // A skills dir inside a read-only parent stands in for any harness
        // whose directory loom cannot write to.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        chmod_ro(&blocked);

        let ok_dest = home.join("skills/good");
        copy_skill(source.path(), &ok_dest).await.unwrap();
        let bad_dest = blocked.join("skills/bad");
        assert!(copy_skill(source.path(), &bad_dest).await.is_err());
        assert!(ok_dest.join("SKILL.md").exists());
        chmod_rw(&blocked);
    }

    /// Detection sees a harness iff its binary is on `PATH` — the same
    /// contract the picker's availability check will hold, so what installs
    /// is what the picker shows.
    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn installed_skill_targets_follow_path_detection() {
        let bin = tempfile::tempdir().unwrap();
        for name in SUPPORTED_HARNESSES {
            let path = bin.path().join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            make_executable(&path);
        }
        let home = tempfile::tempdir().unwrap();

        // Restore on scope exit *and* on panic: a failed assert must not leak
        // a mutated PATH/HOME into every test that runs after this one.
        let restore = EnvRestore::set([
            ("PATH", Some(bin.path().as_os_str().to_owned())),
            ("HOME", Some(home.path().as_os_str().to_owned())),
            ("CODEX_HOME", None),
        ]);
        // PATH contains only our fake binaries — a real harness installed on
        // this dev machine must not leak into the result.

        let targets = installed_skill_targets().await;
        let names: Vec<&str> = targets.iter().map(|t| t.harness).collect();
        assert_eq!(names, ["claude", "codex"]);
        assert_eq!(
            targets[0].skills_dir,
            home.path().join(".claude/skills"),
            "dirs resolve under the active $HOME"
        );

        // A harness missing from PATH drops out entirely.
        std::fs::remove_file(bin.path().join("codex")).unwrap();
        let targets = installed_skill_targets().await;
        let names: Vec<&str> = targets.iter().map(|t| t.harness).collect();
        assert_eq!(names, ["claude"]);
        drop(restore);
    }

    /// Applies `(env var, Some(new) | None-to-remove)` pairs, restoring the
    /// prior values on drop — panic-safe env mutation for tests.
    struct EnvRestore {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvRestore {
        fn set<const N: usize>(changes: [(&'static str, Option<std::ffi::OsString>); N]) -> Self {
            let saved = changes
                .iter()
                .map(|(key, _)| (*key, std::env::var_os(key)))
                .collect();
            for (key, value) in changes {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            Self { saved }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in self.saved.clone() {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[tokio::test]
    async fn install_skill_missing_source_bails() {
        let (_dir, _home) = temp_home();
        assert!(
            install_skill("open-code-review", Path::new("/nonexistent/source"))
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[cfg(windows)]
    fn make_executable(_path: &Path) {}

    #[cfg(unix)]
    fn chmod_ro(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[cfg(unix)]
    fn chmod_rw(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[cfg(windows)]
    fn chmod_ro(_path: &Path) {}

    #[cfg(windows)]
    fn chmod_rw(_path: &Path) {}
}
