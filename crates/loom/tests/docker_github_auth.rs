//! Regression coverage for the Git/GitHub CLI adapters authored in Dockerfile.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const DOCKERFILE: &str = include_str!("../../../Dockerfile");

/// The broker CLI stands in for `loom github-token` and echoes the repository
/// it was asked for, so a test can assert which scope the adapter requested.
const BROKER_STUB: &str = "loom_github_token() {
  if [ \"$1\" = --repository ]; then
    printf 'broker-token:%s\\n' \"$2\"
  else
    printf 'broker-token\\n'
  fi
}
";

fn embedded_script(path: &str) -> String {
    let start = format!("cat > {path} <<'SH'\n");
    let (_, tail) = DOCKERFILE
        .split_once(&start)
        .unwrap_or_else(|| panic!("missing {path} heredoc"));
    let (script, _) = tail
        .split_once("\nSH\n")
        .unwrap_or_else(|| panic!("unterminated {path} heredoc"));
    let body = script.replace("/usr/local/bin/loom github-token", "loom_github_token");
    format!("{BROKER_STUB}{body}")
}

fn run_script(script: &str, env: &[(&str, &str)], args: &[&str]) -> Output {
    run_credential_script(script, env, args, "")
}

/// Run one adapter from a file so its stdin carries the credential request git
/// would write, and from a directory that is not a git checkout so the `gh`
/// wrapper cannot pick up this repository's own origin.
fn run_credential_script(
    script: &str,
    env: &[(&str, &str)],
    args: &[&str],
    request: &str,
) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("adapter.sh");
    std::fs::write(&path, script).unwrap();
    let mut child = Command::new("sh")
        .arg(&path)
        .args(args)
        .current_dir(dir.path())
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
        .write_all(request.as_bytes())
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
            ("GH_TOKEN", "loom-stored-user-token"),
        ],
        &["get"],
    );
    assert!(direct.status.success());
    assert_eq!(
        String::from_utf8(direct.stdout).unwrap(),
        "username=x-access-token\npassword=loom-stored-user-token\n"
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

/// An installation token covers one owner, so the helper must ask for the
/// repository git named rather than the session's whole set. Without this a
/// session holding access under two owners can push to neither.
#[test]
fn git_helper_scopes_the_brokered_token_to_the_repository_git_asked_for() {
    let script = embedded_script("/usr/local/bin/git-credential-ghtoken");
    let broker = &[
        ("LOOM_GITHUB_AUTH_MODE", "broker"),
        ("LOOM_SESSION_ID", "session"),
        ("LOOM_TOKEN", "session-token"),
    ];

    let scoped = run_credential_script(
        &script,
        broker,
        &["get"],
        "protocol=https\nhost=github.com\npath=marin-community/vllm.git\n\n",
    );
    assert!(scoped.status.success());
    assert_eq!(
        String::from_utf8(scoped.stdout).unwrap(),
        "username=x-access-token\npassword=broker-token:marin-community/vllm\n"
    );

    // A different owner in the same session resolves independently.
    let other_owner = run_credential_script(
        &script,
        broker,
        &["get"],
        "protocol=https\nhost=github.com\npath=Open-Athena/mumwelt\n\n",
    );
    assert!(other_owner.status.success());
    assert_eq!(
        String::from_utf8(other_owner.stdout).unwrap(),
        "username=x-access-token\npassword=broker-token:Open-Athena/mumwelt\n"
    );

    // Git omits `path` unless credential.useHttpPath is set; fall back to the
    // session's whole set rather than failing.
    let unscoped = run_credential_script(
        &script,
        broker,
        &["get"],
        "protocol=https\nhost=github.com\n\n",
    );
    assert!(unscoped.status.success());
    assert_eq!(
        String::from_utf8(unscoped.stdout).unwrap(),
        "username=x-access-token\npassword=broker-token\n"
    );
}

/// The Dockerfile must enable `credential.useHttpPath` for github.com, or git
/// never sends the `path` the helper above scopes on.
#[test]
fn git_is_configured_to_send_the_repository_path_to_the_helper() {
    assert!(
        DOCKERFILE.contains("git config --system credential.https://github.com.useHttpPath true")
    );
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

    let scoped = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "broker"),
            ("LOOM_SESSION_ID", "session"),
            ("LOOM_TOKEN", "session-token"),
            ("GH_REPO", "marin-community/vllm"),
        ],
        &["pr", "create"],
    );
    assert!(scoped.status.success());
    assert_eq!(
        String::from_utf8(scoped.stdout).unwrap(),
        "GH_TOKEN=broker-token:marin-community/vllm\nGITHUB_TOKEN=unset\n"
    );

    let direct = run_script(
        &script,
        &[
            ("LOOM_GITHUB_AUTH_MODE", "direct"),
            ("GH_TOKEN", "loom-stored-user-token"),
            ("GITHUB_TOKEN", "wrong-daemon-bot"),
        ],
        &["auth", "status"],
    );
    assert!(direct.status.success());
    assert_eq!(
        String::from_utf8(direct.stdout).unwrap(),
        "GH_TOKEN=loom-stored-user-token\nGITHUB_TOKEN=unset\n"
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
