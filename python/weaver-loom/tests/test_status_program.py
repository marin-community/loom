"""The builtin status program's decision logic, with no server required.

Loads `crates/loom/overlookers/status.py` straight from the repo and drives
it with stubbed clients: the judge's prompt/fallback split, the ok-clears
rule, the capability branches, and the summary format all live here. The
Rust integration suite keeps only the wiring proof — that the script runs
under the engine against a live loom and its marks/audit rows land.

pr-label and archive-merged carry no logic beyond a one-line predicate over
the PR snapshot, so their coverage stays in the Rust end-to-end test
(`builtin_scripts_report_merged_and_unlabelled_prs`) — a pytest double of a
one-liner would be duplication, not coverage.
"""

import importlib.util
import json
from pathlib import Path

from weaver_loom import Round, WeaverError

OVERLOOKERS = Path(__file__).resolve().parents[3] / "crates" / "loom" / "overlookers"


def load_program(name):
    spec = importlib.util.spec_from_file_location(name, OVERLOOKERS / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


status = load_program("status")


class StubClient:
    """The slice of Client the status program touches, scripted per test."""

    def __init__(self, capabilities=None, sessions=None, agent_reply=None,
                 screen="the pane", nudge_fails=False):
        self.capabilities = capabilities or []
        self._sessions = sessions or []
        self.agent_reply = agent_reply
        self.screen = screen
        self.nudge_fails = nudge_fails
        self.calls = []

    def can(self, cap):
        return cap == "observe" or cap in self.capabilities

    def sessions(self):
        return self._sessions

    def preview(self, key, lines=0):
        self.calls.append(("preview", key))
        return self.screen

    def agent(self, prompt, model="", effort=""):
        self.calls.append(("agent", prompt, model, effort))
        return self.agent_reply

    def mark(self, key, level, note="", by=None):
        self.calls.append(("mark", key, level, note, by))

    def nudge(self, key, text, submit=True, by=None):
        if self.nudge_fails:
            raise WeaverError("no live terminal")
        self.calls.append(("nudge", key, text, by))


def session(id, attention=None):
    tags = [{"key": "attention", "value": attention}] if attention else []
    return {"id": id, "status": "running", "branch": {"tags": tags, "repo_root": "/r"}}


def make_round(client, **config):
    return Round(config={"name": "watch", **config}, client=client)


def run_main(client, capsys, **config):
    """Drive main(rnd) with a stubbed round; return the parsed result line."""
    rnd = make_round(client, capabilities=client.capabilities, **config)
    status.main(rnd)
    return json.loads(capsys.readouterr().out.strip().splitlines()[-1])


# -- judge: fallback rule vs prompt path ---------------------------------------


def test_judge_mirrors_attention_without_a_prompt():
    rnd = make_round(StubClient())
    assert status.judge(rnd, session("s", "blocked")) == ("blocked", "attention is blocked")
    assert status.judge(rnd, session("s")) == ("ok", "attention is ok")


def test_judge_prompt_path_asks_the_agent_with_screen_and_model():
    client = StubClient(agent_reply="blocked: stuck on a test")
    rnd = make_round(client, params={"prompt": "stuck?"}, model="haiku", effort="low")
    assert status.judge(rnd, session("s")) == ("blocked", "stuck on a test")
    kind, prompt, model, effort = client.calls[-1]
    assert (kind, model, effort) == ("agent", "haiku", "low")
    assert prompt.startswith("stuck?") and "the pane" in prompt


def test_judge_falls_back_when_agent_is_absent_or_unparseable():
    for reply in (None, "no verdict here"):
        rnd = make_round(StubClient(agent_reply=reply), params={"prompt": "stuck?"})
        assert status.judge(rnd, session("s", "attention")) == (
            "attention",
            "attention is attention",
        )


def test_judge_survives_a_dead_pane():
    class NoPane(StubClient):
        def preview(self, key, lines=0):
            raise WeaverError("409 no live terminal")

    rnd = make_round(NoPane(agent_reply="ok - all quiet"), params={"prompt": "stuck?"})
    assert status.judge(rnd, session("s")) == ("ok", "all quiet")


# -- main: marking, clearing, capabilities, dry-run, summary --------------------


def test_marks_loud_sessions_and_clears_calm_ones(capsys):
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("loud", "blocked"), session("calm")],
    )
    result = run_main(client, capsys)
    assert ("mark", "loud", "blocked", "attention is blocked", "watch") in client.calls
    assert ("mark", "calm", "ok", "", "watch") in client.calls  # ok ⇒ clear
    assert result["outcome"] == "ok"
    assert result["summary"] == "surveyed 2, marked 1 (1 blocked)"
    assert result["actions"] == [
        {"action": "mark", "session": "loud", "level": "blocked",
         "note": "attention is blocked"}
    ]


def test_without_mark_capability_only_observes(capsys):
    client = StubClient(sessions=[session("loud", "attention")])
    result = run_main(client, capsys)
    assert client.calls == []  # no mutation attempted
    assert result["actions"] == [
        {"action": "observe", "session": "loud", "level": "attention",
         "note": "attention is attention"}
    ]


def test_dry_run_mutates_nothing_and_says_so(capsys):
    client = StubClient(capabilities=["mark"],
                        sessions=[session("loud", "blocked"), session("calm")])
    result = run_main(client, capsys, dry_run=True)
    assert client.calls == []
    assert result["actions"] == [
        {"would": "mark", "session": "loud", "level": "blocked",
         "note": "attention is blocked"}
    ]
    assert result["summary"] == "surveyed 2, would mark 1 (1 blocked) (dry run, no marks applied)"


def test_nudge_follows_a_mark_and_a_dead_pane_no_ops(capsys):
    client = StubClient(capabilities=["mark", "nudge"],
                        sessions=[session("loud", "blocked")])
    result = run_main(client, capsys)
    assert ("nudge", "loud", "[overlooker watch] attention is blocked", "watch") in client.calls
    assert {"action": "nudge", "session": "loud",
            "text": "[overlooker watch] attention is blocked"} in result["actions"]

    dead = StubClient(capabilities=["mark", "nudge"],
                      sessions=[session("loud", "blocked")], nudge_fails=True)
    result = run_main(dead, capsys)
    assert not any(a.get("action") == "nudge" for a in result["actions"])


def test_empty_scope_is_a_noop(capsys):
    result = run_main(StubClient(capabilities=["mark"]), capsys)
    assert result == {"outcome": "noop", "summary": "surveyed 0 sessions in scope",
                      "actions": []}
