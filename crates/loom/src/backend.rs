//! The terminal-management seam.
//!
//! Loom's programmatic terminal operations — create a session, check liveness,
//! capture the screen, type into it, kill it — go through here rather than
//! poking the supervisor directly, so the call sites read uniformly and the
//! `tapestry`-specific glue (binary resolution, key-name → bytes) lives in one
//! place.
//!
//! Every session is a [`tapestry`] supervisor: a per-session detached PTY process
//! that streams raw PTY bytes, so an attached xterm owns its own scrollback,
//! selection, and search. Its lifetime is independent of `loom server run`, so a loom
//! restart leaves running terminals untouched — the recovery property that keeps
//! agents alive across restarts.

use anyhow::{bail, Result};

use crate::{db::Db, runner};

/// Whether a session with this name has a live supervisor.
pub async fn has_session(name: &str) -> bool {
    tapestry::Client::is_alive(name).await
}

/// The delegated cgroup-v2 subtree sessions confine themselves under. Created
/// and chowned to the loom user at container boot by `loom-cgroup-init` (see
/// the Dockerfile); absent (the normal case outside the standalone deploy)
/// the [`memory_prelude`] guard makes the confinement a no-op.
const AGENT_CGROUP_DIR: &str = "/sys/fs/cgroup/agents";

/// The per-session memory ceiling in GiB, from the `session.memory_max_gb`
/// setting. 0 = unlimited; the setting validates as a signed integer, so a
/// stored negative clamps to 0 rather than silently reverting to the default.
pub async fn memory_max_gb(db: &Db) -> u64 {
    let default = weaver_core::config::DEFAULT_SESSION_MEMORY_MAX_GB;
    weaver_core::config::get_or(db, "session.memory_max_gb", &default.to_string())
        .await
        .trim()
        .parse::<i64>()
        .map(|v| v.max(0) as u64)
        .unwrap_or(default as u64)
}

/// Shell prelude that moves the session into its own memory-limited cgroup
/// before the launch script proper runs — every process the session spawns
/// inherits it, so a runaway agent OOMs alone inside its cgroup instead of
/// taking the host down. Runs only where [`AGENT_CGROUP_DIR`] is writable (the
/// delegated subtree the standalone deploy prepares at boot); elsewhere the
/// guard falls through silently. A *failed* confinement attempt, by contrast,
/// warns into the session terminal so an unlimited session is visible.
fn memory_prelude(name: &str, memory_max_gb: u64) -> String {
    let dir = format!("{AGENT_CGROUP_DIR}/{name}");
    let bytes = memory_max_gb.saturating_mul(1024 * 1024 * 1024);
    // memory.swap.max is zeroed so the ceiling can't leak into swap on a host
    // that has any, but best-effort: the file is missing on kernels without
    // swap accounting, and the RAM cap alone is still worth keeping.
    format!(
        "if [ -w {AGENT_CGROUP_DIR} ]; then \
if mkdir -p '{dir}' 2>/dev/null && echo {bytes} > '{dir}/memory.max' 2>/dev/null \
&& echo $$ > '{dir}/cgroup.procs' 2>/dev/null; then \
echo 0 > '{dir}/memory.swap.max' 2>/dev/null || true; else \
echo 'loom: warning: could not apply the {memory_max_gb}g session memory limit' >&2; fi; fi\n"
    )
}

/// Create a detached session running `script` via `sh -c` in `cwd`, with `env`
/// applied to the child's process environment. A non-zero `memory_max_gb`
/// prepends the [`memory_prelude`] confining the session to that many GiB.
///
/// `env` is delivered out of band (over stdin to the supervisor, then via
/// `execve`), not `export`-ed into `script`, so secret values never appear on
/// any process's argv. See [`tapestry::spawn_detached`].
pub async fn new_session(
    name: &str,
    cwd: &std::path::Path,
    script: &str,
    env: &[(&str, &str)],
    env_clear: bool,
    memory_max_gb: u64,
) -> Result<()> {
    tracing::info!(session = %name, cwd = %cwd.display(), memory_max_gb, "spawning terminal session");
    let script = match memory_max_gb {
        0 => script.to_string(),
        gb => format!("{}{}", memory_prelude(name, gb), script),
    };
    let supervisor_bin = tapestry_bin();
    let options = tapestry::LaunchOptions {
        name,
        cwd,
        script: &script,
        env,
        env_clear,
        cols: 80,
        rows: 24,
        mode: tapestry::Mode::Pty,
        segment_max_bytes: None,
        supervisor_bin: supervisor_bin.as_deref(),
    };
    let result = runner::spawn(&options, memory_max_gb).await;
    match &result {
        Ok(()) => tracing::info!(session = %name, "terminal session spawned"),
        Err(e) => tracing::warn!(session = %name, error = %e, "failed to spawn terminal session"),
    }
    result
}

/// Create a detached **relay** session: the supervisor spawns `script` via
/// `sh -c` in `cwd` over plain pipes (no PTY), spooling the child's stdout as
/// newline-delimited frames for a subscriber to replay/stream. The seam for ACP
/// agents; PTY sessions keep [`new_session`].
///
/// `env` is delivered out of band exactly as [`new_session`] does it, so secret
/// values never touch argv. [`has_session`]/[`kill_session`] work unchanged for
/// relay sessions (the control socket is the same).
pub async fn new_relay_session(
    name: &str,
    script: &str,
    env: &[(&str, &str)],
    env_clear: bool,
    cwd: &std::path::Path,
    memory_max_gb: u64,
) -> Result<()> {
    tracing::info!(session = %name, cwd = %cwd.display(), memory_max_gb, "spawning relay session");
    let supervisor_bin = tapestry_bin();
    let options = tapestry::LaunchOptions {
        name,
        cwd,
        script,
        env,
        env_clear,
        cols: 80,
        rows: 24,
        mode: tapestry::Mode::Relay,
        segment_max_bytes: None,
        supervisor_bin: supervisor_bin.as_deref(),
    };
    let result = runner::spawn(&options, memory_max_gb).await;
    match &result {
        Ok(()) => tracing::info!(session = %name, "relay session spawned"),
        Err(e) => tracing::warn!(session = %name, error = %e, "failed to spawn relay session"),
    }
    result
}

/// Subscribe to a relay session's frame stream from `cursor`: the returned
/// [`tapestry::RelayStream`] replays every spooled frame with `seq > cursor`,
/// then streams live frames, then a terminal `Exit`. Only one subscriber exists
/// at a time — this evicts any previous one.
pub async fn subscribe_relay(name: &str, cursor: u64) -> Result<tapestry::RelayStream> {
    tapestry::Client::connect(name)
        .await?
        .subscribe(cursor)
        .await
}

/// Append raw bytes to a relay session's child stdin (complete newline-terminated
/// frames; the relay writes them through untouched).
pub async fn relay_write(name: &str, bytes: &[u8]) -> Result<()> {
    let mut c = tapestry::Client::connect(name).await?;
    c.relay_write(bytes).await
}

/// Advance a relay session's retention watermark to `seq` — everything up to and
/// including it has been durably processed, so fully-acked spool segments can be
/// dropped.
pub async fn relay_ack(name: &str, seq: u64) -> Result<()> {
    let mut c = tapestry::Client::connect(name).await?;
    c.relay_ack(seq).await
}

/// Render the session's screen to text; `history` extra scrollback lines.
pub async fn capture(name: &str, history: usize) -> Result<String> {
    let mut c = tapestry::Client::connect(name).await?;
    c.capture(history as u32).await
}

/// Type `text` into the session verbatim (the `send-keys -l` analogue). No
/// trailing newline; pair with [`send_enter`] to submit.
pub async fn send_literal(name: &str, text: &str) -> Result<()> {
    tracing::debug!(session = %name, bytes = text.len(), "sending literal input to terminal");
    let mut c = tapestry::Client::connect(name).await?;
    c.send(text.as_bytes()).await
}

/// Bracketed-paste framing (DECSET 2004), exactly what a terminal emulator emits
/// around a paste. The markers tell the TUI "this block is content, not
/// keystrokes", so a following [`send_enter`] is read as a distinct submit.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Deliver `text` as a single bracketed-paste block, then pair with [`send_enter`]
/// to submit — the reliable way to hand an agent a multi-line message.
///
/// Typing multi-line text verbatim ([`send_literal`]) and following it with a bare
/// `Enter` does not submit: an agent TUI (Claude Code) treats the burst as a paste
/// and folds the trailing `\r` into the composer as one more newline, so the whole
/// message lands in the entry box unsent. Wrapping the text in bracketed-paste
/// markers is how the interactive terminal avoids this — xterm.js frames every
/// paste the same way — and the closing marker ends the paste so the subsequent
/// `Enter` counts as a submit. Newlines are normalized to `\r`, matching xterm.js.
///
/// Loom only drives agents that enable bracketed paste; where 2004 mode is off the
/// markers would land as literal text, so this is not a general typing primitive.
pub async fn paste(name: &str, text: &str) -> Result<()> {
    let framed = frame_paste(text);
    tracing::debug!(session = %name, bytes = framed.len(), "pasting input to terminal");
    let mut c = tapestry::Client::connect(name).await?;
    c.send(&framed).await
}

/// Wrap `text` in bracketed-paste markers, normalizing newlines to `\r` first
/// (what xterm.js does on paste). Split out from [`paste`] so the framing is
/// unit-testable without a live terminal.
fn frame_paste(text: &str) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut framed = Vec::with_capacity(PASTE_START.len() + normalized.len() + PASTE_END.len());
    framed.extend_from_slice(PASTE_START);
    framed.extend_from_slice(normalized.as_bytes());
    framed.extend_from_slice(PASTE_END);
    framed
}

/// Send a single named key (e.g. `Enter`, `Escape`) to the session.
pub async fn send_key(name: &str, key: &str) -> Result<()> {
    let bytes = key_bytes(key);
    tracing::debug!(session = %name, bytes = bytes.len(), "sending key to terminal");
    let mut c = tapestry::Client::connect(name).await?;
    c.send(bytes).await
}

/// Submit the current input — a bare `Enter`.
pub async fn send_enter(name: &str) -> Result<()> {
    tracing::debug!(session = %name, "submitting terminal input");
    send_key(name, "Enter").await
}

/// Kill the session. Best-effort: a missing supervisor means already gone.
pub async fn kill_session(name: &str) -> Result<()> {
    if let Ok(mut c) = tapestry::Client::connect(name).await {
        match c.kill().await {
            Ok(()) => tracing::info!(name = %name, "terminal killed"),
            Err(e) => tracing::warn!(name = %name, error = %e, "failed to kill terminal"),
        }
    } else {
        runner::remove(name).await?;
    }
    Ok(())
}

/// Kill a session and wait until its supervisor has released the control socket.
///
/// A kill request is acknowledged before the supervisor finishes teardown. A
/// caller that immediately reuses the same name (provider handoff does this)
/// must wait, or the replacement can connect to the dying supervisor.
pub async fn kill_session_and_wait(name: &str) -> Result<()> {
    kill_session(name).await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while has_session(name).await {
        if tokio::time::Instant::now() >= deadline {
            bail!("terminal {name} did not stop within 5 seconds");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    runner::remove(name).await?;
    Ok(())
}

/// Every session with a live supervisor.
pub async fn list_sessions() -> Result<Vec<String>> {
    Ok(tapestry::list_sessions().await)
}

/// The `tapestry` supervisor binary, resolved as a sibling of the running `loom`
/// executable (they ship together), so the detached supervisor is the real
/// `tapestry` binary rather than `loom` (whose `current_exe` lacks a `supervise`
/// subcommand). `None` falls back to `tapestry` on `PATH`. An explicit
/// `WEAVER_TAPESTRY_BIN` overrides both — used by the integration tests, whose
/// `loom` and `tapestry` are built into the same target dir.
fn tapestry_bin() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("WEAVER_TAPESTRY_BIN") {
        return Some(std::path::PathBuf::from(p));
    }
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(|d| d.join("tapestry"))
        .filter(|p| p.exists())
}

/// Translate the small set of key names loom uses into the raw bytes a PTY
/// expects. Unknown names fall through as their literal text.
fn key_bytes(key: &str) -> &[u8] {
    match key {
        "Enter" => b"\r",
        "Escape" => b"\x1b",
        "Tab" => b"\t",
        "Space" => b" ",
        "BSpace" => b"\x7f",
        other => other.as_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_paste_wraps_in_bracketed_markers_and_normalizes_newlines() {
        let framed = frame_paste("line one\nline two\r\nline three");
        let s = String::from_utf8(framed).unwrap();
        // The block is delimited so the TUI reads it as content and the following
        // Enter as a distinct submit — the whole point of the fix.
        assert!(
            s.starts_with("\x1b[200~"),
            "missing paste-start marker: {s:?}"
        );
        assert!(s.ends_with("\x1b[201~"), "missing paste-end marker: {s:?}");
        // Every newline (both LF and CRLF) collapses to a single CR, matching xterm.js,
        // and no stray LF survives inside the block.
        assert_eq!(
            s, "\x1b[200~line one\rline two\rline three\x1b[201~",
            "newlines should normalize to a single CR"
        );
        assert!(
            !s.contains('\n'),
            "no LF should remain inside the paste block"
        );
    }

    #[test]
    fn memory_prelude_confines_the_session_to_its_named_cgroup() {
        let prelude = memory_prelude("weaver-abc123", 8);
        // Guarded on the delegated subtree, so it no-ops where none exists.
        assert!(prelude.starts_with("if [ -w /sys/fs/cgroup/agents ]; then "));
        // The limit lands in this session's own cgroup, in bytes.
        assert!(prelude.contains("mkdir -p '/sys/fs/cgroup/agents/weaver-abc123'"));
        assert!(
            prelude.contains("echo 8589934592 > '/sys/fs/cgroup/agents/weaver-abc123/memory.max'")
        );
        // The shell moves itself in, so everything it spawns inherits the cap.
        assert!(prelude.contains("echo $$ > '/sys/fs/cgroup/agents/weaver-abc123/cgroup.procs'"));
        // The cap can't leak into swap (best-effort — see memory_prelude).
        assert!(prelude.contains("echo 0 > '/sys/fs/cgroup/agents/weaver-abc123/memory.swap.max'"));
        // A failed attempt is loud in the session terminal.
        assert!(prelude.contains("could not apply the 8g session memory limit"));
        // Newline-terminated so the launch script proper starts on its own line.
        assert!(prelude.ends_with("fi\n"));
    }

    #[tokio::test]
    async fn memory_max_gb_reads_the_setting_and_defaults_to_8() {
        let db = weaver_core::db::connect_in_memory().await.unwrap();
        assert_eq!(memory_max_gb(&db).await, 8);
        weaver_core::config::apply(&db, &[("session.memory_max_gb".into(), Some("12".into()))])
            .await
            .unwrap();
        assert_eq!(memory_max_gb(&db).await, 12);
        weaver_core::config::apply(&db, &[("session.memory_max_gb".into(), Some("0".into()))])
            .await
            .unwrap();
        assert_eq!(memory_max_gb(&db).await, 0);
        // A stored negative (the setting validates as a signed int) clamps to
        // unlimited rather than reverting to the default.
        weaver_core::config::apply(&db, &[("session.memory_max_gb".into(), Some("-1".into()))])
            .await
            .unwrap();
        assert_eq!(memory_max_gb(&db).await, 0);
    }

    #[test]
    fn memory_prelude_saturates_an_absurd_limit_instead_of_wrapping() {
        let prelude = memory_prelude("s", u64::MAX / 2);
        assert!(prelude.contains(&format!("echo {} > ", u64::MAX)));
    }
}
