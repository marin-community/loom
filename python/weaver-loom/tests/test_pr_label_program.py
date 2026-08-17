"""PR labeller behavior with no GitHub or loom server required."""

import importlib.util
import json
from pathlib import Path

from weaver_loom import Round, WeaverError


PROGRAM = (
    Path(__file__).resolve().parents[3]
    / "crates"
    / "loom-watch"
    / "watches"
    / "pr_label.py"
)
spec = importlib.util.spec_from_file_location("pr_label", PROGRAM)
pr_label = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pr_label)


class StubClient:
    def __init__(self, capabilities=None, sessions=None, error=None):
        self.capabilities = capabilities or []
        self._sessions = sessions or []
        self.error = error
        self.calls = []

    def can(self, capability):
        return capability == "observe" or capability in self.capabilities

    def sessions(self):
        return self._sessions

    def add_pr_label(self, session, label):
        self.calls.append((session, label))
        if self.error:
            raise WeaverError(self.error)


def session(id, state="OPEN", pr=17):
    return {
        "id": id,
        "status": "running",
        "branch": {"github": {"pr_state": state, "pr_number": pr}},
    }


def run(client, capsys, **config):
    rnd = Round(
        config={
            "name": "pr-label",
            "capabilities": client.capabilities,
            **config,
        },
        client=client,
    )
    pr_label.main(rnd)
    return json.loads(capsys.readouterr().out.strip().splitlines()[-1])


def test_labels_each_triggered_open_pr(capsys):
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("open", pr=17), session("merged", state="MERGED", pr=18)],
    )
    result = run(client, capsys)

    assert client.calls == [("open", "weaver")]
    assert result["actions"] == [
        {"action": "label", "session": "open", "pr": 17, "label": "weaver"}
    ]
    assert result["summary"] == "surveyed 2, labelled 1 open PR(s)"


def test_without_mark_capability_reports_the_action(capsys):
    client = StubClient(sessions=[session("s")])
    result = run(client, capsys, params={"label": "loom"})

    assert client.calls == []
    assert result["actions"] == [
        {
            "would": "label",
            "session": "s",
            "pr": 17,
            "label": "loom",
            "note": "mark capability not granted",
        }
    ]
    assert result["summary"] == "surveyed 1, labelled 0 open PR(s), 1 pending"


def test_dry_run_never_writes(capsys):
    client = StubClient(capabilities=["mark"], sessions=[session("s")])
    result = run(client, capsys, dry_run=True)

    assert client.calls == []
    assert result["actions"][0]["would"] == "label"
    assert result["actions"][0]["note"] == "dry run"
    assert result["summary"].endswith("(dry run, no writes applied)")


def test_github_failure_is_reported_without_crashing_the_round(capsys):
    client = StubClient(
        capabilities=["mark"], sessions=[session("s")], error="GitHub unavailable"
    )
    result = run(client, capsys)

    assert result["outcome"] == "ok"
    assert result["actions"][0]["would"] == "label"
    assert "GitHub unavailable" in result["actions"][0]["note"]
