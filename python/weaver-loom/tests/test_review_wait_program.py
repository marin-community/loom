"""The builtin review-wait program's decision logic, with no server required.

Loads `crates/loom/watches/review_wait.py` straight from the repo and drives
it with a stubbed client: the waiting-for-review predicate (open + not draft +
REVIEW_REQUIRED), the park / un-park reconcile, ownership (it touches only its
own marks), the capability + dry-run branches, and the summary all live here.
The Rust integration suite keeps only the wiring proof — that the script runs
under the engine against a live loom and its writes land attributed.
"""

import importlib.util
import json
from pathlib import Path

from weaver_loom import PARKED_VALUES, Round

WATCHES = Path(__file__).resolve().parents[3] / "crates" / "loom" / "watches"


def load_program(name):
    spec = importlib.util.spec_from_file_location(name, WATCHES / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


review_wait = load_program("review_wait")

NAME = "review-wait"


class StubClient:
    """The slice of Client the review-wait program touches, scripted per test."""

    def __init__(self, capabilities=None, sessions=None):
        self.capabilities = capabilities or []
        self._sessions = sessions or []
        self.calls = []

    def can(self, cap):
        return cap == "observe" or cap in self.capabilities

    def sessions(self):
        return self._sessions

    def set_tag(self, key, tag_key, value, note="", by=None):
        self.calls.append(("set_tag", key, tag_key, value, note, by))

    def clear_tag(self, key, tag_key, by=None):
        self.calls.append(("clear_tag", key, tag_key, by))


def session(id, *, pr_state="OPEN", draft=False, review=None, pr=42,
            marked_by=None, has_pr=True, status="running"):
    """A session dict with an optional PR snapshot and an optional pre-existing
    `awaiting: review` mark (attributed to `marked_by` — NAME for the watch's own
    mark, anything else for a foreign/manual one)."""
    branch = {"tags": [], "repo_root": "/r"}
    if marked_by:
        branch["tags"].append({"key": "awaiting", "value": "review", "set_by": marked_by})
    if has_pr:
        branch["github"] = {
            "pr_state": pr_state, "is_draft": draft,
            "review_decision": review, "pr_number": pr,
        }
    return {"id": id, "status": status, "branch": branch}


def make_round(client, **config):
    return Round(config={"name": NAME, **config}, client=client)


def run_main(client, capsys, **config):
    rnd = make_round(client, capabilities=client.capabilities, **config)
    review_wait.main(rnd)
    return json.loads(capsys.readouterr().out.strip().splitlines()[-1])


def sets(client):
    return [c for c in client.calls if c[0] == "set_tag"]


def clears(client):
    return [c for c in client.calls if c[0] == "clear_tag"]


# -- the registry mirror -------------------------------------------------------


def test_parked_value_is_drawn_from_the_shared_registry():
    # The mark's value must stay on weaver's parked ladder, or the row wouldn't
    # sink — the module asserts this at import, this pins it explicitly.
    assert review_wait.REVIEW_VALUE in PARKED_VALUES


# -- waiting_for_review predicate ----------------------------------------------


def test_waiting_predicate_only_an_open_non_draft_review_required_pr():
    waiting = review_wait.waiting_for_review
    assert waiting({"pr_state": "OPEN", "is_draft": False, "review_decision": "REVIEW_REQUIRED"})
    # Decided reviews, drafts, merged/closed PRs, and no PR are all NOT waiting.
    assert not waiting({"pr_state": "OPEN", "is_draft": False, "review_decision": "APPROVED"})
    assert not waiting({"pr_state": "OPEN", "is_draft": False, "review_decision": "CHANGES_REQUESTED"})
    assert not waiting({"pr_state": "OPEN", "is_draft": True, "review_decision": "REVIEW_REQUIRED"})
    assert not waiting({"pr_state": "MERGED", "is_draft": False, "review_decision": "REVIEW_REQUIRED"})
    assert not waiting({"pr_state": "OPEN", "is_draft": False, "review_decision": None})
    assert not waiting({})


# -- main: park, idempotency, un-park ------------------------------------------


def test_parks_a_session_awaiting_external_review(capsys):
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("s", review="REVIEW_REQUIRED", pr=7)],
    )
    result = run_main(client, capsys)
    note = "PR #7 review required — waiting on an external reviewer"
    assert ("set_tag", "s", "awaiting", "review", note, NAME) in client.calls
    assert clears(client) == []
    assert {"action": "park", "session": "s", "key": "awaiting",
            "value": "review", "note": note} in result["actions"]
    assert result["outcome"] == "ok"
    assert result["summary"] == "surveyed 1, parked 1, cleared 0"


def test_already_parked_is_idempotent(capsys):
    # Its own mark is already present and the session is still waiting → no write.
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("s", review="REVIEW_REQUIRED", marked_by=NAME)],
    )
    result = run_main(client, capsys)
    assert sets(client) == [] and clears(client) == []
    assert result == {"outcome": "noop",
                      "summary": "surveyed 1, parked 0, cleared 0", "actions": []}


def test_unparks_once_review_lands(capsys):
    # It parked earlier; the review came back approved → clear the mark.
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("s", review="APPROVED", marked_by=NAME)],
    )
    result = run_main(client, capsys)
    assert ("clear_tag", "s", "awaiting", NAME) in client.calls
    assert sets(client) == []
    assert {"action": "unpark", "session": "s", "key": "awaiting"} in result["actions"]
    assert result["summary"] == "surveyed 1, parked 0, cleared 1"


def test_unparks_on_merge(capsys):
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("s", pr_state="MERGED", marked_by=NAME)],
    )
    run_main(client, capsys)
    assert ("clear_tag", "s", "awaiting", NAME) in client.calls


def test_changes_requested_is_not_parked(capsys):
    # The agent has work to do — never park, and nothing to clear when unmarked.
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("s", review="CHANGES_REQUESTED")],
    )
    result = run_main(client, capsys)
    assert sets(client) == [] and clears(client) == []
    assert result["outcome"] == "noop"


def test_session_without_a_pr_is_a_noop(capsys):
    client = StubClient(capabilities=["mark"], sessions=[session("s", has_pr=False)])
    result = run_main(client, capsys)
    assert sets(client) == [] and clears(client) == []
    assert result["outcome"] == "noop"


# -- ownership: never touch a foreign mark -------------------------------------


def test_leaves_a_foreign_awaiting_mark_untouched(capsys):
    # A human manually set `awaiting: review`; the session is no longer waiting.
    # The watch reconciles only its OWN marks, so it must not clear this one.
    client = StubClient(
        capabilities=["mark"],
        sessions=[session("s", review="APPROVED", marked_by="manual")],
    )
    result = run_main(client, capsys)
    assert clears(client) == []
    assert result["outcome"] == "noop"


# -- capabilities + dry-run ----------------------------------------------------


def test_without_mark_capability_only_reports_would(capsys):
    client = StubClient(sessions=[session("s", review="REVIEW_REQUIRED")])
    result = run_main(client, capsys)
    assert sets(client) == [] and clears(client) == []
    assert result["actions"][0]["would"] == "park"
    assert result["summary"] == "surveyed 1, would park 1, would clear 0"


def test_dry_run_records_would_and_mutates_nothing(capsys):
    client = StubClient(
        capabilities=["mark"],
        sessions=[
            session("a", review="REVIEW_REQUIRED"),
            session("b", review="APPROVED", marked_by=NAME),
        ],
    )
    result = run_main(client, capsys, dry_run=True)
    assert sets(client) == [] and clears(client) == []
    woulds = {(a["would"], a["session"]) for a in result["actions"]}
    assert woulds == {("park", "a"), ("unpark", "b")}
    assert result["summary"] == "surveyed 2, would park 1, would clear 1"


def test_empty_survey_is_a_noop(capsys):
    result = run_main(StubClient(capabilities=["mark"]), capsys)
    assert result == {"outcome": "noop",
                      "summary": "surveyed 0, parked 0, cleared 0", "actions": []}
