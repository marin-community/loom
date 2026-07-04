"""status — when a session goes quiet, judge it and replace the calm idle mark.

When the agent goes quiet, loom stamps a soothing, *quiet* ``idle`` mark — the
calm "resting, no one needed" state (so an idle agent never reads as needing the
user). On the agent's finished-turn hook (``session.idle``) this watch
re-assesses just that session: it asks the judge model (the daemon's one-shot
agent) for advice on a set of attention tags to apply given the recent screen,
then reconciles its own marks to that set — setting the recommended tags and
clearing any it set earlier that no longer apply.

When the judge finds the session genuinely needs a human, that session is
actively waiting, not resting: the watch *replaces* the calm ``idle`` mark with
the real loud status it names. A "nothing needed" verdict leaves the soothing
``idle`` mark in place.

User attention is expensive, so the default prompt tells the model to flag a
session ONLY when it genuinely needs the human, and to name the *kind* of
attention (the tag key — `review`, `question`, `stuck`, …) rather than a generic
"needs attention". The watch never mirrors the agent's own ``attention``
self-report; that is the agent's signal, on its own key. With no judge model
available (no agent, or an empty/garbled reply), the round is a no-op — it leaves
every mark untouched (including ``idle``) rather than guess. Honours dry_run:
would-be writes are logged as actions and nothing mutates.
"""

from weaver_loom import IDLE_KEY, IDLE_VALUE, Round, parse_tag_recommendations

#: Wake on the agent's finished-turn hook — "assess when the agent goes quiet".
TRIGGERS = {"on": ["session.idle"]}

#: Lines of terminal scrollback handed to the judge as context.
SCREEN_LINES = 200

#: The default judge prompt, overridable via params.prompt. Asks for a JSON array
#: of {key, value, note} tags, or [] when the human is not needed.
DEFAULT_PROMPT = (
    "You are triaging a detached coding-agent session for a human operator who "
    "reviews many sessions asynchronously. A quiet agent is already shown as a "
    "calm 'idle' state that needs no one, so do NOT flag a session merely for "
    "being idle or having finished a turn. Their attention is expensive: flag a "
    "session ONLY when it genuinely needs the human now — naming that need "
    "replaces the calm idle mark with a real status. Read the recent session "
    "screen below and decide which attention tags apply.\n\n"
    "Reply with ONLY a JSON array of objects "
    '{"key": "<short-type>", "value": "attention" | "blocked", "note": "<one '
    'line>"}. Use an empty array [] when the session does not need the human (it '
    "stays calmly idle). The key names the KIND of attention; pick the few that "
    "fit, e.g.:\n"
    '  - "review"   — work looks finished / a PR is ready to look at (value "attention")\n'
    '  - "question" — waiting on a decision only the operator can make (value "attention")\n'
    '  - "stuck"    — looping or erroring with no progress (value "blocked")\n'
    "Do not invent noise, and do not restate the agent's own self-reported "
    "status — add only what an outside observer would flag."
)


def judge_tags(rnd, session):
    """Ask the judge model for the set of tags to apply to ``session``.

    Returns the parsed list (possibly empty — the "nothing needed" verdict that
    clears the watch's marks), or ``None`` for "no judgement" (no agent, or an
    unparseable reply) so the caller leaves the session's marks untouched."""
    prompt = rnd.params.get("prompt") or DEFAULT_PROMPT
    screen = rnd.preview_or(session["id"], SCREEN_LINES)
    out = rnd.client.agent(f"{prompt}\n\nSession screen:\n{screen}\n", rnd.model, rnd.effort)
    if not out:
        return None
    return parse_tag_recommendations(out)


def watch_tags(rnd, session):
    """The keys of tags on ``session`` that THIS watch authored — the marks it
    owns and may reconcile (never the agent's own or another author's)."""
    return {
        tag.get("key")
        for tag in (session.get("branch") or {}).get("tags") or []
        if tag.get("set_by") == rnd.name and tag.get("key")
    }


def _has_idle(session):
    """Whether ``session`` carries the soothing ``idle`` mark the idle hook
    stamps — the calm state a real status should replace. Matches the canonical
    ``(idle, idle)`` mark exactly (key AND value), so a stray free-form tag that
    merely shares the ``idle`` key is never mistaken for it and cleared."""
    return any(
        tag.get("key") == IDLE_KEY and tag.get("value") == IDLE_VALUE
        for tag in (session.get("branch") or {}).get("tags") or []
    )


def main(rnd):
    can_mark = rnd.can("mark")
    assessed = 0
    tagged = 0

    # Reactive on session.idle: act on just the session that went quiet. A manual
    # run (no trigger session) falls back to the whole scoped fleet.
    for session in rnd.triggered_sessions():
        desired = judge_tags(rnd, session)
        if desired is None:
            continue  # no judgement available — leave this session's marks alone
        assessed += 1
        sid = session["id"]
        stale = watch_tags(rnd, session) - {t["key"] for t in desired}
        # When the judge names a real need, the session is actively waiting, not
        # resting — replace the soothing `idle` mark with that status. A calm
        # verdict (no desired tags) leaves `idle` in place. The mark is the
        # agent's own, set mechanically by the idle hook, so the watch is
        # explicitly allowed to clear it here (it is not one of its own marks).
        replace_idle = bool(desired) and _has_idle(session)

        # Without `mark`, the round is read-only: report what it would tag.
        if not can_mark:
            for t in desired:
                rnd.did("observe", session=sid, **t)
            continue

        tagged += len(desired)
        for t in desired:
            if rnd.dry_run:
                rnd.would("tag", session=sid, **t)
            else:
                rnd.client.set_tag(sid, t["key"], t["value"], t["note"], by=rnd.name)
                rnd.did("tag", session=sid, **t)
        # Reconcile: clear the watch's own marks the new judgement dropped, plus
        # the calm `idle` mark when a real status replaces it.
        for key in sorted(stale) + ([IDLE_KEY] if replace_idle else []):
            if rnd.dry_run:
                rnd.would("clear", session=sid, key=key)
            else:
                rnd.client.clear_tag(sid, key, by=rnd.name)
                rnd.did("clear", session=sid, key=key)

    if rnd.surveyed == 0:
        rnd.finish("surveyed 0 sessions in scope", outcome="noop")
        return

    dry = " (dry run, no writes applied)" if rnd.dry_run else ""
    verb = "would apply" if rnd.dry_run else "applied"
    rnd.finish(
        f"assessed {assessed} of {rnd.surveyed}, {verb} {tagged} tag(s){dry}",
        outcome="ok" if rnd.actions else "noop",
    )


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
