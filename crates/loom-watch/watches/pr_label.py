"""pr-label — flag sessions whose open PR lacks the loom label.

Read-only: records a would-label action per open PR missing the label
(params.label, default 'weaver'). Labels are read via the gh CLI.

Subscribes to `pr.opened` — it wakes when a session's PR first appears, on that
one branch, instead of re-reading every session's labels on a timer.
"""

from weaver_loom import Round, WeaverError, gh_json

DEFAULT_LABEL = "weaver"

#: Wake when a PR opens — the engine reads this in register mode.
TRIGGERS = {"on": ["pr.opened"]}


def pr_labels(repo_root, pr_number):
    """The PR's current label names via gh.

    Raises :class:`WeaverError` (from :func:`weaver_loom.gh_json`) when gh
    itself couldn't answer — not installed, not authenticated, timed out, a
    non-zero exit. The caller decides whether that's worth surviving; here it
    is, but the *reason* rides along in the note instead of being discarded.
    """
    reply = gh_json(["pr", "view", str(pr_number), "--json", "labels"], cwd=repo_root)
    return [label.get("name", "") for label in reply.get("labels") or []]


def main(rnd):
    label = rnd.params.get("label") or DEFAULT_LABEL
    for session in rnd.triggered_sessions():
        branch = session.get("branch") or {}
        github = branch.get("github") or {}
        if github.get("pr_state") != "OPEN":
            continue
        pr = github.get("pr_number")
        try:
            labels = pr_labels(branch.get("repo_root"), pr)
        except WeaverError as e:
            rnd.would(
                "label",
                session=session["id"],
                pr=pr,
                label=label,
                note=f"PR #{pr}: gh unreadable — {e}",
            )
            continue
        if label in labels:
            continue
        rnd.would(
            "label",
            session=session["id"],
            pr=pr,
            label=label,
            note=f"PR #{pr} lacks label '{label}'",
        )
    rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} open PR(s) missing '{label}'")


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
