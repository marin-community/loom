//! Dotenv-shaped text: upsert `KEY=value` lines while preserving everything
//! else — comments, blank lines, unrelated keys, and their order — and write
//! a secrets-bearing file with owner-only permissions.
//!
//! Multi-line values (an RSA private key PEM) are written double-quoted with
//! embedded newlines escaped to a literal `\n` — the form docker compose's
//! `env_file` parser (compose-go's dotenv, based on `joho/godotenv`) expands
//! back to real newlines inside double quotes.

use std::path::Path;

use anyhow::{Context, Result};

/// Upsert every `(key, value)` pair into `contents`. An existing uncommented
/// `KEY=...` line is replaced in place (keeping its position); a key with no
/// existing line is appended at the end. Everything else in `contents` — other
/// lines, comments, blank lines, ordering — is left untouched.
pub fn upsert(contents: &str, updates: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = if contents.is_empty() {
        Vec::new()
    } else {
        contents.lines().map(str::to_string).collect()
    };
    let mut remaining: Vec<(&str, &str)> = updates.to_vec();

    for line in lines.iter_mut() {
        let Some(key) = uncommented_key(line) else {
            continue;
        };
        if let Some(pos) = remaining.iter().position(|(k, _)| *k == key) {
            let (k, v) = remaining.remove(pos);
            *line = format!("{k}={}", format_value(v));
        }
    }

    if !remaining.is_empty() {
        if lines.last().is_some_and(|l| !l.is_empty()) {
            lines.push(String::new());
        }
        for (key, value) in remaining {
            lines.push(format!("{key}={}", format_value(value)));
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Write `contents` to `path` as a secrets-bearing file: create parent
/// directories as needed and restrict permissions to the owner (0600 on
/// unix). Shared by `loom.toml` ([`crate::loom_config::save`]) and the
/// generated `.env` (`loom config render-env`'s `--out`) — both can hold
/// plaintext credentials.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(())
}

/// The `KEY` of an uncommented `KEY=...` line, or `None` for a comment, a blank
/// line, or a line with no `=`.
fn uncommented_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(k, _)| k.trim())
}

/// Format a value for a dotenv line: multi-line or otherwise shell-special
/// values are double-quoted (embedded newlines escaped to `\n`, embedded double
/// quotes escaped); a plain value is written bare, matching the existing
/// `.env.example` style.
fn format_value(value: &str) -> String {
    let needs_quoting = value.is_empty()
        || value
            .chars()
            .any(|c| c == '\n' || c == '"' || c == '#' || c.is_whitespace());
    if !needs_quoting {
        return value.to_string();
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_new_keys_to_an_empty_file() {
        let out = upsert("", &[("FOO", "bar")]);
        assert_eq!(out, "FOO=bar\n");
    }

    #[test]
    fn replaces_an_existing_key_in_place_and_leaves_the_rest_untouched() {
        let input = "# a comment\nLOOM_DOMAIN=loom.example.com\nGH_TOKEN=old\nHOST_UID=1000\n";
        let out = upsert(input, &[("GH_TOKEN", "ghp_new")]);
        assert_eq!(
            out,
            "# a comment\nLOOM_DOMAIN=loom.example.com\nGH_TOKEN=ghp_new\nHOST_UID=1000\n"
        );
    }

    #[test]
    fn appends_missing_keys_after_a_blank_separator() {
        let input = "LOOM_DOMAIN=loom.example.com\n";
        let out = upsert(input, &[("GH_TOKEN", "ghp_new")]);
        assert_eq!(out, "LOOM_DOMAIN=loom.example.com\n\nGH_TOKEN=ghp_new\n");
    }

    #[test]
    fn does_not_double_up_the_blank_separator() {
        let input = "LOOM_DOMAIN=loom.example.com\n\n";
        let out = upsert(input, &[("GH_TOKEN", "ghp_new")]);
        assert_eq!(out, "LOOM_DOMAIN=loom.example.com\n\nGH_TOKEN=ghp_new\n");
    }

    #[test]
    fn ignores_commented_out_keys_when_matching() {
        let input = "# GH_TOKEN=disabled\n";
        let out = upsert(input, &[("GH_TOKEN", "ghp_new")]);
        assert_eq!(out, "# GH_TOKEN=disabled\n\nGH_TOKEN=ghp_new\n");
    }

    #[test]
    fn multiline_values_are_double_quoted_with_escaped_newlines() {
        let out = upsert(
            "",
            &[("PEM", "-----BEGIN KEY-----\nAAAA\n-----END KEY-----")],
        );
        assert_eq!(
            out,
            "PEM=\"-----BEGIN KEY-----\\nAAAA\\n-----END KEY-----\"\n"
        );
    }

    #[test]
    fn plain_values_are_written_bare_matching_env_example_style() {
        let out = upsert("", &[("ANTHROPIC_API_KEY", "sk-ant-abc123")]);
        assert_eq!(out, "ANTHROPIC_API_KEY=sk-ant-abc123\n");
    }

    #[test]
    fn multiple_updates_apply_together() {
        let input = "A=1\nB=2\n";
        let out = upsert(input, &[("B", "22"), ("C", "3")]);
        assert_eq!(out, "A=1\nB=22\n\nC=3\n");
    }
}
