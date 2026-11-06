# Restricted GitHub sessions

Restricted sessions let a trusted GitHub Actions workflow supply a complete,
one-shot prompt without giving the agent Loom's normal issue-solving prelude
or an unrestricted developer environment. Loom supplies the security envelope;
the workflow owns task semantics, stale-write checks, and prose policy.

The stock `github_comment` profile uses Claude over ACP with no Loom prelude,
no repository environment, no Claude user/project/local
settings, repository-scoped read tools, and a fixed GitHub issue/PR MCP tool.
The profile selects that reviewed tool as `loom/github/comment@v1`; Loom expands
the set into exact tool permissions when it stamps the session and launches the
single built-in MCP server from its registry. Profile data cannot provide a
server command. New tool domains belong in `crates/loom-agent/src/mcp/` and must
be registered by Loom before a profile can select one.
The MCP bridge calls a session-scoped Loom endpoint; Loom calls GitHub through
its App client against the session's fixed repository and linked issue/PR
number. The agent receives the fixed GitHub tool rather than general shell
access to GitHub, and Loom enforces the configured Claude permission rules. Loom
uses the configured GitHub App's short-lived installation token for the fixed
repository. The stock policy lives in
`crates/loom-policy/profiles/github_comment/profile.json`, not in a schema migration. Loom
seeds a missing stock profile through normal validation and does not overwrite
later operator edits. Custom profiles use the same REST/CLI/UI or
deployment-reconciliation contract; loading policy implicitly from a managed
checkout would let repository content choose its own launch boundary and is
deliberately unsupported. Custom profiles may compose the built-in capability
sets Loom recognizes, but cannot define executable MCP adapters from repository
content. Operator-authored custom MCPs are available to ordinary profiles only;
their groups cannot shadow trusted builtin groups, and restricted profiles
require explicit builtin groups rather than future-widening `all`, even though
both use the same provider-neutral `mcp_access` contract.

## GitHub credential policy

| Use case | Credential path |
| --- | --- |
| Ordinary interactive session | Launching user's Loom-stored Account PAT, then the App broker scoped to the profile-approved repositories |
| Restricted GitHub tool | Loom's App-backed REST call for the session's fixed repository |
| GitHub Actions calling Loom | GitHub OIDC exchanged for a ten-minute Loom automation token |

For ordinary sessions, Loom explicitly selects the user's Account PAT or the
App broker and stamps that choice for the image's `git` helper and `gh` wrapper.
Restricted sessions use neither adapter: their fixed GitHub tools call Loom,
which uses the App internally.

## GitHub Actions request

The caller job needs `id-token: write`. A composite action runs under the
calling workflow's OIDC identity, so Loom's federation mapping must name the
caller's numeric repository id and exact workflow ref—not the composite action's
repository.

After constructing the full prompt in `prompt_file`, the job exchanges its OIDC
token and submits the run:

```bash
set -euo pipefail

audience=$(jq -rn --arg value "$LOOM_URL" '$value|@uri')
oidc_token=$(curl --fail-with-body --silent --show-error \
  -H "Authorization: Bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=$audience" | jq -r .value)
echo "::add-mask::$oidc_token"
loom_token=$(printf '%s' "$oidc_token" |
  WEAVER_API="$LOOM_URL" loom auth federate | jq -er .token)
echo "::add-mask::$loom_token"

session=$(jq -cn \
  --arg repo "$GITHUB_REPOSITORY" \
  --arg title "Prose cleanup for #$NUMBER" \
  --rawfile goal "$prompt_file" \
  --argjson number "$NUMBER" \
  '{
    repo: $repo,
    title: $title,
    goal: $goal,
    github_issue: $number
  }')
WEAVER_API="$LOOM_URL" LOOM_TOKEN="$loom_token" loom runs create \
  --profile github_comment \
  --idempotency-key "prose-cleanup:$KIND:$NUMBER:$BODY_HASH" \
  --session "$session"
```

`loom auth federate` accepts an OIDC-token file as its optional positional
argument; `-` or no argument reads stdin. The command prints the same JSON
credential document as the API. Capture `.token`, mask the short-lived token
immediately, and pass it to `loom runs create` through `LOOM_TOKEN` rather than
a command argument. The only curl above is GitHub's runner-specific OIDC-token
acquisition; both Loom requests use the supported CLI.

GitHub caller keys accept up to 128 ASCII letters, digits, `.`, `_`, `:`, and
`-`. Loom namespaces them by verified repository and subject. An empty key keeps
the compatibility behavior of deduplicating one workflow run attempt; a body
hash key converges retries and reruns for the same source description.

Automation callers may also set `channel` to route distinct idempotency keys
through one live ACP session. The channel is scoped to the verified subject,
service tag, profile, and source. A busy session queues the update through its
durable ACP prompt path; a provisioning or orphaned channel returns a retryable
error instead of acknowledging an undelivered update. Channel names accept up
to 64 ASCII letters, digits, `.`, `_`, `:`, and `-`.

Automation requests carry the task prompt and are stored for audit and
idempotency. Loom resolves the short-lived, repository-scoped App credential
while executing a fixed GitHub tool.

## Shared grant authority

Loom can accept a shared authority's Ed25519 access JWT directly for
`runs.create`. This is a separate authentication mode from browser login and
Loom's own automation tokens. Configure the `auth.shared_grants` setting as a
JSON document with these fields:

| Field | Value |
| --- | --- |
| `issuer` | Exact trusted JWT issuer |
| `public_keys` | Array of trusted Ed25519 public keys in PEM form |
| `authorize_url` | Authority endpoint for the live operation decision |
| `credentials_url` | Authority endpoint for the same grant's runtime credentials |
| `ca_file` | Optional PEM CA bundle for a private authority; omission uses the client's public trust roots |
| `profiles` | Map of reviewed profile names to `{cwd, revision, resolver_revision}` |

Every configured profile must be strict, restricted, and environment-cleared.
Obtain the profile revision and resolver fingerprint from
`sessions.launches.resolve`; changing either requires reviewing and updating
the trust policy. The configured `cwd` is an absolute server-side repository
root. Authority endpoints require HTTPS, except for isolated loopback tests.
Requests never follow redirects with a bearer credential.
Enabling this setting permanently disables implicit loopback administration
and local machine tokens for this database, including deployment reconciliation.
Clearing or pruning the setting does not restore that authority. Provision an
operator PAT before enabling shared mode; server-issued engine credentials and
ordinary browser sign-in remain available.

The JWT must have protected header `typ=at+jwt`, algorithm `EdDSA`, exact
audience `loom`, a matching subject and acting principal, and a grant ID,
owner, policy revision, expiry, and service permissions. Launching profile
`review` requires action `session.create` beneath `/review/`. Loom checks the
signature and token bounds, then posts the original bearer to `authorize_url`
with `{"audience":"loom","action":"session.create","resource":"/review/"}`.
An unavailable authority or a revoked grant denies the launch.

The Marin authority uses OpenFGA for current standing permissions and transitive
group membership, and its grant ledger for scope, delegation, expiry, and
revocation. Loom does not load OpenFGA models or evaluate group membership
locally. Removing standing access can therefore deny a previously issued JWT;
the live decision is required even when its signature and expiry are valid.
The authority's model, store and credentials are operator configuration, never
fields selected by the launch caller. OpenBao supplies identity and signing.

The caller supplies the profile, title, and goal. It cannot
choose a repository path, launch overrides, scratch uploads, a channel, or a
Slack route. Loom selects the configured repository and pinned profile. A
shared grant reaches no other API operation, including profile changes and
automation-token minting. Run metadata records `shared_grant_id`; idempotency
keys are fixed by that grant, so one grant owns exactly one run. Retries with
different task content are rejected.

Loom posts `{"resource":"/review/"}` and the same bearer to
`credentials_url`. The broker returns the matching grant identity,
`auth_url`, and an optional `storage_token`. These credentials enter only the
new session's runtime environment as `MARIN_AUTH_URL` and
`MARIN_GRANT_TOKEN`, after the ordinary environment clearing. They never enter
the stored launch request or profile environment. The broker must derive
storage authority from the same grant and recheck live state before issuing it.

Runtime credentials remain in memory. A stopped shared-grant session needs a
fresh authorized launch; wake and recovery refuse to respawn it without its
credential context. The authority's lifecycle observer revokes the recorded
grant when the actual run ends, which stops future credential refresh.

Restricted Loom currently has no built-in CoreWeave storage tool. Injecting a
storage grant establishes runtime identity but does not add a shell or an
unreviewed MCP to the restricted tool set.

## GitHub deployment checklist

1. Configure and install the production GitHub App on each target repository.
2. Add one `githubFederations` entry per approved caller workflow, constrained
   to the numeric repository id, exact workflow ref, event/ref where useful, and
   only `github_comment`.
3. Deploy the merged Loom image and reconcile the manifest. Audit with `loom
   profile show github_comment` and `loom federation ls`.
4. Run a synthetic issue through the direct API. Verify the prompt appears as
   the first turn without `WEAVER.md`, a duplicate body-hash key returns the
   original run, the fixed tool is visible, and only the requested
   GitHub mutation occurs.
5. Move callers to the OIDC exchange without changing their idempotency keys or
   stale-write preconditions, then roll the workflow revision out in controlled
   batches.

Disable the federation mapping to stop new runs. Removing the App installation
from a repository disables fixed GitHub operations for that repository without
changing the session policy.
