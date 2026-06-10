"""Live round-trip against a fully-isolated loom server.

Skipped by default. It is opt-in (set `WEAVER_PY_LIVE=1`) because it spawns the
`loom` daemon, and the automated CI coverage is the server-free gating suite in
`test_capabilities.py`.

Safety — this NEVER touches the user's loom. It mirrors the Rust integration
harness (`crates/loom/tests/integration/fixtures.rs`) exactly:

  * a temp `WEAVER_HOME` (and so its own `server.json`),
  * a temp `WEAVER_DB` under it (its own SQLite file),
  * a UNIQUE `WEAVER_TMUX_SOCKET` under the temp dir (a private tmux server),
  * an ephemeral 127.0.0.1 port chosen by binding `:0`,

and it kills the server + tears the tmux socket down on teardown.

What it asserts is the parts that are reproducible without an agent runtime:
the binding reaches the real server and decodes its typed responses
(`sessions()` -> []), and gated mutating calls that the capability permits
(`set_tag`/`clear_tag`/`mark`) reach the server and surface the server's error
as `WeaverError` (tagging a session that does not exist). Creating a real
session launches an agent (`claude`) under tmux, which is not reproducible in
CI, so the full create-then-tag round-trip is left to a human running this
locally against a seeded session.

Run it locally with:

    WEAVER_PY_LIVE=1 pytest crates/weaver-py/tests/test_live_roundtrip.py -v
"""

import os
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

import weaver_py

LIVE = os.environ.get("WEAVER_PY_LIVE") == "1"

pytestmark = pytest.mark.skipif(
    not LIVE, reason="live test is opt-in: set WEAVER_PY_LIVE=1 (spawns an isolated loom)"
)


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _loom_binary() -> str:
    # Allow an explicit override; otherwise look for a built debug/release binary
    # under the workspace target dir (the repo root is four parents up from this
    # test file: crates/weaver-py/tests/<file>).
    if env := os.environ.get("WEAVER_PY_LOOM_BIN"):
        return env
    root = Path(__file__).resolve().parents[3]
    for profile in ("debug", "release"):
        cand = root / "target" / profile / "loom"
        if cand.exists():
            return str(cand)
    pytest.skip(
        "no `loom` binary found under target/{debug,release}; "
        "build it (`cargo build -p loom`) or set WEAVER_PY_LOOM_BIN"
    )


@pytest.fixture
def isolated_loom():
    """Boot a fully-isolated loom server and yield its base URL."""
    binary = _loom_binary()
    with tempfile.TemporaryDirectory(prefix="weaver-py-live-") as home:
        home_path = Path(home)
        port = _free_port()
        addr = f"127.0.0.1:{port}"
        socket_name = str(home_path / "tmux.sock")

        env = dict(os.environ)
        env["WEAVER_HOME"] = str(home_path)
        env["WEAVER_DB"] = str(home_path / "weaver.db")
        env["WEAVER_TMUX_SOCKET"] = socket_name
        env["WEAVER_API"] = f"http://{addr}"

        proc = subprocess.Popen(
            [binary, "serve", "--addr", addr],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            base = f"http://{addr}"
            # Poll until the server answers (or the process dies).
            deadline = time.time() + 30
            up = False
            while time.time() < deadline:
                if proc.poll() is not None:
                    raise RuntimeError("loom serve exited during startup")
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                        up = True
                        break
                except OSError:
                    time.sleep(0.1)
            if not up:
                raise RuntimeError("loom serve never came up")
            # Give the HTTP stack a beat past the TCP accept.
            time.sleep(0.3)
            yield base
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
            # Tear the private tmux server down (best effort).
            subprocess.run(
                ["tmux", "-S", socket_name, "kill-server"],
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )


def test_sessions_decodes_an_empty_fleet(isolated_loom):
    c = weaver_py.Client(base=isolated_loom, capabilities=["observe"])
    sessions = c.sessions()
    assert sessions == [], "a fresh isolated server has no sessions"


def test_set_tag_reaches_the_server_and_surfaces_its_error(isolated_loom):
    # Capability present, so the gate passes; the session does not exist, so the
    # server errors and the binding maps it to WeaverError (not CapabilityDenied).
    c = weaver_py.Client(base=isolated_loom, capabilities=["observe", "mark"])
    with pytest.raises(weaver_py.WeaverError):
        c.set_tag("does-not-exist", "triage", "attention", "smoke test")


def test_clear_tag_reaches_the_server_and_surfaces_its_error(isolated_loom):
    c = weaver_py.Client(base=isolated_loom, capabilities=["observe", "mark"])
    with pytest.raises(weaver_py.WeaverError):
        c.clear_tag("does-not-exist", "triage")


def test_mark_reaches_the_server_and_surfaces_its_error(isolated_loom):
    # The `triage`-tag convenience over set_tag, same gate and same round-trip.
    c = weaver_py.Client(base=isolated_loom, capabilities=["observe", "mark"])
    with pytest.raises(weaver_py.WeaverError):
        c.mark("does-not-exist", "attention", "smoke test")


def test_set_tag_without_capability_never_reaches_the_server(isolated_loom):
    c = weaver_py.Client(base=isolated_loom, capabilities=["observe"])
    with pytest.raises(weaver_py.CapabilityDenied):
        c.set_tag("does-not-exist", "triage", "attention", "smoke test")
