#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Agent lint review — run the docs/lint.md catalog over a diff and error on slop.

The "claude shell technique" (borrowed from marin-community/marin): build one
prompt = the lint catalog + instructions + the diff, pipe it to `claude -p`
(print/headless mode) as a fresh isolated session, and parse the findings it
prints back. `cargo fmt` / `clippy` own the mechanical checks; this catches the
judgement-call slop an LLM coding agent tends to leave behind — naming, shape,
dead code, duplication, comment/test quality. See docs/lint.md.

Run it as a step in the commit → PR flow (the `pull-request` skill), after you
commit and before you open the PR. It is deliberately NOT wired into the
pre-commit hook — that stays a fast fmt + clippy gate (scripts/pre-commit.sh),
so a slow or flaky review never sits in the commit path. It reviews the whole
branch against its merge-base with main.

Usage:
  scripts/lint-review.py            # review this branch vs its merge-base with main

Exit status:
  0  no blocking findings — or the review was skipped (see below)
  1  one or more findings at/above the blocking confidence threshold

The review SKIPS (exit 0) when it can't run a clean pass: `claude` not on PATH,
no in-scope changes, the agent timing out, or the agent itself erroring. A flaky
or absent agent must not block progress — only real findings do.

Knobs (env vars):
  WEAVER_SKIP_AGENT_LINT=1            skip the review entirely
  WEAVER_LINT_AGENT_CMD="claude -p"   headless agent invocation (stdin -> findings)
  WEAVER_LINT_MIN_CONFIDENCE=0.9      findings at/above this confidence block
  WEAVER_LINT_TIMEOUT=600             seconds before the agent run is abandoned

Escape hatch for a false positive: add `// wl-allow: <code>` on the cited line
(see docs/lint.md).
"""

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys

CATALOG = "docs/lint.md"
GLOBS = ["*.rs", "*.ts", "*.vue"]

INSTRUCTIONS = (
    "Apply the lint catalog above to the diff below. Follow the "
    '"Detector usage" section exactly: emit one finding per line in the format '
    "it specifies, and emit nothing at all when there are no findings. Work only "
    "from the diff as given — do not re-derive it or read other files."
)

# Markers of the *calling* Claude Code session. Stripped before exec so the
# sub-agent runs as a fresh, isolated session on subscription auth — not nested
# in our transcript, not billed via the metered API key.
STRIPPED_ENV = (
    "ANTHROPIC_API_KEY",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
)

# Catalog "Output format":  <path>:<line>: <code> (<confidence>) <message>
FINDING_RE = re.compile(r"^[^:\s]+:\d+: wl-[A-Za-z-]+ \((?P<conf>[\d.]+)\) .+$")


def skip(msg: str) -> None:
    print(f"  ⓘ agent lint skipped: {msg}")
    sys.exit(0)


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True)


def select_diff() -> tuple[str, str]:
    """Return (diff, human-readable scope), or skip if there's nothing to review."""
    # Diff the working tree against the merge-base so the review covers all branch
    # work, committed or not. Resolve the base leniently: a fresh clone may lack
    # origin/main, a detached worktree may lack a remote at all.
    base = ""
    for ref in ("origin/main", "main"):
        if run(["git", "rev-parse", "--verify", "--quiet", ref]).returncode == 0:
            base = run(["git", "merge-base", ref, "HEAD"]).stdout.strip()
            if base:
                break
    if not base:
        skip("could not resolve a merge-base with main (try: git fetch origin main)")
    diff = run(["git", "diff", base, "-U15", "--", *GLOBS]).stdout
    short = run(["git", "rev-parse", "--short", base]).stdout.strip()
    return diff, f"branch vs {short}"


def parse_min_confidence() -> float:
    raw = os.environ.get("WEAVER_LINT_MIN_CONFIDENCE", "0.9")
    try:
        val = float(raw)
    except ValueError:
        val = -1.0
    if not 0.0 <= val <= 1.0:
        print(f"  ⚠ WEAVER_LINT_MIN_CONFIDENCE={raw!r} is not a number in [0,1]; using 0.9", file=sys.stderr)
        return 0.9
    return val


def main() -> int:
    # No flags: the review always covers the branch vs its merge-base with main.
    # parse_args() still handles -h/--help and rejects stray arguments.
    argparse.ArgumentParser(description="Run the docs/lint.md catalog over the branch diff via a headless agent.").parse_args()

    os.chdir(run(["git", "rev-parse", "--show-toplevel"]).stdout.strip())

    if os.environ.get("WEAVER_SKIP_AGENT_LINT"):
        skip("WEAVER_SKIP_AGENT_LINT is set")
    if not os.path.isfile(CATALOG):
        skip(f"no catalog at {CATALOG}")

    agent_cmd = shlex.split(os.environ.get("WEAVER_LINT_AGENT_CMD", "claude -p"))
    if not agent_cmd or shutil.which(agent_cmd[0]) is None:
        skip(f"agent '{agent_cmd[0] if agent_cmd else '(empty)'}' not found on PATH")

    min_conf = parse_min_confidence()
    timeout = int(os.environ.get("WEAVER_LINT_TIMEOUT", "600"))

    diff, scope = select_diff()
    if not diff.strip():
        skip(f"no Rust/TS/Vue changes in {scope}")

    with open(CATALOG) as f:
        catalog = f.read()
    prompt = f"{catalog}\n\n{INSTRUCTIONS}\n\n```diff\n{diff}\n```\n"

    env = {k: v for k, v in os.environ.items() if k not in STRIPPED_ENV}
    print(f"▶ agent lint review ({scope}) via '{' '.join(agent_cmd)}' — advisory below, blocks at confidence ≥ {min_conf:g}")

    try:
        result = subprocess.run(agent_cmd, input=prompt, capture_output=True, text=True, env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        skip(f"agent timed out after {timeout}s")
    if result.returncode != 0:
        skip(f"agent exited {result.returncode} (tooling issue, not a lint finding)")

    findings = [ln for ln in result.stdout.splitlines() if FINDING_RE.match(ln.strip())]
    if not findings:
        print("✓ agent lint: no findings")
        return 0

    print("\n" + "\n".join(findings) + "\n")

    blocking = [ln for ln in findings if float(FINDING_RE.match(ln.strip())["conf"]) >= min_conf]
    if blocking:
        print(f"✗ {len(blocking)} blocking lint finding(s) at confidence ≥ {min_conf:g}.", file=sys.stderr)
        print("  Fix them, suppress a false positive with `// wl-allow: <code>` on the line", file=sys.stderr)
        print("  (see docs/lint.md), or bypass once with `git commit --no-verify`.", file=sys.stderr)
        return 1

    print(f"ⓘ findings above are below the ≥ {min_conf:g} blocking threshold — advisory only.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
