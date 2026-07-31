"""review-wait — park sessions waiting on an external human PR reviewer.

When a session's pull request is open, not a draft, and needs a review that
hasn't landed yet (GitHub ``review_decision == REVIEW_REQUIRED``), the ball is
in the reviewer's court: there is nothing for the user to do. This watch marks
such a session with a quiet ``awaiting: review`` tag, which both *labels* it
(so a scanning user reads "awaiting review, no action mine") and *parks* it
below the calm default in the fleet sort — its value sits on weaver's parked
ladder (:data:`weaver_loom.PARKED_VALUES`), the quiet mirror of the loud
attention ladder. The moment review lands (approved or changes requested), the
PR merges, or the draft flag flips, the session may need someone again, so the
mark is cleared and the row returns to its normal slot.

The mark is the watch's own axis key (``awaiting``), distinct from the status
watch's loud ``review`` mark ("ready for *you* to look at"), so the two never
collide: a parked external-review wait and a "ready to review" nudge are
different states. Reconciles on each PR transition it subscribes to, so the
mark always reflects the current review state. Read-only without ``mark``
(reports what it would park / un-park); honours ``dry_run`` likewise.

Subscribes to the PR transitions that can start or end a review wait — the
review decision changing (the core edge), a PR opening already needing review,
and a PR merging (clear the mark) — each handing it just the one branch that
changed instead of polling the fleet on a timer.
"""

from weaver_loom import PARKED_VALUES, Round

#: The watch's own axis key. The value is :data:`REVIEW_VALUE`; the pair renders
#: as a quiet ``awaiting: review`` pill and parks the row.
AWAITING_KEY = "awaiting"

#: The parked value the mark carries — on weaver's parked ladder, so the row
#: sinks below the calm default. Sourced from the shared registry so it can't
#: drift from the core / frontend definition.
REVIEW_VALUE = "review"
assert REVIEW_VALUE in PARKED_VALUES  # guards the mirror against drift

#: Wake on the PR transitions that begin or end a review wait. The engine reads
#: this in register mode.
TRIGGERS = {"on": ["pr.review_changed", "pr.opened", "pr.merged"]}


def waiting_for_review(github):
    """Whether this PR is parked on an external reviewer: open, not a draft, and
    review required but not yet given. A merged/closed PR, a draft, or a decided
    review (approved / changes requested) is *not* waiting — the user may have
    something to do."""
    return (
        github.get("pr_state") == "OPEN"
        and not github.get("is_draft")
        and github.get("review_decision") == "REVIEW_REQUIRED"
    )


def has_own_mark(session, name):
    """Whether THIS watch's parked review mark is already on ``session`` — its
    own ``(awaiting, review)`` tag, authored by this watch. Reconciles only its
    own marks (never a human's manual tag of the same key), mirroring the status
    watch's ownership rule."""
    return any(
        tag.get("key") == AWAITING_KEY
        and tag.get("value") == REVIEW_VALUE
        and tag.get("set_by") == name
        for tag in (session.get("branch") or {}).get("tags") or []
    )


def main(rnd):
    can_mark = rnd.can("mark")
    parked = 0
    cleared = 0

    # Reactive on a PR transition: act on just the branch that changed. A manual
    # run (no trigger branch) falls back to the whole scoped fleet, reconciling
    # every session's review-wait mark in one pass.
    for session in rnd.triggered_sessions():
        sid = session["id"]
        github = (session.get("branch") or {}).get("github") or {}
        marked = has_own_mark(session, rnd.name)

        if waiting_for_review(github):
            if marked:
                continue  # already parked — idempotent, no redundant write
            pr = github.get("pr_number")
            note = f"PR #{pr} review required — waiting on an external reviewer"
            parked += 1
            if can_mark and not rnd.dry_run:
                rnd.client.set_tags(
                    sid,
                    [{"key": AWAITING_KEY, "value": REVIEW_VALUE, "note": note}],
                    by=rnd.name,
                )
                rnd.did("park", session=sid, key=AWAITING_KEY, value=REVIEW_VALUE, note=note)
            else:
                rnd.would("park", session=sid, key=AWAITING_KEY, value=REVIEW_VALUE, note=note)
        elif marked:
            # No longer waiting (review landed, the PR merged, or it un-drafted):
            # un-park so the row returns to its normal slot.
            cleared += 1
            if can_mark and not rnd.dry_run:
                rnd.client.set_tags(sid, [], by=rnd.name)
                rnd.did("unpark", session=sid, key=AWAITING_KEY)
            else:
                rnd.would("unpark", session=sid, key=AWAITING_KEY)

    if rnd.dry_run or not can_mark:
        summary = f"surveyed {rnd.surveyed}, would park {parked}, would clear {cleared}"
    else:
        summary = f"surveyed {rnd.surveyed}, parked {parked}, cleared {cleared}"
    rnd.finish(summary)


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
