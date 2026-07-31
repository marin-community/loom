"""archive-merged — flag sessions whose pull request has merged.

Read-only: records a would-archive action per merged PR (the
github.archive_on_merge setting still performs the actual archive).

Subscribes to `pr.merged` — it wakes only when a PR actually merges, on the one
branch that changed, instead of polling the whole fleet on a timer.
"""

from weaver_loom import Round

#: Wake only on a PR merging — the engine reads this in register mode.
TRIGGERS = {"on": ["pr.merged"]}
AUTO_ARCHIVE_KEY = "auto-archive"
AUTO_ARCHIVE_DISABLED = "disabled"


def auto_archive_disabled(session):
    return any(
        tag.get("key") == AUTO_ARCHIVE_KEY
        and tag.get("value") == AUTO_ARCHIVE_DISABLED
        for tag in (session.get("branch") or {}).get("tags") or []
    )


def main(rnd):
    for session in rnd.triggered_sessions():
        if auto_archive_disabled(session):
            continue
        github = (session.get("branch") or {}).get("github") or {}
        if github.get("pr_state") != "MERGED":
            continue
        pr = github.get("pr_number")
        rnd.would(
            "archive",
            session=session["id"],
            pr=pr,
            note=f"PR #{pr} is merged — the session can be archived",
        )
    rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} with a merged PR")


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
