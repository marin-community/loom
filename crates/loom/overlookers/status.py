"""status — survey the scoped fleet and stamp a triage mark on each session.

The judgement is best-effort-LLM with a deterministic fallback, so the round
works fully without a real agent:

* when a prompt is configured (params.prompt), ask the daemon's one-shot
  agent for a level + note from the session's screen preview (parsed
  leniently via parse_judgement);
* otherwise — or when the agent is absent or unparseable — fall back to a
  rule that mirrors the session's own `attention` tag as the mark.

An `ok` judgement returns the triage axis to calm (clears the tag) rather
than marking. With the `nudge` capability, a freshly marked session is also
nudged with the note. Honours dry_run: would-be marks are logged as
`{would: "mark"}` actions and nothing mutates.
"""

from weaver_loom import Round, WeaverError, parse_judgement

#: The storable triage values; `ok` is the absence of the tag.
STORABLE = ("attention", "blocked")

#: A scheduled survey of the whole scoped fleet — the engine reads this in
#: register mode. (A status sweep is inherently fleet-wide, so it stays on a
#: cadence rather than a per-session event.)
TRIGGERS = {"cron": "0 * * * *"}


def attention_value(session):
    """The session's own `attention` tag value — `ok` when absent (calm)."""
    for tag in (session.get("branch") or {}).get("tags") or []:
        if tag.get("key") == "attention":
            return tag.get("value") or "ok"
    return "ok"


def judge(rnd, session):
    """Decide a (level, note) for one session: best-effort LLM judgement when
    a prompt is configured, else mirror the agent's self-reported attention."""
    prompt = rnd.params.get("prompt") or ""
    if prompt:
        try:
            screen = rnd.client.preview(session["id"], 200)
        except WeaverError:
            screen = ""
        out = rnd.client.agent(
            f"{prompt}\n\nSession screen:\n{screen}\n", rnd.model, rnd.effort
        )
        judged = parse_judgement(out) if out else None
        if judged:
            return judged

    attention = attention_value(session)
    level = attention if attention in STORABLE else "ok"
    return level, f"attention is {attention}"


def main(rnd):
    can_mark = rnd.can("mark")
    can_nudge = rnd.can("nudge")
    marked = 0
    counts = {}

    for session in rnd.sessions():
        level, note = judge(rnd, session)

        # An `ok` judgement returns the triage axis to calm rather than
        # marking: clear the tag (the server records the cleared-tag event —
        # the audit rule). The dry-run path mutates nothing.
        if level not in STORABLE:
            if can_mark and not rnd.dry_run:
                rnd.client.mark(session["id"], "ok", by=rnd.name)
            continue

        if not can_mark:
            rnd.did("observe", session=session["id"], level=level, note=note)
            continue

        marked += 1
        counts[level] = counts.get(level, 0) + 1
        if rnd.dry_run:
            rnd.would("mark", session=session["id"], level=level, note=note)
            continue

        rnd.client.mark(session["id"], level, note, by=rnd.name)
        rnd.did("mark", session=session["id"], level=level, note=note)

        # The nudge rung of the intervention ladder: only when capability-
        # granted. Best-effort; a session with no live terminal just no-ops.
        if can_nudge:
            text = f"[overlooker {rnd.name}] {note}"
            try:
                rnd.client.nudge(session["id"], text, by=rnd.name)
                rnd.did("nudge", session=session["id"], text=text)
            except WeaverError:
                pass

    if rnd.surveyed == 0:
        rnd.finish("surveyed 0 sessions in scope", outcome="noop")
        return

    breakdown = ", ".join(f"{n} {level}" for level, n in sorted(counts.items()))
    dry = " (dry run, no marks applied)" if rnd.dry_run else ""
    verb = "would mark" if rnd.dry_run else "marked"
    if breakdown:
        summary = f"surveyed {rnd.surveyed}, {verb} {marked} ({breakdown}){dry}"
    else:
        summary = f"surveyed {rnd.surveyed}, {verb} 0{dry}"
    rnd.finish(summary, outcome="ok")


if __name__ == "__main__":
    Round.main(main, TRIGGERS)
