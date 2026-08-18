//! Regression coverage for the Git/GitHub CLI adapters authored in Dockerfile.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const DOCKERFILE: &str = include_str!("../../../Dockerfile");

fn embedded_script(path: &str) -> String {
    let start = format!("cat > {path} <<'SH'\n");
    let (_, tail) = DOCKERFILE
        .split_once(&start)
        .unwrap_or_else(|| panic!("missing {path} heredoc"));
    let (script, _) = tail
        .split_once("\nSH\n")
        .unwrap_or_else(|| panic!("unterminated {path} heredoc"));
    script.replace("/usr/local/bin/weaver github-token", "printf broker-token")
}

fn run_script(script: &str, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut child = Command::new("sh")
        .arg("-s")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn git_helper_obeys_loom_owned_auth_mode() {
    let script = embedded_script("/usr/local/bin/git-credential-ghtoken");

    let brokered = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "broker"),
            ("LOOM_SESSION_ID", "session"),
            ("LOOM_TOKEN", "session-token"),
            ("GH_TOKEN", "wrong-daemon-bot"),
        ],
        &["get"],
    );
    assert!(brokered.status.success());
    assert_eq!(
        String::from_utf8(brokered.stdout).unwrap(),
        "username=x-access-token\npassword=broker-token\n"
    );

    let direct = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "direct"),
            ("GH_TOKEN", "personal-token"),
        ],
        &["get"],
    );
    assert!(direct.status.success());
    assert_eq!(
        String::from_utf8(direct.stdout).unwrap(),
        "username=x-access-token\npassword=personal-token\n"
    );

    let disabled = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "disabled"),
            ("GH_TOKEN", "wrong-daemon-bot"),
        ],
        &["get"],
    );
    assert!(disabled.status.success());
    assert!(disabled.stdout.is_empty());

    let incomplete = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "broker"),
            ("GH_TOKEN", "wrong-daemon-bot"),
        ],
        &["get"],
    );
    assert!(!incomplete.status.success());
    assert!(String::from_utf8(incomplete.stderr)
        .unwrap()
        .contains("missing its session credential"));

    let unmarked = run_script(&script, &[("GH_TOKEN", "wrong-daemon-bot")], &["get"]);
    assert!(!unmarked.status.success());
    assert!(String::from_utf8(unmarked.stderr)
        .unwrap()
        .contains("missing LOOM_GITHUB_AUTH_MODE"));

    let incomplete_direct = run_script(&script, &[("LOOM_GITHUB_AUTH_MODE", "direct")], &["get"]);
    assert!(!incomplete_direct.status.success());
    assert!(String::from_utf8(incomplete_direct.stderr)
        .unwrap()
        .contains("missing GH_TOKEN"));
}

#[test]
fn gh_wrapper_obeys_loom_owned_auth_mode() {
    let script = embedded_script("/usr/local/bin/gh").replace(
        "exec /usr/bin/gh \"$@\"",
        "printf 'GH_TOKEN=%s\\nGITHUB_TOKEN=%s\\n' \"$GH_TOKEN\" \"${GITHUB_TOKEN-unset}\"",
    );
    let output = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "broker"),
            ("LOOM_SESSION_ID", "session"),
            ("LOOM_TOKEN", "session-token"),
            ("GH_TOKEN", "wrong-daemon-bot"),
            ("GITHUB_TOKEN", "also-wrong"),
        ],
        &["auth", "status"],
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "GH_TOKEN=broker-token\nGITHUB_TOKEN=unset\n"
    );

    let direct = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "direct"),
            ("GH_TOKEN", "personal-token"),
            ("GITHUB_TOKEN", "wrong-daemon-bot"),
        ],
        &["auth", "status"],
    );
    assert!(direct.status.success());
    assert_eq!(
        String::from_utf8(direct.stdout).unwrap(),
        "GH_TOKEN=personal-token\nGITHUB_TOKEN=unset\n"
    );

    let unmarked = run_script(
        &script,
        &[("GH_TOKEN", "wrong-daemon-bot")],
        &["auth", "status"],
    );
    assert!(!unmarked.status.success());
    assert!(String::from_utf8(unmarked.stderr)
        .unwrap()
        .contains("missing LOOM_GITHUB_AUTH_MODE"));

    let incomplete_direct = run_script(
        &script,
        &[("LOOM_GITHUB_AUTH_MODE", "direct")],
        &["auth", "status"],
    );
    assert!(!incomplete_direct.status.success());
    assert!(String::from_utf8(incomplete_direct.stderr)
        .unwrap()
        .contains("missing GH_TOKEN"));

    let disabled = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "disabled"),
            ("GH_TOKEN", "wrong-daemon-bot"),
        ],
        &["auth", "status"],
    );
    assert!(!disabled.status.success());
    assert!(String::from_utf8(disabled.stderr)
        .unwrap()
        .contains("disabled for this Loom session"));
}
