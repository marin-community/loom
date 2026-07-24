"""The builtin status program's decision logic, with no server required.

Loads `crates/loom/watches/status.py` straight from the repo and drives it
with stubbed clients: the judge's parse + no-judgement split, the reconcile
(set recommended tags, clear the watch's own dropped ones), the capability
branches, dry-run, and the summary all live here. The Rust integration suite
keeps only the wiring proof — that the script runs under the engine against a
live loom and its writes/audit rows land.

pr-label and archive-merged carry no logic beyond a one-line predicate over the
PR snapshot, so their coverage stays in the Rust end-to-end test
(`builtin_scripts_report_merged_and_unlabelled_prs`) — a pytest double of a
one-liner would be duplication, not coverage.
"""

import importlib.util
import json
from pathlib import Path

from weaver_loom import Round, WeaverError

WATCHES = Path(__file__).resolve().parents[3] / "crates" / "loom" / "watches"


def load_program(name):
    spec = importlib.util.spec_from_file_location(name, WATCHES / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


status = load_program("status")

TAGS = '[{"key": "review", "value": "attention", "note": "looks done"}]'


class StubClient:
    """The slice of Client the status program touches, scripted per test."""

    def __init__(self, capabilities=None, sessions=None, agent_reply=None, screen="the pane"):
        self.capabilities = capabilities or []
        self._sessions = sessions or []
        self.agent_reply = agent_reply
        self.screen = screen
        self.calls = []

    def can(self, cap):
        return cap == "observe" or cap in self.capabilities

    def sessions(self):
        return self._sessions

    def preview(self, key, lines=0):
        self.calls.append(("preview", key, lines))
        return self.screen

    def agent(self, prompt, model="", effort=""):
        self.calls.append(("agent", prompt, model, effort))
        return self.agent_reply

    def set_tags(self, key, tags, by=None, clear=None):
        self.calls.append(("set_tags", key, tags, by, clear or []))


def session(id, attention=None, watch=None, status="running", idle=False):
    """A session dict: an optional `attention` self-report (set_by agent), an
    optional soothing `idle` mark (set_by agent, the idle hook's stamp), and any
    number of watch-authored marks (set_by 'watch', this round's name)."""
    tags = []
    if attention:
        tags.append({"key": "attention", "value": attention, "set_by": "agent"})
    if idle:
        tags.append({"key": "idle", "value": "idle", "set_by": "agent"})
    for key, value in (watch or {}).items():
        tags.append({"key": key, "value": value, "set_by": "watch"})
    return {"id": id, "status": status, "branch": {"tags": tags, "repo_root": "/r"}}


def make_round(client, **config):
    return Round(config={"name": "watch", **config}, client=client)


def run_main(client, capsys, **config):
    """Drive main(rnd) with a stubbed round; return the parsed result line."""
    rnd = make_round(client, capabilities=client.capabilities, **config)
    status.main(rnd)
    return json.loads(capsys.readouterr().out.strip().splitlines()[-1])


def batches(client):
    return [c for c in client.calls if c[0] == "set_tags"]


# -- judge: parse the agent's tag set, no-judgement vs calm verdict ------------


def test_judge_parses_the_agents_tag_set():
    client = StubClient(agent_reply=TAGS)
    rnd = make_round(client)
    assert status.judge_tags(rnd, session("s")) == [
        {"key": "review", "value": "attention", "note": "looks done"}
    ]


def test_judge_is_none_without_an_agent():
    # No agent (empty reply) → None ("no judgement"), distinct from a calm []
    # verdict: the caller leaves the session's marks untouched.
    for reply in (None, ""):
        rnd = make_round(StubClient(agent_reply=reply))
        assert status.judge_tags(rnd, session("s")) is None


def test_judge_empty_array_is_the_calm_verdict():
    rnd = make_round(StubClient(agent_reply="nothing needed: []"))
    assert status.judge_tags(rnd, session("s")) == []


def test_judge_uses_the_default_prompt_with_screen_and_model():
    client = StubClient(agent_reply=TAGS)
    rnd = make_round(client, model="haiku", effort="low")
    status.judge_tags(rnd, session("s"))
    assert ("preview", "s", status.SCREEN_LINES) in client.calls
    kind, prompt, model, effort = client.calls[-1]
    assert (kind, model, effort) == ("agent", "haiku", "low")
    assert prompt.startswith(status.DEFAULT_PROMPT) and "the pane" in prompt


def test_judge_honours_a_configured_prompt():
    client = StubClient(agent_reply=TAGS)
    rnd = make_round(client, params={"prompt": "MY PROMPT"})
    status.judge_tags(rnd, session("s"))
    _, prompt, *_ = client.calls[-1]
    assert prompt.startswith("MY PROMPT")


def test_judge_survives_a_dead_pane():
    class NoPane(StubClient):
        def preview(self, key, lines=0):
            raise WeaverError("409 no live terminal")

    client = NoPane(agent_reply=TAGS)
    rnd = make_round(client, params={"prompt": "judge"})
    assert status.judge_tags(rnd, session("s")) == [
        {"key": "review", "value": "attention", "note": "looks done"}
    ]
    # The agent was still consulted, with an empty screen.
    assert any(c[0] == "agent" for c in client.calls)


# -- main: apply + reconcile, capabilities, dry-run, summary -------------------


def test_applies_tags_and_reconciles_its_own_dropped_marks(capsys):
    # The watch previously marked `stuck`; the new judgement recommends `review`,
    # so `review` is set and the watch's own `stuck` is cleared.
    client = StubClient(
        capabilities=["mark"],
        agent_reply=TAGS,
        sessions=[session("s", watch={"stuck": "blocked"})],
    )
    result = run_main(client, capsys)
    assert batches(client) == [
        (
            "set_tags",
            "s",
            [{"key": "review", "value": "attention", "note": "looks done"}],
            "watch",
            [],
        )
    ]
    assert result["outcome"] == "ok"
    assert result["summary"] == "assessed 1 of 1, applied 1 tag(s)"


def test_empty_verdict_clears_the_watchs_own_marks(capsys):
    client = StubClient(
        capabilities=["mark"],
        agent_reply="[]",
        sessions=[session("s", watch={"review": "attention"})],
    )
    result = run_main(client, capsys)
    assert batches(client) == [("set_tags", "s", [], "watch", [])]
    assert result["summary"] == "assessed 1 of 1, applied 0 tag(s)"


def test_no_judgement_leaves_every_mark_untouched(capsys):
    # Agent absent → None: the round must not clear the watch's existing marks.
    client = StubClient(
        capabilities=["mark"],
        agent_reply=None,
        sessions=[session("s", watch={"review": "attention"})],
    )
    result = run_main(client, capsys)
    assert batches(client) == []
    assert result == {
        "outcome": "noop",
        "summary": "assessed 0 of 1, applied 0 tag(s)",
        "actions": [],
    }


def test_never_touches_the_agents_own_self_report(capsys):
    # The agent self-reports `attention`; the judge says nothing is needed. Only
    # watch-authored marks are reconciled, so the agent's tag is never cleared.
    client = StubClient(
        capabilities=["mark"],
        agent_reply="[]",
        sessions=[session("s", attention="attention")],
    )
    run_main(client, capsys)
    assert batches(client) == [("set_tags", "s", [], "watch", [])]


def test_without_mark_capability_only_observes(capsys):
    client = StubClient(agent_reply=TAGS, sessions=[session("s")])
    result = run_main(client, capsys)
    assert batches(client) == []
    assert result["actions"] == [
        {"action": "observe", "session": "s", "key": "review",
         "value": "attention", "note": "looks done"}
    ]


def test_dry_run_records_would_and_mutates_nothing(capsys):
    client = StubClient(
        capabilities=["mark"],
        agent_reply=TAGS,
        sessions=[session("s", watch={"stuck": "blocked"})],
    )
    result = run_main(client, capsys, dry_run=True)
    assert batches(client) == []
    assert {"would": "tag", "session": "s", "key": "review",
            "value": "attention", "note": "looks done"} in result["actions"]
    assert {"would": "clear", "session": "s", "key": "stuck"} in result["actions"]
    assert result["summary"] == "assessed 1 of 1, would apply 1 tag(s) (dry run, no writes applied)"


# -- idle: a real status replaces the soothing idle mark ----------------------


def test_real_status_replaces_the_soothing_idle_mark(capsys):
    # The session is resting (the idle hook stamped `idle`); the judge finds work
    # ready to review → the watch sets `review` AND clears `idle`, replacing the
    # calm mark with the real status. The `idle` mark is the agent's own, yet the
    # watch is allowed to clear it here.
    client = StubClient(
        capabilities=["mark"],
        agent_reply=TAGS,
        sessions=[session("s", idle=True)],
    )
    run_main(client, capsys)
    assert batches(client) == [
        (
            "set_tags",
            "s",
            [{"key": "review", "value": "attention", "note": "looks done"}],
            "watch",
            [{"key": "idle", "value": "idle"}],
        )
    ]


def test_calm_verdict_leaves_the_idle_mark(capsys):
    # Nothing needed → the agent stays calmly idle; the watch must NOT clear the
    # soothing `idle` mark.
    client = StubClient(
        capabilities=["mark"],
        agent_reply="[]",
        sessions=[session("s", idle=True)],
    )
    run_main(client, capsys)
    assert batches(client) == [("set_tags", "s", [], "watch", [])]


def test_no_idle_mark_means_nothing_to_replace(capsys):
    # A real verdict but no `idle` mark on the session → the round sets the tag
    # without a spurious idle clear.
    client = StubClient(
        capabilities=["mark"],
        agent_reply=TAGS,
        sessions=[session("s")],
    )
    run_main(client, capsys)
    assert batches(client)[0][4] == []


def test_stray_idle_keyed_tag_is_not_cleared(capsys):
    # A free-form tag that merely shares the `idle` key but isn't the canonical
    # (idle, idle) mark must NOT be cleared when a real status is applied.
    sess = {
        "id": "s",
        "status": "running",
        "branch": {
            "repo_root": "/r",
            "tags": [{"key": "idle", "value": "paused", "set_by": "manual"}],
        },
    }
    client = StubClient(capabilities=["mark"], agent_reply=TAGS, sessions=[sess])
    run_main(client, capsys)
    assert batches(client)[0][4] == []


def test_dry_run_would_clear_the_idle_mark(capsys):
    client = StubClient(
        capabilities=["mark"],
        agent_reply=TAGS,
        sessions=[session("s", idle=True)],
    )
    result = run_main(client, capsys, dry_run=True)
    assert batches(client) == []
    assert {"would": "clear", "session": "s", "key": "idle"} in result["actions"]


def test_empty_survey_is_a_noop(capsys):
    result = run_main(StubClient(capabilities=["mark"]), capsys)
    assert result == {"outcome": "noop", "summary": "surveyed 0 sessions in scope",
                      "actions": []}
