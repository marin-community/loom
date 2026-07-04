"""weaver_loom — the Python layer over the loom REST API.

Everything loom can do is exposed over HTTP (`crates/loom/src/web.rs`); this
module is a convenience layer on top, nothing more. It holds no state the API
doesn't, and a script could always speak HTTP directly — the loom daemon stays
the single owner of the live runtime either way.

Two pieces:

* :class:`Client` — a thin, capability-gated wrapper over the REST routes.
  DTOs cross as plain dicts (the shapes `frontend/types.ts` documents);
  mutating calls check the granted capability set *before* issuing a request,
  mirroring the intervention-ladder contract of the `weaver-py` binding.
* :class:`Round` — the watch program context: the round config the engine
  passes in ``$WEAVER_WATCH``, a client granted that round's
  capabilities, the scope-filtered fleet survey, and the
  ``{outcome, summary, actions}`` result the engine reads from stdout.

A program declares **what wakes it** with a ``TRIGGERS`` manifest and ends with
``Round.main(main, TRIGGERS)`` — the engine reads the manifest in *register
mode* and then only calls the round for those events, handing it the session
that changed via :meth:`Round.triggered_sessions`::

    from weaver_loom import Round

    TRIGGERS = {"on": ["pr.merged"]}

    def main(rnd):
        for session in rnd.triggered_sessions():
            github = (session.get("branch") or {}).get("github") or {}
            if github.get("pr_state") == "MERGED":
                rnd.would("archive", session=session["id"], note="PR merged")
        rnd.finish(f"surveyed {rnd.surveyed}, {len(rnd.actions)} findings")

    if __name__ == "__main__":
        Round.main(main, TRIGGERS)

The manifest keys are ``cron`` / ``every`` (a schedule) and ``on`` (a list of
normalized trigger events — ``session.started``, ``session.idle``,
``session.exited``, ``session.attention``, ``session.stale``, ``triage.changed``,
``pr.opened``, ``pr.checks_red``, ``pr.checks_green``, ``pr.merged``,
``pr.review_changed`` — each optionally ``name=level``).

The loom engine vendors this module onto ``PYTHONPATH`` for every script it
runs, so a program needs no install step; for standalone iteration, install it
from the weaver repo with ``uv pip install -e python/weaver-loom``.
"""

import json
import os
import subprocess
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_BASE = "http://127.0.0.1:7878"

#: The intervention ladder, calm → loud. ``observe`` is implicit.
CAPABILITIES = ["observe", "mark", "escalate", "nudge", "interrupt", "launch"]

#: Lifecycle states with no live session behind them.
TERMINAL_STATUSES = {"done", "error", "archived"}

#: The levels an agent judgement may name: the two storable triage values plus
#: the calm ``ok`` — recognising ``ok`` lets a judgement explicitly return the
#: axis to calm (the caller then clears the tag rather than marking).
JUDGED_LEVELS = ("ok", "attention", "blocked")

#: The storable loud values, calm → urgent. ``ok``/empty is never stored (it
#: clears the tag). A tag whose value is one of these is *loud* — it raises a
#: badge — regardless of key; the key is the type. Mirrors weaver-core's
#: ``ATTENTION_VALUES``.
LOUD_VALUES = ("attention", "blocked")

#: The quiet, soothing mark loom stamps when an agent goes quiet (a finished
#: turn or a ``waiting`` lull): the calm "resting, no one needed" state. Not on
#: :data:`LOUD_VALUES`, so it never raises a badge. The status watch replaces it
#: with a real loud status — or clears it — when the session actually needs a
#: human. Mirrors weaver-core's ``IDLE_KEY`` / ``IDLE_VALUE``.
IDLE_KEY = "idle"
IDLE_VALUE = "idle"

#: The quiet values that **park** a session *below* the calm default in the
#: dashboard's fleet sort — the opposite end of the ladder from
#: :data:`LOUD_VALUES`. A parked session is waiting on an external actor (a human
#: PR reviewer, …) and needs nothing from the user, so a scanning user skips past
#: it. The value names what is awaited (``review``, …); a watch picks its own axis
#: key and the value carries the meaning. Quiet by design — never a badge.
#: Mirrors weaver-core's ``PARKED_VALUES``.
PARKED_VALUES = ("review",)


def parse_judgement(text):
    """Parse an agent judgement into ``(level, note)``, or ``None``.

    Lenient by design: the first line containing a recognised triage word
    yields the level; the rest of that line (after a ``:`` or ``-``) is the
    note. ``None`` means no level was found — the caller falls back to its
    deterministic rule.
    """
    for line in (text or "").splitlines():
        words = "".join(c if c.isalpha() else " " for c in line.lower()).split()
        for level in JUDGED_LEVELS:
            if level in words:
                cuts = [i for i in (line.find(":"), line.find("-")) if i >= 0]
                note = line[min(cuts) + 1 :].strip() if cuts else ""
                return level, note or line.strip()
    return None


def _slug(text):
    """A short tag key: lowercased, runs of non-alphanumerics → single hyphens,
    trimmed. ``"Needs Review!"`` → ``"needs-review"``; ``""`` → ``""``."""
    cleaned = "".join(c if c.isalnum() else "-" for c in (text or "").strip().lower())
    return "-".join(p for p in cleaned.split("-") if p)


def _json_array(text):
    """The first ``[`` … last ``]`` slice of ``text``, or ``None`` — a forgiving
    grab so the array survives surrounding prose or a code fence."""
    text = text or ""
    start, end = text.find("["), text.rfind("]")
    return text[start : end + 1] if 0 <= start < end else None


def parse_tag_recommendations(text):
    """Parse a judge model's reply into a list of ``{key, value, note}`` tags, or
    ``None`` when the reply carries no recognizable recommendation.

    The reply is expected to be a JSON array of objects (the model may wrap it in
    prose or a code fence — the first ``[`` … last ``]`` is extracted). Each
    entry needs a non-empty key and a ``value`` on the loud ladder
    (:data:`LOUD_VALUES`); malformed entries are dropped and keys are slugged and
    de-duplicated. An empty array parses to ``[]`` — the explicit "nothing
    needed" verdict, so the caller clears its marks. ``None`` (no array / invalid
    JSON) means "no judgement", distinct from a calm verdict: the caller then
    leaves marks untouched rather than guess."""
    blob = _json_array(text)
    if blob is None:
        return None
    try:
        items = json.loads(blob)
    except (ValueError, TypeError):
        return None
    if not isinstance(items, list):
        return None
    out, seen = [], set()
    for item in items:
        if not isinstance(item, dict):
            continue
        key = _slug(item.get("key"))
        value = str(item.get("value") or "").strip().lower()
        if not key or value not in LOUD_VALUES or key in seen:
            continue
        seen.add(key)
        out.append({"key": key, "value": value, "note": str(item.get("note") or "").strip()})
    return out


class WeaverError(RuntimeError):
    """A failed request or an unreachable loom server."""


class CapabilityDenied(WeaverError):
    """A mutating call attempted without the capability it requires."""


def gh_json(args, cwd=None, timeout=30):
    """Run ``gh <args>`` and parse its stdout as JSON.

    A watch that shells out to ``gh`` needs exactly one thing: a way to tell
    "gh couldn't answer this one call" (worth logging and moving on) apart
    from "our code is broken" (worth a traceback). This draws that line once:
    every failure — gh missing, the call timing out, a non-zero exit, output
    that isn't JSON — raises :class:`WeaverError` carrying gh's own message,
    so a caller that wants to tolerate an unreadable PR can catch that one
    type and *keep the reason* in its note instead of discarding it. Anything
    that isn't a `WeaverError` (a bug in the caller's own code) is left to
    propagate as-is.
    """
    try:
        out = subprocess.run(
            ["gh", *args], cwd=cwd, capture_output=True, text=True, timeout=timeout
        )
    except FileNotFoundError as e:
        # cwd missing raises the same exception type as gh itself missing;
        # e.filename names whichever path the OS actually failed to find.
        what = "gh" if e.filename in (None, "gh") else e.filename
        raise WeaverError(f"{what} not found: {e}") from e
    except subprocess.TimeoutExpired as e:
        raise WeaverError(f"gh {' '.join(args)} timed out after {timeout}s") from e
    if out.returncode != 0:
        detail = out.stderr.strip() or out.stdout.strip() or "no output"
        raise WeaverError(f"gh {' '.join(args)}: exit {out.returncode}: {detail}")
    try:
        return json.loads(out.stdout)
    except ValueError as e:
        raise WeaverError(f"gh {' '.join(args)}: unparseable JSON: {e}") from e


def _base_url(base=None):
    """Resolve a base URL: the argument, else ``$WEAVER_API``, else the loom
    default. A bare ``host:port`` is accepted."""
    base = (base or os.environ.get("WEAVER_API") or DEFAULT_BASE).strip().rstrip("/")
    if not base.startswith(("http://", "https://")):
        base = "http://" + base
    return base


class Client:
    """A capability-gated client for one loom server.

    ``observe`` is implicit, so read methods always work; each mutating method
    raises :class:`CapabilityDenied` when its capability wasn't granted at
    construction. Sessions and branches cross as plain dicts.
    """

    def __init__(self, base=None, capabilities=None):
        self.base = _base_url(base)
        self.capabilities = list(capabilities or [])

    def can(self, cap):
        """Whether this client holds ``cap`` (``observe`` is always held)."""
        return cap == "observe" or cap in self.capabilities

    def _gate(self, cap):
        if not self.can(cap):
            raise CapabilityDenied(
                f"capability '{cap}' not granted (granted: {self.capabilities or ['observe']})"
            )

    def _request(self, method, path, body=None):
        url = self.base + "/api" + path
        data = json.dumps(body).encode() if body is not None else None
        headers = {"Content-Type": "application/json"} if data else {}
        # The engine injects $LOOM_TOKEN (the machine-local token); present it so
        # the round authenticates even when loopback trust is off.
        token = (os.environ.get("LOOM_TOKEN") or "").strip()
        if token:
            headers["Authorization"] = "Bearer " + token
        req = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with urllib.request.urlopen(req) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as e:
            detail = e.read().decode("utf-8", errors="replace").strip()
            raise WeaverError(f"{method} {path}: HTTP {e.code}: {detail}") from None
        except urllib.error.URLError as e:
            raise WeaverError(f"{method} {path}: {e.reason}") from None
        return json.loads(raw) if raw else None

    # -- Reads (observe) ----------------------------------------------------

    def sessions(self):
        """Every active session (``GET /api/sessions``)."""
        return self._request("GET", "/sessions")

    def session(self, key):
        """One session by id, branch id, branch name, or ``repo:branch``."""
        return self._request("GET", f"/sessions/{key}")

    def preview(self, key, lines=0):
        """The session's terminal as text, with ``lines`` of scrollback."""
        reply = self._request("GET", f"/sessions/{key}/preview?lines={lines}")
        return (reply or {}).get("screen", "")

    def diff(self, key):
        """The worktree file tree + change map vs the diff base."""
        return self._request("GET", f"/sessions/{key}/tree")

    def programs(self):
        """The builtin watch program registry."""
        return self._request("GET", "/watches/programs")

    def agent(self, prompt, model="", effort=""):
        """Run a one-shot headless agent in the daemon and return its stdout,
        or ``None`` when the agent is absent or failed (degrade gracefully —
        the daemon never errors a missing agent). A judgement primitive: pair
        with :func:`parse_judgement`."""
        reply = self._request(
            "POST",
            "/agent/oneshot",
            {"prompt": prompt, "model": model or "", "effort": effort or ""},
        )
        return (reply or {}).get("output")

    # -- Writes (capability-gated) --------------------------------------------

    def set_tag(self, key, tag_key, value, note="", by=None):
        """Set (upsert) a tag on a session; needs ``mark``."""
        self._gate("mark")
        body = {"value": value, "note": note}
        if by is not None:
            body["by"] = by
        return self._request("PUT", f"/sessions/{key}/tags/{tag_key}", body)

    def clear_tag(self, key, tag_key, by=None):
        """Clear a tag — how a loud axis returns to calm; needs ``mark``.
        ``by`` attributes the clear (a watch name)."""
        self._gate("mark")
        query = f"?by={urllib.parse.quote(by, safe='')}" if by else ""
        return self._request("DELETE", f"/sessions/{key}/tags/{tag_key}{query}")

    def mark(self, key, level, note="", by=None):
        """Stamp the watch's ``triage`` mark; needs ``mark``. A ``level``
        of ``attention``/``blocked`` sets it; empty or ``ok`` clears it."""
        if not level or level == "ok":
            return self.clear_tag(key, "triage", by)
        return self.set_tag(key, "triage", level, note, by)

    def nudge(self, key, text, submit=True, by=None):
        """Type a message into the session's agent pane; needs ``nudge``.
        ``by`` attributes the recorded ``nudge`` audit event."""
        self._gate("nudge")
        body = {"text": text, "submit": submit}
        if by is not None:
            body["by"] = by
        return self._request("POST", f"/sessions/{key}/send", body)

    def interrupt(self, key):
        """Send a break (Escape) to stop the current turn; needs ``interrupt``."""
        self._gate("interrupt")
        return self._request("POST", f"/sessions/{key}/interrupt", {})


class Round:
    """One watch round, as the engine runs it.

    Reads the round config from ``$WEAVER_WATCH`` (``{id, name, program,
    params, scope, capabilities, model, effort, dry_run, state}``), builds a
    :class:`Client` granted that round's capabilities, accumulates the action
    log, and prints the result the engine parses. A mutating program must
    check :attr:`dry_run` (record a :meth:`would` action instead of acting).

    Two optional primitives let a watch persist memory and pace itself across
    rounds: :attr:`state` (read) + :meth:`set_state` (write) is its lookaside
    scratch memory, and :meth:`wake_in` schedules a dynamic self-trigger — the
    pair a backoff watcher uses to track per-session retries and recheck them on
    an exponential schedule instead of polling.
    """

    def __init__(self, config=None, client=None):
        if config is None:
            config = json.loads(os.environ.get("WEAVER_WATCH", "{}"))
        self.config = config
        self.name = config.get("name", "")
        self.params = config.get("params") or {}
        self.scope = config.get("scope") or {}
        #: The watch's configured agent model / reasoning effort — pass
        #: these to :meth:`Client.agent` so judgement honours the config.
        self.model = config.get("model", "")
        self.effort = config.get("effort", "")
        #: ``run`` (execute a round) or ``register`` (declare the manifest). The
        #: engine sets it via the config and ``$WEAVER_WATCH_MODE``; in
        #: register mode the round is neutered (no mutations, empty survey) so a
        #: script that doesn't use :meth:`main` can't act when merely asked what
        #: wakes it.
        self.mode = config.get("mode") or os.environ.get("WEAVER_WATCH_MODE") or "run"
        #: The triggering context the engine passed: ``{event, level, session,
        #: branch, repo}`` for a reactive round; ``{event: "cron"|"manual"}``
        #: otherwise. Drives :meth:`triggered_sessions`.
        self.trigger = config.get("trigger") or {}
        self.dry_run = bool(config.get("dry_run")) or self.mode == "register"
        caps = [] if self.mode == "register" else (config.get("capabilities") or [])
        self.client = client or Client(capabilities=caps)
        self.actions = []
        #: How many live sessions the last survey admitted.
        self.surveyed = 0
        #: The watch's **lookaside state** — its scratch memory from the
        #: previous round, carried across rounds by the engine. A plain dict the
        #: program reads at the top of a round; write the next round's state with
        #: :meth:`set_state`. ``{}`` when the program keeps none.
        self.state = config.get("state") or {}
        #: The next-state write (``None`` until :meth:`set_state`) and the
        #: dynamic-wake request (``None`` until :meth:`wake_in`); both surface in
        #: :meth:`finish` only when set, so a program that uses neither is
        #: unchanged.
        self._next_state = None
        self._wake_in = None

    @staticmethod
    def main(fn, triggers=None):
        """Entry point that handles both engine modes — a script ends with
        ``Round.main(main, TRIGGERS)``.

        In ``register`` mode (the engine asking what events wake this watch) it
        prints the subscription manifest and returns *without running*, so
        declaring triggers has no side effects. Otherwise it constructs a
        :class:`Round` and calls ``fn(round)``.
        """
        if os.environ.get("WEAVER_WATCH_MODE", "run") == "register":
            print(json.dumps(triggers or {}))
            return
        fn(Round())

    def can(self, cap):
        """Whether this round holds ``cap`` (``observe`` is always held)."""
        return self.client.can(cap)

    def sessions(self):
        """The round's survey: the live fleet, scope-filtered.

        Terminal sessions are skipped; the watch's scope applies its
        ``attention`` filter (``!ok`` or an exact level; an absent tag is the
        calm ``ok``) and its ``repo`` pin. Sets :attr:`surveyed`. Empty in
        register mode (the round must not touch the fleet just to be asked what
        wakes it).
        """
        if self.mode == "register":
            self.surveyed = 0
            return []
        admitted = []
        for session in self.client.sessions():
            if session.get("status") in TERMINAL_STATUSES:
                continue
            if not self._admits(session):
                continue
            admitted.append(session)
        self.surveyed = len(admitted)
        return admitted

    def triggered_sessions(self):
        """The session(s) the triggering event concerns — the survey a reactive
        round should act on.

        When the trigger names a single session/branch (a reactive event), this
        returns just that one (scope-filtered), so the round acts on the branch
        that changed instead of re-surveying the whole fleet — the difference
        between one GitHub call and one per session. When it names none (a
        ``cron``/``manual`` tick) it falls back to the full :meth:`sessions`
        survey. Empty in register mode, like :meth:`sessions` — a script asked
        only what wakes it must not touch the fleet. Sets :attr:`surveyed`.
        """
        if self.mode == "register":
            self.surveyed = 0
            return []
        sid = (self.trigger or {}).get("session")
        bid = (self.trigger or {}).get("branch")
        if not sid and not bid:
            return self.sessions()
        admitted = []
        for session in self.client.sessions():
            if sid and session.get("id") != sid:
                continue
            if bid and (session.get("branch") or {}).get("id") != bid:
                continue
            if not self._admits(session):
                continue
            admitted.append(session)
        self.surveyed = len(admitted)
        return admitted

    def _admits(self, session):
        branch = session.get("branch") or {}
        want = self.scope.get("attention")
        if want:
            have = "ok"
            for tag in branch.get("tags") or []:
                if tag.get("key") == "attention":
                    have = tag.get("value") or "ok"
            matched = have != want[1:] if want.startswith("!") else have == want
            if not matched:
                return False
        repo = self.scope.get("repo")
        if repo and branch.get("repo_root") != repo:
            return False
        return True

    def preview_or(self, session_id, lines=0, default=""):
        """The session's terminal preview, or ``default`` when the pane is
        already gone (:class:`WeaverError`) — the "read what's on screen but
        tolerate a dead session" pattern every screen-reading watch needs. A
        session vanishing between the survey and the read is expected and
        recoverable; anything else (a bug in the caller) still propagates."""
        try:
            return self.client.preview(session_id, lines)
        except WeaverError:
            return default

    def would(self, action, **fields):
        """Record a stubbed would-do action — a read-only or dry-run finding."""
        self.actions.append({"would": action, **fields})

    def did(self, action, **fields):
        """Record an action the round actually performed."""
        self.actions.append({"action": action, **fields})

    def set_state(self, state):
        """Persist ``state`` (a JSON-able dict) as this watch's lookaside
        state, replacing the prior one. It is handed back as :attr:`state` on the
        next round — the program's across-round memory (the engine carries it; no
        session or file needed).

        Must be a dict: the engine only persists an object, so a non-dict would be
        silently dropped (leaving the prior state in place). Reject it here so the
        mistake surfaces at the program boundary instead."""
        if not isinstance(state, dict):
            raise TypeError(f"set_state expects a dict, got {type(state).__name__}")
        self._next_state = state

    def wake_in(self, seconds):
        """Ask the engine to re-run this watch once in ``seconds`` — a
        dynamic self-trigger, independent of any cron cadence. Use it to schedule
        the next look in a backoff loop (``rnd.wake_in(60)``). ``seconds <= 0``
        clears any pending wake (``rnd.wake_in(0)`` — "nothing left to recheck").
        Re-arm it every round you still have pending work; the wake is one-shot."""
        self._wake_in = int(seconds)

    def finish(self, summary, outcome=None):
        """Print the round result the engine reads from stdout. ``outcome``
        defaults to ``ok`` when any action was recorded, else ``noop``. A
        :meth:`set_state` write and a :meth:`wake_in` request ride along when
        set, so the engine persists them after the round."""
        result = {
            "outcome": outcome or ("ok" if self.actions else "noop"),
            "summary": summary,
            "actions": self.actions,
        }
        if self._next_state is not None:
            result["state"] = self._next_state
        if self._wake_in is not None:
            result["wake_in"] = self._wake_in
        print(json.dumps(result))
