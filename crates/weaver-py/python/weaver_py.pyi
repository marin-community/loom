"""Type stubs for the `weaver_py` extension module.

These describe the surface the compiled module exposes so editors and type
checkers can assist a script author. The implementation lives in Rust
(`src/lib.rs`); this file is the typed mirror.
"""

from typing import Any

CAPABILITIES: list[str]
"""The capability vocabulary, calm -> loud: the intervention ladder
(`observe`, `mark`, `escalate`, `nudge`, `interrupt`, `launch`)."""

class WeaverError(RuntimeError):
    """A transport/server error: a failed request, a decode error, or an
    unreachable loom server."""

class CapabilityDenied(ValueError):
    """A mutating call was attempted without the capability it requires."""

class Client:
    """A capability-gated, synchronous client for one loom server."""

    def __init__(
        self,
        base: str | None = ...,
        capabilities: list[str] | None = ...,
    ) -> None:
        """`base` defaults to `$WEAVER_API` (or `http://127.0.0.1:7878`).
        `capabilities` is the granted set; `observe` is always implied."""

    @property
    def base(self) -> str:
        """The base URL this client targets."""

    @property
    def capabilities(self) -> list[str]:
        """The granted capability set (excluding the implicit `observe`)."""

    def can(self, cap: str) -> bool:
        """Whether this client holds `cap` (`observe` is always true)."""

    # -- Reads (observe) --
    def sessions(self) -> list[dict[str, Any]]:
        """Every active session, as a list of dicts. Each carries a nested
        `branch` whose `tags` is a list of `{key, value, note, set_by, set_at}`
        (the well-known `attention`/`triage` keys plus any free-form key);
        absence of a key is the calm state."""

    def session(self, key: str) -> dict[str, Any]:
        """One session by key (id, branch id, branch name, or `repo:branch`)."""

    def preview(self, key: str, lines: int = ...) -> str:
        """The session's tmux pane as text, with `lines` of extra scrollback."""

    def diff(self, key: str) -> dict[str, Any]:
        """The worktree file tree + change map vs the diff base."""

    # -- Writes (capability-gated) --
    def set_tag(
        self,
        key: str,
        tag_key: str,
        value: str,
        note: str = ...,
        by: str | None = ...,
    ) -> dict[str, Any]:
        """Set (upsert) a tag on a session — `tag_key` is the axis
        (`attention`, `triage`, or any free-form key). Needs `mark`."""

    def clear_tag(self, key: str, tag_key: str) -> dict[str, Any]:
        """Clear a tag — how a loud axis returns to calm. Needs `mark`."""

    def mark(
        self,
        key: str,
        level: str,
        note: str = ...,
        by: str | None = ...,
    ) -> dict[str, Any]:
        """Stamp the triage mark on a session — a convenience over the `triage`
        tag (empty/`ok` clears it). Needs `mark`."""

    def nudge(self, key: str, text: str, submit: bool = ...) -> dict[str, Any]:
        """Type a message into a session's agent pane. Needs `nudge`."""

    def interrupt(self, key: str) -> dict[str, Any]:
        """Send a break to interrupt the agent's current turn. Needs `interrupt`."""

    def warm_session(self) -> Any:
        """The persistent overlooker session. Not yet available (plan T12)."""

    def run_agent(self, prompt: str) -> Any:
        """Spawn a fresh one-shot agent. Not exposed over weaver-api (engine-only)."""
