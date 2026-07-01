#!/usr/bin/env bash
#
# Shared pre-commit / CI gate: formatting and lints must both pass.
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

echo "✓ fmt + clippy clean"
