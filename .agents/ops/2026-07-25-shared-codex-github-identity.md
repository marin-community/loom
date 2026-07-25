# Shared Codex GitHub identity crossed Loom sessions

## Status

Mitigation implemented locally; production deployment is pending.

## Impact

On 2026-07-24, a Loom Codex session created by `rjpower` posted GitHub issue
comment `5073906073` as `yonromai`. The comment announced a child Loom session
for PR 7606. Romain Yon did not create or interact with either session.

## Evidence

- GitHub attributes comment `5073906073` to the
  `chatgpt-codex-connector` app and the `yonromai` user.
- The parent session `nxm6ul1e` transcript records a
  `codex_apps.github.add_comment_to_issue` invocation at the comment timestamp
  with the exact published body.
- Loom records `nxm6ul1e` as created by `rjpower`; it created child session
  `24f9f8d7`.
- The agent process's `GH_TOKEN` identifies as the Loom bot, so neither the
  injected token nor shell `gh` produced the comment.
- All Loom sessions share the persisted Codex login. That account had a
  server-side GitHub connector authorization whose GitHub identity was
  `yonromai`.

## Root cause

Loom intentionally shares Claude and Codex provider accounts, but Codex's
account-level connected apps were also inherited. The model could therefore
write to GitHub through the persistent connector identity independently of
Loom's injected GitHub credential and session ownership.

This was not a stale Romain session or a recent login by Romain. It was an
`rjpower`-owned session acting through an older connector authorization attached
to the shared Codex account.

## Corrective change

- Keep the shared Claude and Codex authentication state.
- Disable Codex account-level apps in the container at startup.
- Pass `--disable apps` to terminal Codex launches.
- Force `features.apps=false` into ACP `CODEX_CONFIG`, while preserving all
  other operator configuration.
- Continue routing GitHub writes through Loom's injected bot token or the
  restricted server-side GitHub endpoint.

## Verification

- `cargo check -p loom`
- `cargo test -p loom agent::tests::codex_runtime_runs_codex_with_its_prompt -- --exact`
- `cargo test -p loom agent::tests::codex_acp_disables_apps_without_discarding_operator_config -- --exact`
- `cargo test -p loom --test integration acp::codex_acp_launch_maps_the_adapter_contract -- --exact --test-threads=1`

No live Loom process was restarted or reconfigured during the investigation.
