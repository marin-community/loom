# Configuration policy

Loom resolves every registered setting through one explicit precedence chain:

| Precedence | Source | Owned by | How to change it |
|---|---|---|---|
| 1 | Runtime override | operator | Settings, `PATCH /api/settings`, or `loom config set` |
| 2 | Deployment default | infrastructure repository | `loom deployment apply` / `POST /api/deployment/reconcile` |
| 3 | Built-in default | Loom release | `weaver-core::config::REGISTRY` |

The runtime and deployment layers are separate database tables. A live edit
therefore does not destroy the deployment's declared value: clearing the
runtime override reveals the deployment default, and removing the deployment
default reveals the built-in. `GET /api/settings` reports the effective
`value`, its `source`, and any `deployment_value`; the Settings UI shows the
same provenance.

This policy applies only to registered, non-secret global settings. Secrets
stay in the environment, profile secret references, or the credential-specific
store and are never returned by the settings API. Per-repository setup,
environment, and agent defaults belong in committed `.weaver/config.toml`; a
repo-specific session primer belongs in `WEAVER.md`.

## Deployment manifests

`loom deployment apply` accepts YAML or JSON. Its `settings` map accepts string,
integer, and boolean scalars and validates every key against the same registry
used by the runtime API and Settings UI:

```yaml
settings:
  slack.status_updates: true
  slack.status_artifacts: false
  slack.status_header_template: "On it — <{session_url}>"
  slack.prompt_instructions: |
    Prefer a concise answer in the thread.
    Link a pull request only when the request asks for a change.

profiles:
  - profile:
      name: slack
      description: Slack conversation workflow
      agent_kind: codex
      protocol: acp
      instructions: |
        Follow the organization's repository and landing conventions.
    env: []
federations: []
prune: true
```

Profiles may include multiline `instructions`. This is the preferred home for
organization workflow and response conventions because the same profile works
for user, delegated, GitHub, Slack, watch, and authenticated automation
launches. Slack and GitHub choose their trigger profiles with `slack.profile`
and `github.profile`; both default to `default`. Infrastructure code may read a
checked-in `AGENTS.md` into this manifest field, but Loom receives and exposes
the effective text rather than reading a deployment checkout at runtime.
Loom also seeds lightweight instructions for an untouched `default` profile and
editable `slack` and `github` starters from its runtime posture. The reviewed
starter text lives under `crates/loom-policy/profiles/<name>/instructions.md`.
The origin profiles are opt-in so an upgrade does not silently change the
environment or runtime used by existing triggers.

Profiles may declare `github_repositories`. Inside sessions launched from such
a profile, `git` and `gh` transparently request a short-lived GitHub App
installation token from Loom. Interactive profiles use the list as an
allowlist and stamp only the session's current repository. Strict,
environment-cleared automation profiles retain the complete list for reviewed
cross-repository workflows, so their entries must use one owner. Tokens grant
write access to repository contents, issues, pull requests, Actions, and
workflow files. The configured GitHub App must have those permissions and be
installed on every listed repository.

```yaml
profiles:
  - profile:
      name: external-update
      agent_kind: codex
      protocol: acp
      class: automation
      strict: true
      env_clear: true
      github_repositories:
        - marin-community/marin
        - marin-community/vllm
    env: []
```

Apply it through the authenticated local CLI:

```sh
loom deployment apply --file loom-deployment.yaml
```

With `prune: true`, deployment-managed settings, profiles, and federation
mappings omitted from the manifest are removed from the deployment layer.
Runtime setting overrides are never pruned. A full desired-state manifest
should use `prune: true`; a partial update should use `false`.

Infrastructure tooling may instead send the same document as JSON to the
admin-only `POST /api/deployment/reconcile` route. Reconciliation is
idempotent, so the normal deployment loop can apply it on every rollout.

## Runtime overrides

Runtime edits take effect without rebuilding a deployment:

```sh
loom config set slack.status_updates false
```

The Settings page and `PATCH /api/settings` use the same validation and storage
path. Send `null` to clear an override and inherit again:

```json
{"slack.status_updates": null}
```

`weaver config ls` prints the effective value and source for diagnostics.

## Adding configurable behavior

New global behavior follows one route:

1. Declare a dotted key, type, help text, and safe built-in default in
   `weaver-core::config::REGISTRY`.
2. Read it through `config::get`, `get_or`, or `get_bool`; those helpers apply
   the complete precedence chain.
3. Add focused behavior and validation tests. The registry automatically
   exposes the key to deployment manifests, the REST settings API, and the
   Settings UI.

Prefer typed switches and enums for policy. For prose customization, use
profile `instructions` rather than adding an origin-specific setting. This
keeps the text beside its runtime and tool policy and covers new launch origins
without expanding the global setting registry. Templates should expose a small
documented placeholder set and retain a safe built-in fallback. Configuration
is operator-controlled input, not a secret store.
