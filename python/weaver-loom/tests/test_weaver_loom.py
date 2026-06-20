"""The weaver_loom contract, with no server required.

These run in CI (the `python-binding` job). They cover the pure logic every
overlooker program leans on — judgement parsing, the survey's scope filter,
capability gating, mark routing, and the round-result stdout contract — by
stubbing the HTTP layer; the live end-to-end coverage is the Rust integration
suite (`crates/loom/tests/integration/overlookers.rs`), which runs the real
builtin scripts against a real loom.
"""

import json

import pytest
from weaver_loom import (
    CAPABILITIES,
    CapabilityDenied,
    Client,
    Round,
    parse_judgement,
    parse_tag_recommendations,
)


class StubClient(Client):
    """A Client whose requests are recorded instead of sent."""

    def __init__(self, capabilities=None, replies=None):
        super().__init__(base="http://stub", capabilities=capabilities)
        self.requests = []
        self.replies = replies or {}

    def _request(self, method, path, body=None):
        self.requests.append((method, path, body))
        return self.replies.get(path)


def session(id, status="running", tags=None, repo_root="/repo"):
    return {
        "id": id,
        "status": status,
        "branch": {"tags": tags or [], "repo_root": repo_root},
    }


def attention(value):
    return [{"key": "attention", "value": value}]


# -- parse_judgement ---------------------------------------------------------


def test_parse_judgement_finds_level_and_note():
    assert parse_judgement("blocked: stuck retrying the same test") == (
        "blocked",
        "stuck retrying the same test",
    )
    # The earliest separator wins, mirroring the engine's old parser.
    assert parse_judgement("attention - waiting: on review") == (
        "attention",
        "waiting: on review",
    )
    # 'ok' is recognised even without a separator; the whole line is the note.
    assert parse_judgement("ok") == ("ok", "ok")
    # The level word is matched on word boundaries, not substrings.
    assert parse_judgement("blockedness everywhere") is None


def test_parse_judgement_scans_lines_and_degrades_to_none():
    assert parse_judgement("preamble\nBlocked - cannot push") == (
        "blocked",
        "cannot push",
    )
    assert parse_judgement("looks fine to me, carry on") is None
    assert parse_judgement("") is None
    assert parse_judgement(None) is None


# -- parse_tag_recommendations -----------------------------------------------


def test_parse_tag_recommendations_validates_and_slugs():
    out = parse_tag_recommendations(
        '[{"key": "Needs Review!", "value": "Attention", "note": "PR is up"},'
        ' {"key": "stuck", "value": "blocked", "note": ""}]'
    )
    assert out == [
        {"key": "needs-review", "value": "attention", "note": "PR is up"},
        {"key": "stuck", "value": "blocked", "note": ""},
    ]


def test_parse_tag_recommendations_drops_malformed_and_dedupes():
    out = parse_tag_recommendations(
        '[{"key": "review", "value": "ok"},'        # value off the loud ladder
        ' {"key": "", "value": "attention"},'        # no key
        ' "not-an-object",'                           # not a dict
        ' {"key": "review", "value": "attention"},'   # first review wins
        ' {"key": "review", "value": "blocked"}]'     # dupe key dropped
    )
    assert out == [{"key": "review", "value": "attention", "note": ""}]


def test_parse_tag_recommendations_extracts_an_array_from_prose():
    out = parse_tag_recommendations(
        'Sure — here you go:\n```json\n[{"key": "review", "value": "attention"}]\n```\n'
    )
    assert out == [{"key": "review", "value": "attention", "note": ""}]


def test_parse_tag_recommendations_none_vs_empty():
    # An explicit empty array is the calm verdict ([]); no array / invalid JSON
    # is "no judgement" (None) — the caller treats the two differently.
    assert parse_tag_recommendations("nothing to flag: []") == []
    assert parse_tag_recommendations("looks fine, carry on") is None
    assert parse_tag_recommendations("[not json]") is None
    assert parse_tag_recommendations("") is None
    assert parse_tag_recommendations(None) is None


# -- the survey's scope filter -------------------------------------------------


def make_round(scope=None, sessions=None, capabilities=None, **config):
    client = StubClient(
        capabilities=capabilities or [], replies={"/sessions": sessions or []}
    )
    return Round(
        config={"name": "t", "scope": scope or {}, **config},
        client=client,
    )


def test_sessions_skips_terminal_and_counts_surveyed():
    rnd = make_round(
        sessions=[
            session("live"),
            session("done", status="done"),
            session("archived", status="archived"),
        ]
    )
    assert [s["id"] for s in rnd.sessions()] == ["live"]
    assert rnd.surveyed == 1


def test_scope_attention_filter_matches_exact_and_negated():
    fleet = [
        session("calm"),
        session("loud", tags=attention("attention")),
        session("stuck", tags=attention("blocked")),
    ]
    not_ok = make_round(scope={"attention": "!ok"}, sessions=fleet)
    assert [s["id"] for s in not_ok.sessions()] == ["loud", "stuck"]
    only_blocked = make_round(scope={"attention": "blocked"}, sessions=fleet)
    assert [s["id"] for s in only_blocked.sessions()] == ["stuck"]
    # An absent tag is the calm `ok` state.
    only_ok = make_round(scope={"attention": "ok"}, sessions=fleet)
    assert [s["id"] for s in only_ok.sessions()] == ["calm"]


def test_scope_repo_pin_excludes_other_repos():
    fleet = [session("here"), session("there", repo_root="/elsewhere")]
    rnd = make_round(scope={"repo": "/repo"}, sessions=fleet)
    assert [s["id"] for s in rnd.sessions()] == ["here"]


# -- triggered_sessions: scope to the session the event named ------------------


def branch_session(id, branch_id, **kw):
    s = session(id, **kw)
    s["branch"]["id"] = branch_id
    return s


def trigger_round(trigger, sessions, scope=None):
    client = StubClient(capabilities=[], replies={"/sessions": sessions})
    return Round(
        config={"name": "t", "scope": scope or {}, "trigger": trigger}, client=client
    )


def test_triggered_sessions_scopes_to_the_named_branch():
    fleet = [branch_session("a", "b1"), branch_session("b", "b2")]
    rnd = trigger_round({"event": "pr.merged", "branch": "b1"}, fleet)
    assert [s["id"] for s in rnd.triggered_sessions()] == ["a"]
    assert rnd.surveyed == 1


def test_triggered_sessions_scopes_to_the_named_session():
    fleet = [branch_session("a", "b1"), branch_session("b", "b1")]
    rnd = trigger_round({"event": "session.idle", "session": "b"}, fleet)
    assert [s["id"] for s in rnd.triggered_sessions()] == ["b"]


def test_triggered_sessions_falls_back_to_full_survey_without_a_target():
    # A cron/manual tick names no session, so the round surveys the whole fleet.
    fleet = [session("a"), session("b"), session("done", status="done")]
    rnd = trigger_round({"event": "cron"}, fleet)
    assert [s["id"] for s in rnd.triggered_sessions()] == ["a", "b"]


# -- Round.main: register declares, run executes -------------------------------


def test_main_register_mode_prints_manifest_without_running(monkeypatch, capsys):
    monkeypatch.setenv("WEAVER_OVERLOOKER_MODE", "register")
    ran = []
    Round.main(lambda rnd: ran.append(rnd), {"on": ["pr.merged"]})
    assert ran == [], "register mode declares triggers, it never runs a round"
    assert json.loads(capsys.readouterr().out) == {"on": ["pr.merged"]}


def test_main_run_mode_invokes_the_round(monkeypatch):
    monkeypatch.delenv("WEAVER_OVERLOOKER_MODE", raising=False)
    monkeypatch.setenv("WEAVER_OVERLOOKER", "{}")
    ran = []
    Round.main(lambda rnd: ran.append(rnd))
    assert len(ran) == 1 and isinstance(ran[0], Round)


def test_register_mode_neuters_the_round(monkeypatch):
    # A legacy script that constructs a Round directly in register mode must not
    # be able to act: the survey is empty, the round is dry, and the engine-built
    # client is granted no write capabilities even when the config names some.
    monkeypatch.setenv("WEAVER_OVERLOOKER_MODE", "register")
    monkeypatch.setenv(
        "WEAVER_OVERLOOKER", json.dumps({"name": "t", "capabilities": ["mark"]})
    )
    rnd = Round()
    assert rnd.sessions() == []
    assert rnd.dry_run is True
    assert not rnd.client.can("mark"), "register mode drops the granted capabilities"
    with pytest.raises(CapabilityDenied):
        rnd.client.mark("live", "blocked")


def test_register_mode_triggered_sessions_is_empty(monkeypatch):
    # triggered_sessions() honours the same register-mode guard as sessions():
    # even when the trigger names a concrete session, a legacy script that calls
    # it while merely being asked what wakes it must not touch the fleet. The
    # round is given a client that explodes on any survey to prove it isn't hit.
    monkeypatch.setenv("WEAVER_OVERLOOKER_MODE", "register")

    class Exploding:
        def can(self, _cap):
            return False

        def sessions(self):
            raise AssertionError("register mode must not survey the fleet")

    rnd = Round(
        config={"trigger": {"event": "pr.opened", "session": "s1"}},
        client=Exploding(),
    )
    assert rnd.triggered_sessions() == []
    assert rnd.surveyed == 0


# -- capability gating ---------------------------------------------------------


def test_observe_is_implicit_and_writes_are_gated():
    c = StubClient()
    assert c.can("observe")
    c.sessions()  # a read never gates
    for call in (
        lambda: c.set_tag("s", "triage", "blocked"),
        lambda: c.clear_tag("s", "triage"),
        lambda: c.mark("s", "blocked"),
        lambda: c.nudge("s", "hello"),
        lambda: c.interrupt("s"),
    ):
        with pytest.raises(CapabilityDenied):
            call()
    # The gate fires before any request leaves the process.
    assert c.requests == [("GET", "/sessions", None)]


def test_capabilities_constant_matches_the_ladder():
    assert CAPABILITIES == [
        "observe",
        "mark",
        "escalate",
        "nudge",
        "interrupt",
        "launch",
    ]


# -- mark routing & attribution -------------------------------------------------


def test_mark_sets_a_loud_level_with_attribution():
    c = StubClient(capabilities=["mark"])
    c.mark("s1", "blocked", "stuck", by="watch")
    assert c.requests == [
        (
            "PUT",
            "/sessions/s1/tags/triage",
            {"value": "blocked", "note": "stuck", "by": "watch"},
        )
    ]


def test_mark_ok_clears_via_delete_with_by_query():
    c = StubClient(capabilities=["mark"])
    c.mark("s1", "ok", by="watch")
    c.mark("s1", "")
    assert c.requests == [
        ("DELETE", "/sessions/s1/tags/triage?by=watch", None),
        ("DELETE", "/sessions/s1/tags/triage", None),
    ]


def test_nudge_carries_by_for_the_audit_event():
    c = StubClient(capabilities=["nudge"])
    c.nudge("s1", "hello", by="watch")
    c.nudge("s1", "staged", submit=False)
    assert c.requests == [
        ("POST", "/sessions/s1/send", {"text": "hello", "submit": True, "by": "watch"}),
        ("POST", "/sessions/s1/send", {"text": "staged", "submit": False}),
    ]


def test_agent_returns_output_or_none():
    c = StubClient(replies={"/agent/oneshot": {"output": "blocked: judged"}})
    assert c.agent("look", model="haiku", effort="low") == "blocked: judged"
    assert c.requests[-1] == (
        "POST",
        "/agent/oneshot",
        {"prompt": "look", "model": "haiku", "effort": "low"},
    )
    # A degraded daemon reply ({output: null}) reads as None, not an error.
    absent = StubClient(replies={"/agent/oneshot": {"output": None}})
    assert absent.agent("look") is None


# -- the round-result contract ---------------------------------------------------


def test_round_reads_config_and_finish_prints_the_contract(capsys):
    rnd = make_round(
        params={"prompt": "stuck?"},
        model="haiku",
        effort="low",
        dry_run=True,
        capabilities=["mark"],
    )
    assert rnd.params == {"prompt": "stuck?"}
    assert (rnd.model, rnd.effort, rnd.dry_run) == ("haiku", "low", True)
    assert rnd.can("mark") and not rnd.can("nudge")

    rnd.would("mark", session="s1", level="blocked")
    rnd.did("observe", session="s2")
    rnd.finish("surveyed 2")
    result = json.loads(capsys.readouterr().out.strip().splitlines()[-1])
    assert result == {
        "outcome": "ok",
        "summary": "surveyed 2",
        "actions": [
            {"would": "mark", "session": "s1", "level": "blocked"},
            {"action": "observe", "session": "s2"},
        ],
    }


def test_finish_defaults_to_noop_without_actions(capsys):
    make_round().finish("nothing in scope")
    result = json.loads(capsys.readouterr().out.strip())
    assert result["outcome"] == "noop"
    assert result["actions"] == []


def test_state_is_read_from_config_and_omitted_from_finish_by_default(capsys):
    # The lookaside state arrives as a plain dict the program reads…
    rnd = make_round(state={"n": 3})
    assert rnd.state == {"n": 3}
    # …and a program that doesn't write it back / self-schedule leaves both keys
    # out of the result, so existing programs are unchanged.
    rnd.finish("done")
    result = json.loads(capsys.readouterr().out.strip())
    assert "state" not in result and "wake_in" not in result


def test_set_state_and_wake_in_ride_along_in_finish(capsys):
    rnd = make_round(state={"n": 3})
    rnd.set_state({"n": 4})
    rnd.wake_in(60)
    rnd.finish("rescheduled")
    result = json.loads(capsys.readouterr().out.strip())
    assert result["state"] == {"n": 4}
    assert result["wake_in"] == 60


def test_wake_in_zero_is_emitted_to_clear_a_pending_wake(capsys):
    # wake_in(0) is distinct from never calling it: it explicitly clears.
    rnd = make_round()
    rnd.wake_in(0)
    rnd.finish("nothing left to watch")
    result = json.loads(capsys.readouterr().out.strip())
    assert result["wake_in"] == 0


def test_state_defaults_to_empty_dict_without_config(capsys):
    assert make_round().state == {}


def test_set_state_rejects_non_dict():
    # The engine only persists an object; a non-dict would be silently dropped,
    # so the boundary rejects it loudly instead.
    rnd = make_round()
    for bad in ([1, 2], "nope", None, 7):
        with pytest.raises(TypeError):
            rnd.set_state(bad)
