# Configuration policy

## Configuration ownership

Choose the configuration surface from who owns the value and how widely it
should apply:

| Owner | Configure in | Use for |
|---|---|---|
| User | **Settings → Account** and **Preferences** | Personal sign-in, password, API tokens, optional interactive-session GitHub PAT, and terminal appearance |
| Session profile | **Settings → Agents & profiles** or a deployment manifest | Agent/model policy, instructions, GitHub repository allowlists, shared environment, and write-only session secrets |
| Deployment | The **Administration** settings; deployment IaC for the production source | Approved users and roles, the Loom GitHub App, Slack App, federations, runtime policy, and machine-wide credentials or files |
| Repository | `.weaver/config.toml`, `WEAVER.md`, and `AGENTS.md` | Non-secret repository setup, environment, and workflow instructions |

The Administration section is visible only to admins. Users can launch and
operate sessions, repositories, reviews, per-session shells, and shared layout;
inspect watch activity, redacted server logs, and diagnostics; and manage their
own account and preferences. Admins also manage deployment-wide policy,
integrations, profiles, shared environment, watch definitions and runs, the raw
operator log, the host scratch shell, and access. Existing users become admins
when the role migration is applied; newly approved users default to the `user`
role.

Loopback trust and the machine-local token resolve the primary user's current
role. Demoting that user while another admin exists therefore removes local
administrative authority; keep an intentional admin path before doing so.

**Settings → Agents & profiles** contains the readable, non-secret environment
on the `default` profile. Other profiles keep their write-only environment
beside their launch policy. Do not put personal tokens or deployment
credentials in repository configuration.

An ordinary interactive session uses the launching user's write-only GitHub PAT
from **Settings → Account** when one is set. Otherwise, `git` and `gh` ask Loom
for a short-lived GitHub App installation token, limited to repositories
allowlisted by the selected profile. Loom presents the selected credential
through the image's managed Git and GitHub CLI adapters. See [Restricted GitHub
sessions](restricted-sessions.md#github-credential-policy) for automation's
tighter tool boundary.

Files under a shared session home are deployment resources, not environment
variables. An operator deployment may materialize them from its secret backend,
but every session sharing that home can read them. Per-profile files require
isolated session homes or mounts.

## Registered setting precedence

Loom resolves every registered setting through one explicit precedence chain:

| Precedence | Source | Owned by | How to change it |
|---|---|---|---|
| 1 | Runtime override | admin | Administration settings, `PATCH /api/settings`, or `loom config set` |
| 2 | Deployment default | infrastructure repository | `loom deployment apply` / `POST /api/deployment/reconcile` |
| 3 | Built-in default | Loom release | `weaver-core::config::REGISTRY` |

The runtime and deployment layers are separate database tables. A live edit
therefore does not destroy the deployment's declared value: clearing the
runtime override reveals the deployment default, and removing the deployment
default reveals the built-in. `GET /api/settings` reports the effective
`value`, its `source`, and any `deployment_value`; the Settings UI shows the
same provenance.

This precedence applies only to registered, non-secret global settings. Secrets
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

Profiles may declare `github_repositories` to define the GitHub App broker's
scope. Ordinary interactive sessions select the launching user's Account PAT
first and use this broker when the App path is selected. Interactive profiles
stamp only the session's current allowlisted repository. Strict,
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

The Administration pages and `PATCH /api/settings` use the same validation and storage
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
