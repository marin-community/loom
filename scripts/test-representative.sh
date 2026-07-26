#!/usr/bin/env bash
#
# Fast local feedback over pure logic plus one amortized journey for each
# setup-heavy feature changed in the profile-first launch round. CI remains the
# exhaustive cargo-workspace and Playwright gate.

set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
export CARGO_TARGET_DIR="$root/target"

# `--lib --bins` is load-bearing: a package-level filter without it also builds
# and links Loom's giant integration target.
cargo test --workspace --lib --bins --locked

# The live-server harness expects this sibling binary. Build it once before the
# selected journeys instead of rediscovering the requirement in every case.
cargo build -p tapestry --locked

journeys=(
  "acp::canonical_handoff_selects_a_strict_profile_and_rejects_class_mismatch"
  "acp::same_source_handoffs_to_different_profiles_have_one_winner"
  "acp::archive_and_delete_win_against_a_waiting_handoff_without_resurrection"
  "profiles::profile_capacity_admission_is_serialized_across_repositories"
  "profiles::profile_clone_copies_environment_atomically_and_honors_source_revision"
  "profiles::deployment_reconcile_serializes_with_registry_mutation"
  "scratch::reused_worktree_launch_and_live_upload_share_one_inventory_boundary"
  "recover::respawn_accepts_same_profile_lifetime_and_rejects_recreate"
  "custom_agents::canonical_launch_executes_the_reviewed_custom_agent_snapshot"
  "auth::session_token_can_delegate_through_the_cli_resolve_then_create_path"
)

for journey in "${journeys[@]}"; do
  cargo test -p loom --test integration "$journey" --locked -- --exact
done

(
  cd python/weaver-loom
  uv run pytest
)

(
  cd e2e
  npx playwright test tests/create.spec.ts --workers=1
)
