//! Environment policy shared by the storage, launch and runtime layers.

/// Environment names that exist only for Loom's own boot-time provisioning.
///
/// The server consumes these once at startup and must not pass them on to
/// session processes: sessions inherit the server's ambient environment unless
/// their profile clears it, so an un-stripped name would hand the payload —
/// a whole credential store — to every agent and tool call.
pub const BOOT_ONLY_ENV: &[&str] = &["OPENCODE_CREDENTIALS_JSON"];

/// Whether `name` is a boot-only variable that sessions must never inherit.
pub fn is_boot_only_env(name: &str) -> bool {
    BOOT_ONLY_ENV.contains(&name)
}

/// Drop boot-only variables from a launch environment.
pub fn without_boot_only<'a>(env: &'a [(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    env.iter()
        .copied()
        .filter(|(name, _)| !is_boot_only_env(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credential_export_is_boot_only() {
        assert!(is_boot_only_env("OPENCODE_CREDENTIALS_JSON"));
        assert!(!is_boot_only_env("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn a_launch_environment_drops_boot_only_names() {
        let env = [
            ("PATH", "/usr/bin"),
            ("OPENCODE_CREDENTIALS_JSON", "[{\"secret\":true}]"),
        ];
        assert_eq!(without_boot_only(&env), vec![("PATH", "/usr/bin")]);
    }
}
