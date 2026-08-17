"""pr-label — add the loom label when a session's PR first appears.

Subscribes to `pr.opened` — it wakes when a session's PR first appears, on that
one branch, instead of polling every session's labels on a timer.
"""

from weaver_loom import Round, WeaverError, gh_json

DEFAULT_LABEL = "weaver"

#: Wake when a PR opens — the engine reads this in register mode.
TRIGGERS = {"on": ["pr.opened"]}


def main(rnd):
    label = rnd.params.get("label") or DEFAULT_LABEL
    for session in rnd.triggered_sessions():
        branch = session.get("branch") or {}
        github = branch.get("github") or {}
        if github.get("pr_state") != "OPEN":
            continue
        pr = github.get("pr_number")
        fields = {"session": session["id"], "pr": pr, "label": label}
        if rnd.dry_run or not rnd.can("mark"):
            rnd.would("label", **fields)
            continue
        try:
            gh_json(
                [
                    "api",
                    "-X",
                    "POST",
                    f"repos/{{owner}}/{{repo}}/issues/{pr}/labels",
                    "-f",
                    f"labels[]={label}",
                ],
                cwd=branch.get("repo_root"),
            )
        except WeaverError as e:
            rnd.would("label", **fields, note=str(e))
            continue
        rnd.did("label", **fields)
    rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} label action(s)")


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
