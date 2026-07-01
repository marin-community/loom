#!/usr/bin/env bash
#
# Shared pre-commit / CI gate: Rust formatting + lints and the frontend
# typecheck must all pass.
#
# This is the single source of truth for "is the tree clean?". It is run by:
#   - the git pre-commit hook (.githooks/pre-commit), enabled per-clone with
#     `git config core.hooksPath .githooks` (see AGENTS.md); and
#   - the CI `lint` job (.github/workflows/ci.yml),
# so a commit that passes locally passes CI. Bypass the hook for one commit with
# `git commit --no-verify`.
#
# The agentic lint review (scripts/lint-review.py) is deliberately NOT run here:
# it is a separate, explicit step in the commit → PR flow (see the `pull-request`
# skill and AGENTS.md). Keeping the agent out of the commit hook keeps this gate
# fast and identical to CI, so a slow or flaky review never sits in the commit path.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

echo "▶ cargo fmt --all --check"
if ! cargo fmt --all --check; then
  echo >&2
  echo "✗ formatting check failed — run \`cargo fmt --all\`, then re-stage and commit." >&2
  exit 1
fi

echo "▶ cargo clippy --workspace --all-targets --locked -- -D warnings"
cargo clippy --workspace --all-targets --locked -- -D warnings

# Frontend typecheck (vue-tsc). The clippy step above compiled loom, whose
# build.rs runs `npm install` — so node_modules is normally present by now; we
# install it here only for a fresh or Rust-only checkout that skipped that
# (build.rs writes a placeholder and skips install when npm is absent). Skipped
# entirely when npm isn't installed, matching build.rs's tolerance — CI always
# has npm, so the gate is still enforced there.
frontend="crates/loom/frontend"
if command -v npm >/dev/null 2>&1; then
  echo "▶ vue-tsc --noEmit ($frontend)"
  if [ ! -d "$frontend/node_modules" ]; then
    echo "  installing frontend deps (npm install)…"
    npm --prefix "$frontend" install
  fi
  npm --prefix "$frontend" run typecheck
  echo "✓ fmt + clippy + typecheck clean"
else
  echo "▶ vue-tsc typecheck — skipped (npm not found)"
  echo "✓ fmt + clippy clean"
fi
