"""pr-label — add the loom label when a session's PR first appears.

Label addition is idempotent, so the open edge needs one GitHub call rather
than a read followed by a conditional write. ``mark`` grants that write;
without it (or during a dry run), the program reports what it would do.

Subscribes to `pr.opened` — it wakes when a session's PR first appears, on that
one branch, instead of re-reading every session on a timer.
"""

from weaver_loom import Round, WeaverError

DEFAULT_LABEL = "weaver"

#: Wake when a PR opens — the engine reads this in register mode.
TRIGGERS = {"on": ["pr.opened"]}


def main(rnd):
    label = rnd.params.get("label") or DEFAULT_LABEL
    can_mark = rnd.can("mark")
    labelled = 0
    pending = 0
    for session in rnd.triggered_sessions():
        branch = session.get("branch") or {}
        github = branch.get("github") or {}
        if github.get("pr_state") != "OPEN":
            continue
        pr = github.get("pr_number")
        fields = {"session": session["id"], "pr": pr, "label": label}
        if rnd.dry_run or not can_mark:
            note = "dry run" if rnd.dry_run else "mark capability not granted"
            rnd.would("label", **fields, note=note)
            pending += 1
            continue
        try:
            rnd.client.add_pr_label(session["id"], label)
        except WeaverError as e:
            rnd.would(
                "label",
                **fields,
                note=f"PR #{pr}: label could not be added — {e}",
            )
            pending += 1
            continue
        rnd.did("label", **fields)
        labelled += 1
    suffix = " (dry run, no writes applied)" if rnd.dry_run else ""
    summary = f"surveyed {rnd.surveyed}, labelled {labelled} open PR(s)"
    if pending:
        summary += f", {pending} pending"
    rnd.finish(summary + suffix)


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
