# Configuration policy

## Configuration ownership

Choose where to configure a value based on who owns it and how widely it
should apply:

| Owner | Configure in | Use for |
|---|---|---|
| User | **Settings → Account** and **Preferences** | Personal sign-in, password, API tokens, optional interactive-session GitHub PAT, and terminal appearance |
| Session profile | **Settings → Agents & profiles** or a deployment manifest | Agent/model policy, instructions, GitHub repository allowlists, shared environment, and write-only session secrets |
| Deployment | The **Administration** settings; deployment IaC for the production source | Approved users and roles, GitHub organization admission, the Loom GitHub App, Slack App, federations, runtime policy, and machine-wide credentials or files |
| Repository | `.weaver/config.toml`, `WEAVER.md`, and `AGENTS.md` | Non-secret repository setup, environment, and workflow instructions |

The Administration section is visible only to admins. Users can launch and
operate sessions, repositories, reviews, per-session shells, and shared layout;
inspect watch activity, redacted server logs, and diagnostics; and manage their
own account and preferences. Admins also manage deployment-wide policy,
integrations, profiles, shared environment, watch definitions and runs, the raw
operator log, the host scratch shell, and access. Existing users become admins
when the role migration is applied; newly approved users default to the `user`
role.

In single-user mode, loopback trust and the machine-local token resolve the
primary user's current role. Demoting that user while another admin exists
therefore removes local administrative authority. Organization authorization
disables both implicit paths, so shared deployments require a manual user or a
current organization authorization for every request.

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

## GitHub organization admission

`auth.github_organizations` accepts space- or comma-separated
`login:numeric-id` pairs. For example, `acme:12345` binds the readable login to
GitHub's immutable organization id. When a GitHub identity signs in, Loom
requires an active membership in one configured organization and grants the
`user` role for one hour. The default is empty, which keeps authorization
manual.

The GitHub App must be installed on the organization with its **Members**
organization permission set to **Read-only**. The callback checks membership
with the user's OAuth token. Before the one-hour authorization expires, the
server resolves the user's current login from their immutable numeric id and
checks every configured organization with a short-lived GitHub App installation
token. An active result from any organization wins. It also revalidates an
expired authorization when that identity sends `@loom`.

Only an active membership renews authorization. A missing permission, timeout,
GitHub outage, malformed response, pending membership, or absent membership
fails closed. Loom immediately invalidates that user's browser, personal, and
session credentials and closes their active sessions. The periodic check tries
again while the authorization remains derived.

Loom retains the user and GitHub identity rows after authorization expires for
audit history and later re-admission; retained identity is not access. An admin
can select **Approve manually** to replace the renewable organization source
with a durable manual grant. Clearing the setting prevents new sign-ins and
causes existing derived grants to fail their next revalidation. The first
nonempty organization configuration permanently latches the database into
shared-deployment mode: clearing settings, removing users, or completing
workloads never restores implicit loopback or machine-token administration.
Returning the database to single-user trust requires a separate, deliberate
recovery procedure; Loom does not expose one automatically. Removing a user
synchronously closes sessions they own.

Manual GitHub approvals require both the current login and the account's
immutable numeric user id. Existing login-only rows retain password access, but
GitHub sign-in remains disabled until an administrator binds the verified id in
**People & security**. The setup wizard records the bootstrap operator's id as
`LOOM_OWNER_GITHUB_ID`.

## Registered setting precedence

Loom resolves every registered setting through one explicit precedence chain:

| Precedence | Source | Owned by | How to change it |
|---|---|---|---|
| 1 | Runtime override | admin | Administration settings, `settings.patch`, or `loom settings patch` |
| 2 | Deployment default | infrastructure repository | `loom deployment apply` / `POST /api/deployment/reconcile` |
| 3 | Built-in default | Loom release | `weaver-core::config::REGISTRY` |

The runtime and deployment layers are separate database tables. A live edit
therefore does not destroy the deployment's declared value: clearing the
runtime override reveals the deployment default, and removing the deployment
default reveals the built-in. `settings.get` reports the effective
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
  auth.github_organizations: "acme:12345"
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
first and use this broker when the App path is selected. An interactive session
stamps only its own current repository, which it gets whether or not the
profile lists it — the allowlist governs expansion beyond that repository.
Strict, environment-cleared automation profiles retain the complete list for
reviewed cross-repository workflows, so their entries must use one owner.
Tokens grant write access to repository contents, issues, pull requests,
Actions, and workflow files. The configured GitHub App must have those
permissions and be installed on every listed repository.

An entry may also be an `owner/*` pattern. A pattern scopes no token by itself;
it declares that this session may expand into that owner without a human
decision, so `loom permissions request github-repository owner/repo` is applied
on the spot rather than raising attention and waiting. Each expansion is still
validated against the App installation, recorded as an audited grant, and
revocable with `loom permissions revoke github-repository`. Use it for an
organization whose repositories the profile's sessions are already trusted
with; leave it off when every expansion deserves a person's eyes.

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

Use Administration → Settings or `loom settings patch` to set
`slack.status_updates` to `false`.

The Administration pages and `settings.patch` use the same validation and storage
path. Send `null` to clear an override and inherit again:

```json
{"slack.status_updates": null}
```

`loom settings list` prints the effective value and source for diagnostics.

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
