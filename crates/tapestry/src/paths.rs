//! Where a session's control socket lives.
//!
//! One unix-domain socket per session, named by the session, under a per-machine
//! run directory. The directory derives from `weaver_home()` (honouring
//! `$WEAVER_HOME`), so the test harnesses that already pin `WEAVER_HOME` to a
//! temp dir get socket isolation for free — the tapestry analogue of
//! `WEAVER_TMUX_SOCKET`. `$WEAVER_TAPESTRY_DIR` overrides the directory outright
//! for callers that want to place sockets elsewhere.

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
/// The name is used verbatim as the file stem; callers pass the same opaque
/// session id loom already mints (`weaver-<id>`), which contains no path
/// separators.
pub fn socket_path(name: &str) -> PathBuf {
    run_dir().join(format!("{name}.sock"))
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
