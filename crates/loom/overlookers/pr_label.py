"""pr-label — flag sessions whose open PR lacks the loom label.

Read-only: records a would-label action per open PR missing the label
(params.label, default 'weaver'). Labels are read via the gh CLI.

Subscribes to `pr.opened` — it wakes when a session's PR first appears, on that
one branch, instead of re-reading every session's labels on a timer.
"""

import json
import subprocess

from weaver_loom import Round

DEFAULT_LABEL = "weaver"

#: Wake when a PR opens — the engine reads this in register mode.
TRIGGERS = {"on": ["pr.opened"]}


def pr_labels(repo_root, pr_number):
    """The PR's current label names via gh, or None when unreadable."""
    try:
        out = subprocess.run(
            ["gh", "pr", "view", str(pr_number), "--json", "labels"],
            cwd=repo_root,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if out.returncode != 0:
            return None
        reply = json.loads(out.stdout)
    except (OSError, subprocess.SubprocessError, ValueError):
        return None
    return [label.get("name", "") for label in reply.get("labels") or []]


def main(rnd):
    label = rnd.params.get("label") or DEFAULT_LABEL
    for session in rnd.triggered_sessions():
        branch = session.get("branch") or {}
        github = branch.get("github") or {}
        if github.get("pr_state") != "OPEN":
            continue
        pr = github.get("pr_number")
        labels = pr_labels(branch.get("repo_root"), pr)
        if labels is not None and label in labels:
            continue
        note = (
            f"PR #{pr} lacks label '{label}'"
            if labels is not None
            else f"PR #{pr}: labels unreadable via gh — would ensure '{label}'"
        )
        rnd.would("label", session=session["id"], pr=pr, label=label, note=note)
    rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} open PR(s) missing '{label}'")


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
