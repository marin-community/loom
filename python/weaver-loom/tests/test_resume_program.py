"""The builtin resume program's decision logic, with no server required.

Loads `crates/loom/overlookers/resume.py` straight from the repo and drives it
with a stubbed client and a frozen clock: the detection (screen pattern), the
exponential backoff cadence carried in the lookaside state, the dynamic-wake
scheduling, escalation after `max_attempts`, recovery, reactive-vs-sweep
candidate selection, the capability branches, and dry-run all live here. The
Rust integration suite keeps only the wiring proof — that the script runs under
the engine, detects a live screen, nudges, and persists state + wake.
"""

import importlib.util
import json
from pathlib import Path

from weaver_loom import Round

OVERLOOKERS = Path(__file__).resolve().parents[3] / "crates" / "loom" / "overlookers"


def load_program(name):
    spec = importlib.util.spec_from_file_location(name, OVERLOOKERS / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


resume = load_program("resume")

OVERLOAD = "  thinking...\n● API Error: 529 Overloaded. try again in a moment.\n> "
CALM = "user@host:~/work$ tests passed\n> "


class StubClient:
    """The slice of Client the resume program touches, scripted per test."""

    def __init__(self, capabilities=None, sessions=None, screens=None):
        self.capabilities = capabilities or []
        self._sessions = sessions or []
        self.screens = screens or {}
        self.calls = []

    def can(self, cap):
        return cap == "observe" or cap in self.capabilities

    def sessions(self):
        return self._sessions

    def preview(self, key, lines=0):
        self.calls.append(("preview", key, lines))
        return self.screens.get(key, "")

    def nudge(self, key, text, submit=True, by=None):
        self.calls.append(("nudge", key, text, by))

    def mark(self, key, level, note="", by=None):
        self.calls.append(("mark", key, level, note, by))


def sess(id, status="running"):
    return {"id": id, "status": status, "branch": {"tags": [], "repo_root": "/r"}}


def make_round(client, **config):
    return Round(config={"name": "resume", "capabilities": client.capabilities, **config}, client=client)


def run_main(client, capsys, monkeypatch, now, **config):
    """Drive main(rnd) with a frozen clock; return the parsed result line."""
    monkeypatch.setattr(resume.time, "time", lambda: now)
    rnd = make_round(client, **config)
    resume.main(rnd)
    return json.loads(capsys.readouterr().out.strip().splitlines()[-1])


def nudges(client):
    return [c for c in client.calls if c[0] == "nudge"]


def marks(client):
    return [c for c in client.calls if c[0] == "mark"]


# -- backoff math -------------------------------------------------------------


def test_backoff_doubles_and_caps():
    conf = {"base": 30.0, "cap": 900.0}
    assert resume.backoff(conf, 1) == 30
    assert resume.backoff(conf, 2) == 60
    assert resume.backoff(conf, 3) == 120
    assert resume.backoff(conf, 4) == 240
    # 30 * 2**5 = 960, clamped to the 900s cap.
    assert resume.backoff(conf, 6) == 900
    assert resume.backoff(conf, 99) == 900


# -- detection + first nudge --------------------------------------------------


def test_first_detection_nudges_and_arms_backoff(capsys, monkeypatch):
    client = StubClient(
        capabilities=["nudge", "mark"],
        sessions=[sess("s")],
        screens={"s": OVERLOAD},
    )
    result = run_main(client, capsys, monkeypatch, now=1000, trigger={})
    assert nudges(client) == [("nudge", "s", "continue", "resume")]
    # First failure → one attempt, next recheck a base backoff (30s) out.
    assert result["state"]["s"]["attempts"] == 1
    assert result["state"]["s"]["next"] == 1030
    assert result["state"]["s"]["escalated"] is False
    # The dynamic wake is armed at the soonest pending recheck.
    assert result["wake_in"] == 30
    assert result["outcome"] == "ok"


def test_calm_session_is_never_nudged(capsys, monkeypatch):
    client = StubClient(capabilities=["nudge"], sessions=[sess("s")], screens={"s": CALM})
    result = run_main(client, capsys, monkeypatch, now=1000, trigger={})
    assert nudges(client) == []
    assert result["state"] == {}
    # Nothing pending → the wake is cleared.
    assert result["wake_in"] == 0
    assert result["outcome"] == "noop"


def test_pattern_and_nudge_text_are_configurable(capsys, monkeypatch):
    client = StubClient(
        capabilities=["nudge"],
        sessions=[sess("s")],
        screens={"s": "...KABOOM..."},
    )
    result = run_main(
        client, capsys, monkeypatch, now=0, trigger={},
        params={"pattern": "KABOOM", "nudge": "carry on"},
    )
    assert nudges(client) == [("nudge", "s", "carry on", "resume")]
    assert "s" in result["state"]


# -- backoff cadence across rounds --------------------------------------------


def test_inside_backoff_window_waits_without_nudging(capsys, monkeypatch):
    # Still stalled, but the recheck time hasn't arrived → wait, don't re-prompt.
    prior = {"s": {"attempts": 1, "next": 1030, "escalated": False}}
    client = StubClient(capabilities=["nudge"], sessions=[sess("s")], screens={"s": OVERLOAD})
    result = run_main(client, capsys, monkeypatch, now=1010, trigger={}, state=prior)
    assert nudges(client) == []
    assert result["state"]["s"]["attempts"] == 1, "attempt count unchanged while waiting"
    # The wake re-arms at the remaining backoff (1030 - 1010).
    assert result["wake_in"] == 20


def test_consecutive_stall_after_window_backs_off_further(capsys, monkeypatch):
    # The recheck arrived and it is STILL stalled → re-prompt and double the wait.
    prior = {"s": {"attempts": 1, "next": 1030, "escalated": False}}
    client = StubClient(capabilities=["nudge"], sessions=[sess("s")], screens={"s": OVERLOAD})
    result = run_main(client, capsys, monkeypatch, now=1030, trigger={}, state=prior)
    assert len(nudges(client)) == 1
    assert result["state"]["s"]["attempts"] == 2
    # Second failure → 60s backoff (30 * 2**1).
    assert result["state"]["s"]["next"] == 1090
    assert result["wake_in"] == 60


def test_recovery_clears_tracking(capsys, monkeypatch):
    prior = {"s": {"attempts": 2, "next": 1090, "escalated": False}}
    client = StubClient(capabilities=["nudge"], sessions=[sess("s")], screens={"s": CALM})
    result = run_main(client, capsys, monkeypatch, now=1100, trigger={}, state=prior)
    assert nudges(client) == []
    assert "s" not in result["state"], "a recovered session is dropped from tracking"
    assert result["wake_in"] == 0, "nothing left to recheck → wake cleared"


# -- escalation after max_attempts --------------------------------------------


def test_max_attempts_escalates_and_stops_nudging(capsys, monkeypatch):
    prior = {"s": {"attempts": 6, "next": 1000, "escalated": False}}
    client = StubClient(capabilities=["nudge", "mark"], sessions=[sess("s")], screens={"s": OVERLOAD})
    result = run_main(client, capsys, monkeypatch, now=1000, trigger={}, state=prior)
    # No further nudge; instead a single escalation mark.
    assert nudges(client) == []
    assert len(marks(client)) == 1
    assert marks(client)[0][1:3] == ("s", "attention")
    assert result["state"]["s"]["escalated"] is True
    # It keeps a slow recheck (the cap) so the mark can clear on recovery.
    assert result["wake_in"] == 900


def test_escalated_session_is_not_marked_again(capsys, monkeypatch):
    prior = {"s": {"attempts": 6, "next": 1900, "escalated": True}}
    client = StubClient(capabilities=["nudge", "mark"], sessions=[sess("s")], screens={"s": OVERLOAD})
    run_main(client, capsys, monkeypatch, now=1900, trigger={}, state=prior)
    assert marks(client) == [], "an already-escalated session is not re-marked"
    assert nudges(client) == []


def test_escalated_recovery_clears_the_mark(capsys, monkeypatch):
    prior = {"s": {"attempts": 6, "next": 1900, "escalated": True}}
    client = StubClient(capabilities=["nudge", "mark"], sessions=[sess("s")], screens={"s": CALM})
    result = run_main(client, capsys, monkeypatch, now=2000, trigger={}, state=prior)
    assert marks(client) == [("mark", "s", "ok", "", "resume")], "recovery clears the triage mark"
    assert "s" not in result["state"]


# -- candidate selection: reactive vs sweep -----------------------------------


def test_reactive_trigger_scopes_to_the_triggering_session(capsys, monkeypatch):
    client = StubClient(
        capabilities=["nudge"],
        sessions=[sess("a"), sess("b")],
        screens={"a": OVERLOAD, "b": OVERLOAD},
    )
    result = run_main(client, capsys, monkeypatch, now=0, trigger={"session": "a"})
    # Only the triggering session is examined and nudged; "b" is untouched.
    assert [n[1] for n in nudges(client)] == ["a"]
    assert not any(c[0] == "preview" and c[1] == "b" for c in client.calls)
    assert "b" not in result["state"]


def test_sweep_checks_the_whole_fleet(capsys, monkeypatch):
    # A cron/manual tick (no triggering session) sweeps every session, catching
    # several the same outage hit.
    client = StubClient(
        capabilities=["nudge"],
        sessions=[sess("a"), sess("b")],
        screens={"a": OVERLOAD, "b": OVERLOAD},
    )
    result = run_main(client, capsys, monkeypatch, now=0, trigger={})
    assert sorted(n[1] for n in nudges(client)) == ["a", "b"]
    assert set(result["state"]) == {"a", "b"}


def test_a_tracked_session_that_vanished_is_forgotten(capsys, monkeypatch):
    # "ghost" is tracked but no longer in the live fleet → dropped silently.
    prior = {"ghost": {"attempts": 2, "next": 500, "escalated": False}}
    client = StubClient(capabilities=["nudge"], sessions=[sess("s")], screens={"s": CALM})
    result = run_main(client, capsys, monkeypatch, now=1000, trigger={}, state=prior)
    assert "ghost" not in result["state"]
    assert result["state"] == {}


# -- dry-run + capability gating ----------------------------------------------


def test_dry_run_records_would_and_mutates_nothing(capsys, monkeypatch):
    client = StubClient(
        capabilities=["nudge", "mark"],
        sessions=[sess("s")],
        screens={"s": OVERLOAD},
    )
    result = run_main(client, capsys, monkeypatch, now=1000, trigger={}, dry_run=True)
    assert nudges(client) == [] and marks(client) == []
    assert {"would": "nudge", "session": "s", "text": "continue"} in result["actions"]
    # The backoff still advances and the wake is still armed in a dry run.
    assert result["state"]["s"]["attempts"] == 1
    assert result["wake_in"] == 30
    assert "dry run" in result["summary"]


def test_without_nudge_capability_only_observes(capsys, monkeypatch):
    client = StubClient(capabilities=[], sessions=[sess("s")], screens={"s": OVERLOAD})
    result = run_main(client, capsys, monkeypatch, now=1000, trigger={})
    assert nudges(client) == []
    assert any(
        a.get("would") == "nudge" and "not granted" in (a.get("note") or "")
        for a in result["actions"]
    )


def test_max_attempts_without_mark_capability_records_would(capsys, monkeypatch):
    prior = {"s": {"attempts": 6, "next": 1000, "escalated": False}}
    client = StubClient(capabilities=["nudge"], sessions=[sess("s")], screens={"s": OVERLOAD})
    result = run_main(client, capsys, monkeypatch, now=1000, trigger={}, state=prior)
    assert marks(client) == []
    assert any(a.get("would") == "escalate" for a in result["actions"])
