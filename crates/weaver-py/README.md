# weaver-py

A Pythonic, synchronous wrapper over [`weaver-api`](../weaver-api) — drive the
loom fleet from Python through the same typed REST surface as the CLIs, never
through sqlite or the terminal runtime directly. The daemon stays the single
owner of live state. Loom's watch executor uses the separate stdlib-only
[`weaver_loom`](../../python/weaver-loom) package that it can vendor into script
subprocesses.

## Install

The module is a PyO3 extension built with [maturin](https://www.maturin.rs/).
It is kept out of the Cargo workspace (it links libpython), so build it on its
own:

```bash
# Editable install into the active virtualenv (fast iteration):
pip install maturin
maturin develop --manifest-path crates/weaver-py/Cargo.toml

# …or build a portable wheel and install that:
maturin build --release --manifest-path crates/weaver-py/Cargo.toml
pip install target/wheels/weaver_py-*.whl
```

The wheel is `abi3` for CPython ≥ 3.9, so one build runs on any supported Python.

## Use

```python
import weaver_py

# observe-only by default; grant the rungs of the ladder you need
c = weaver_py.Client(base="http://127.0.0.1:7878",
                     capabilities=["observe", "mark", "nudge"])

for s in c.sessions():
    branch = s["branch"]
    # Status lives in `branch["tags"]`, a list of {key, value, note, set_by,
    # set_at}. The well-known keys are `attention` (the agent's self-report) and
    # `triage` (a watch's assessment); a missing key means calm.
    tags = {t["key"]: t["value"] for t in branch["tags"]}
    print(s["id"], branch["title"], branch["description"],
          tags.get("attention", "calm"), tags.get("triage", "calm"))

s = c.session("abc123")            # by id, branch id, branch name, or repo:branch
screen = c.preview("abc123", 200)  # terminal as text, 200 lines of scrollback
changes = c.changes("abc123")      # typed, bounded worktree changes

c.set_tag("abc123", "triage", "attention", note="stuck on tests")  # needs "mark"
c.clear_tag("abc123", "triage")                          # back to calm; needs "mark"
c.mark("abc123", level="attention", note="stuck on tests")  # triage-tag shortcut; "mark"
c.nudge("abc123", "try running the tests again")            # needs "nudge"
c.interrupt("abc123")                                       # needs "interrupt"
```

`base` defaults to `$WEAVER_API` (a URL or a bare `host:port`), falling back to
the loom default `http://127.0.0.1:7878`. So a bare `weaver_py.Client()` targets
the local daemon.

Responses cross into Python as plain dicts/lists (via `serde_json` →
`pythonize`), so you read `s["branch"]["tags"]` directly; there is no wrapper
class to learn per response type. A type stub (`python/weaver_py.pyi`) ships the
surface for editors and type checkers.

## The capability model

Acting on *other people's* sessions is a loaded gun, so the binding enforces the
**intervention ladder** — least-privilege, rung by rung. A `Client` is
constructed with its granted set; every mutating method gates on its capability
*before* any request leaves the process and raises `CapabilityDenied` if the
grant is absent.

| Capability  | Gates                                    | Default  |
|-------------|------------------------------------------|----------|
| `observe`   | all reads (`sessions`/`session`/`preview`/`diff`) | always on |
| `mark`      | write a tag (`set_tag`/`clear_tag`/`mark`) | opt-in   |
| `escalate`  | raise the watch's own attention     | opt-in   |
| `nudge`     | `nudge` — type a message into a session  | opt-in   |
| `interrupt` | `interrupt` — break the agent's turn     | opt-in   |
| `launch`    | spawn new sessions (highest privilege)   | opt-in   |

`observe` is implicit — read methods always work, even with an empty grant.
`c.can("nudge")` reports whether a capability is held, mirroring the engine's
`ov.can(...)` so a program can branch on its own grants.

The gate itself is a pure function, `weaver_api::capability::require`, unit-tested
in the `weaver-api` crate (the workspace `test` job) — the security-relevant
core lives below the pyo3 glue, not buried in it.

## Example

[`examples/fleet_status.py`](examples/fleet_status.py) is the acceptance demo —
query the fleet and, capabilities permitting, mark a session:

```bash
python crates/weaver-py/examples/fleet_status.py
python crates/weaver-py/examples/fleet_status.py --mark <session> attention "looks stuck"
```

## Tests

```bash
maturin develop --manifest-path crates/weaver-py/Cargo.toml
pytest crates/weaver-py/tests
```

`tests/test_capabilities.py` is the security contract: it needs no server and is
the CI coverage — it asserts each mutating method raises when its capability is
absent, before any request is made.

`tests/test_live_roundtrip.py` is a live round-trip against a **fully-isolated**
loom (its own temp `WEAVER_HOME` / `WEAVER_DB`, its own terminal sockets under that home,
an ephemeral port, torn down on exit — it never touches your real loom). It is
opt-in:

```bash
cargo build -p loom            # the live test needs the `loom` binary
WEAVER_PY_LIVE=1 pytest crates/weaver-py/tests/test_live_roundtrip.py -v
```
