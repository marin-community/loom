//! Where a session's control socket lives.
//!
//! One unix-domain socket per session, named by the session, under a per-machine
//! run directory. The directory derives from `weaver_home()` (honouring
//! `$WEAVER_HOME`), so the test harnesses that already pin `WEAVER_HOME` to a
//! temp dir get socket isolation for free. `$WEAVER_TAPESTRY_DIR` overrides the
//! directory outright for callers that want to place sockets elsewhere.

use std::path::PathBuf;

/// Directory holding every session's control socket on this machine.
pub fn run_dir() -> PathBuf {
    if let Ok(p) = std::env::var("WEAVER_TAPESTRY_DIR") {
        return PathBuf::from(p);
    }
    weaver_core::db::weaver_home().join("sock")
}

/// The control-socket path for a session name.
///
/// Callers pass the opaque session id loom already mints (`weaver-<id>`), which
/// contains no path separators. The name is still [`sanitize`]d to a single path
/// component as defence-in-depth, so a hostile name can never escape [`run_dir`]
/// — and because the sanitizer is deterministic, the supervisor that binds and
/// the client that connects derive the identical path.
pub fn socket_path(name: &str) -> PathBuf {
    run_dir().join(format!("{}.sock", sanitize(name)))
}

/// The relay frame-spool directory for a session — a directory of segment files
/// (`<first-seq>.seg`) living beside the control socket. Only relay-mode
/// supervisors create it; it outlives *client* restarts (not supervisor
/// restarts), so the frames a dead subscriber missed can be replayed.
pub fn spool_dir(name: &str) -> PathBuf {
    run_dir().join(format!("{}.spool", sanitize(name)))
}

/// The relay stderr log for a session — the child's stderr, appended verbatim,
/// beside the control socket.
pub fn stderr_log_path(name: &str) -> PathBuf {
    run_dir().join(format!("{}.stderr.log", sanitize(name)))
}

/// Reduce a session name to a single safe filename component: ASCII
/// alphanumerics plus `-`/`_` survive, everything else (path separators, `.`,
/// NUL, …) becomes `_`. This forecloses `../` traversal and absolute-path
/// injection through the name.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// List the names of every session that currently has a socket file. A socket's
/// presence does not prove the supervisor is alive (it may be stale after a
/// crash); callers confirm with a `Ping`.
pub fn list_socket_names() -> Vec<String> {
    let dir = run_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".sock").map(str::to_string)
        })
        .collect()
}
