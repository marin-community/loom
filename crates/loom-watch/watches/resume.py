"""resume — when a session stalls on a transient API error, gently re-prompt it
to continue, backing off exponentially so a sustained outage is never hammered.

Claude Code surfaces a server-side overload as a line like
``API Error: 529 Overloaded`` and then sits idle: the conversation is intact, so
a single "continue" resumes it once the overload clears. This watch wakes when a
session goes quiet (the idle / stale signals), reads the recent screen, and when
it matches the transient-error pattern nudges the session to resume.

But a nudge that itself 529s must not become a tight retry loop. So each
consecutive stall doubles the wait before the next nudge — exponential backoff
(``base_secs`` → ``max_secs``), tracked per session in the watch's **lookaside
state** and driven by its **dynamic self-wake**: the watch re-triggers itself at
the soonest due recheck (``rnd.wake_in``) rather than polling. While a session is
inside its backoff window the watch leaves it alone — quietly waiting, not
re-prompting. After ``max_attempts`` consecutive failures it escalates (marks the
session ``attention``) and stops re-prompting until the session recovers, at
which point it drops the session from tracking and clears the mark.

State shape (opaque to the engine), per session id::

    {"<session-id>": {"attempts": <int>, "next": <epoch-seconds>, "escalated": <bool>}}

Honours dry_run: would-be nudges/marks are logged as actions and nothing
mutates. Needs ``nudge`` to resume and ``mark`` to escalate; without a capability
it records the would-be action and moves on.
"""

import re
import time

from weaver_loom import Round

#: Wake when a session goes quiet — a finished turn (idle) or no activity
#: (stale). The watch also re-triggers itself via wake_in for backoff rechecks.
TRIGGERS = {"on": ["session.idle", "session.stale"]}

#: The transient-error signature, overridable via params.pattern. Requires the
#: `API Error: <code>` banner Claude Code itself prints — a bare `overloaded`
#: (with no code) matched any mention of that ordinary English word anywhere
#: on screen, including in the agent's own prose, and nudged sessions that
#: were never actually stalled. `429` catches rate limiting, `5\d\d` sibling
#: server errors (529 overloaded, 500, 503, ...).
DEFAULT_PATTERN = r"(?i)api error:\s*(429|5\d\d)\b"

#: What to type to resume; the conversation is intact, so "continue" is enough.
DEFAULT_NUDGE = "continue"

#: First backoff, the cap it doubles toward, and how many consecutive failures
#: to re-prompt through before escalating instead. All overridable via params.
DEFAULT_BASE_SECS = 30
DEFAULT_MAX_SECS = 900
DEFAULT_MAX_ATTEMPTS = 6

#: How the escalation marks a session it has given up re-prompting.
ESCALATE_VALUE = "attention"


def _conf(rnd):
    """The round's tuned parameters, with defaults filled in."""
    p = rnd.params or {}
    return {
        "pattern": re.compile(p.get("pattern") or DEFAULT_PATTERN),
        "nudge": p.get("nudge") or DEFAULT_NUDGE,
        "base": float(p.get("base_secs") or DEFAULT_BASE_SECS),
        "cap": float(p.get("max_secs") or DEFAULT_MAX_SECS),
        "max_attempts": int(p.get("max_attempts") or DEFAULT_MAX_ATTEMPTS),
        "lines": int(p.get("screen_lines") or 0),
    }


def is_stalled(rnd, sid, conf):
    """Whether ``sid``'s current screen shows the transient-error signature.

    Reads the live viewport (``screen_lines`` of scrollback, 0 = just what's on
    screen) so a *recovered* session — whose error has scrolled out of view —
    reads as healthy. A dead pane is treated as not-stalled (nothing to resume)."""
    screen = rnd.preview_or(sid, conf["lines"])
    return bool(conf["pattern"].search(screen))


def backoff(conf, attempts):
    """The delay before the next nudge after ``attempts`` consecutive failures:
    ``base * 2**(attempts-1)``, capped at ``max_secs``."""
    return min(conf["cap"], conf["base"] * (2 ** max(0, attempts - 1)))


def escalate(rnd, sid, attempts):
    """Mark a session the watch has given up re-prompting (capability/dry-run
    aware). Returns nothing; records the action it took or would take."""
    note = "stalled on a transient API error after %d resume attempts" % attempts
    if not rnd.can("mark"):
        rnd.would("escalate", session=sid, note=note + " (mark capability not granted)")
        return
    if rnd.dry_run:
        rnd.would("mark", session=sid, value=ESCALATE_VALUE, note=note)
    else:
        rnd.client.mark(sid, ESCALATE_VALUE, note, by=rnd.name)
        rnd.did("mark", session=sid, value=ESCALATE_VALUE, note=note)


def clear_escalation(rnd, sid):
    """Clear the watch's escalation mark when a session recovers."""
    if not rnd.can("mark"):
        return
    if rnd.dry_run:
        rnd.would("clear", session=sid, key="triage")
    else:
        rnd.client.mark(sid, "ok", by=rnd.name)  # mark "ok" clears the triage tag
        rnd.did("clear", session=sid, key="triage")


def reprompt(rnd, sid, conf):
    """Nudge a stalled session to resume (capability/dry-run aware). Returns True
    when a (real or simulated) nudge was issued, False when the capability is
    absent."""
    if not rnd.can("nudge"):
        rnd.would("nudge", session=sid, text=conf["nudge"], note="nudge capability not granted")
        return False
    if rnd.dry_run:
        rnd.would("nudge", session=sid, text=conf["nudge"])
    else:
        rnd.client.nudge(sid, conf["nudge"], by=rnd.name)
        rnd.did("nudge", session=sid, text=conf["nudge"])
    return True


def main(rnd):
    conf = _conf(rnd)
    now = time.time()
    state = dict(rnd.state or {})

    # The live fleet (scope-filtered). On a reactive idle/stale we examine just
    # the session that fired plus any we are already tracking; on a cron/manual
    # tick (which includes our own self-wake) we sweep the whole fleet, so a wake
    # set for one session also catches others the same outage hit.
    live = {s["id"]: s for s in rnd.sessions()}
    trig_sid = (rnd.trigger or {}).get("session")
    if trig_sid:
        candidates = ({trig_sid} | set(state)) & set(live)
    else:
        candidates = set(live) | (set(state) & set(live))

    nudged = 0
    for sid in sorted(candidates):
        st = state.get(sid) or {}
        attempts = int(st.get("attempts") or 0)
        next_at = float(st.get("next") or 0.0)
        escalated = bool(st.get("escalated"))

        if not is_stalled(rnd, sid, conf):
            # Recovered (or never stalled): drop tracking and clear any mark.
            if st:
                if escalated:
                    clear_escalation(rnd, sid)
                state.pop(sid, None)
            continue

        # Still stalled but inside the backoff window → wait quietly, don't nudge.
        if st and now < next_at:
            continue

        if attempts >= conf["max_attempts"]:
            # Sustained outage: escalate once, then stop re-prompting. Keep a slow
            # recheck so the mark clears once the session finally recovers.
            if not escalated:
                escalate(rnd, sid, attempts)
            state[sid] = {"attempts": attempts, "next": now + conf["cap"], "escalated": True}
            continue

        # Re-prompt, then back off before the next attempt.
        if reprompt(rnd, sid, conf):
            nudged += 1
        attempts += 1
        state[sid] = {"attempts": attempts, "next": now + backoff(conf, attempts), "escalated": False}

    # Forget sessions that have gone away entirely.
    for sid in list(state):
        if sid not in live:
            state.pop(sid, None)

    # Re-arm the dynamic wake at the soonest pending recheck, or clear it when
    # nothing is left to watch. Persist the lookaside state either way.
    pending = [float(st.get("next") or 0.0) for st in state.values()]
    rnd.wake_in(max(1, int(min(pending) - now)) if pending else 0)
    rnd.set_state(state)

    if not candidates:
        rnd.finish("no sessions to check", outcome="noop")
        return
    dry = " (dry run, no writes applied)" if rnd.dry_run else ""
    verb = "would nudge" if rnd.dry_run else "nudged"
    rnd.finish(
        "checked %d, %s %d, tracking %d%s" % (len(candidates), verb, nudged, len(state), dry),
        outcome="ok" if rnd.actions else "noop",
    )


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
