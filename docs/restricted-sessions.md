# Restricted GitHub sessions

Restricted sessions let a trusted GitHub Actions workflow supply a complete,
one-shot prompt without giving the agent Loom's normal issue-solving prelude
or an unrestricted developer environment. Loom supplies the security envelope;
the workflow owns task semantics, stale-write checks, and prose policy.

The stock `github_comment` profile uses Claude over ACP with no Loom prelude,
no repository environment or setup script, no Claude user/project/local
settings, repository-scoped read tools, and a fixed GitHub issue/PR MCP surface.
The profile selects that reviewed surface as `mcp/github/comment@v1`; Loom expands
the set into exact tool permissions when it stamps the session and launches the
corresponding built-in adapter from its registry. Profile data cannot provide an
adapter command. New adapter families belong in `crates/loom-agent/src/mcp/` and must
be registered by Loom before a profile can select one.
The MCP bridge calls a session-scoped Loom endpoint; Loom calls GitHub through
its App client against the session's fixed repository and linked issue/PR
number. The agent receives the fixed GitHub tool rather than a general GitHub
shell surface, and Loom enforces the configured Claude permission rules. Loom
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
loom_token=$(curl --fail-with-body --silent --show-error \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg token "$oidc_token" '{token:$token}')" \
  "$LOOM_URL/api/auth/federate" | jq -r .token)

request=$(jq -n \
  --arg repo "$GITHUB_REPOSITORY" \
  --arg title "Prose cleanup for #$NUMBER" \
  --arg key "prose-cleanup:$KIND:$NUMBER:$BODY_HASH" \
  --rawfile goal "$prompt_file" \
  --argjson number "$NUMBER" \
  '{
    profile: "github_comment",
    idempotency_key: $key,
    source: "actions",
    session: {
      repo: $repo,
      title: $title,
      goal: $goal,
      github_issue: $number
    }
  }')
curl --fail-with-body --silent --show-error \
  -H "Authorization: Bearer $loom_token" \
  -H 'Content-Type: application/json' \
  -d "$request" "$LOOM_URL/api/runs/create"
```

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

## Deployment checklist

1. Configure and install the production GitHub App on each target repository.
2. Add one `githubFederations` entry per approved caller workflow, constrained
   to the numeric repository id, exact workflow ref, event/ref where useful, and
   only `github_comment`.
3. Deploy the merged Loom image and reconcile the manifest. Audit with `loom
   profile show github_comment` and `loom federation ls`.
4. Run a synthetic issue through the direct API. Verify the prompt appears as
   the first turn without `WEAVER.md`, a duplicate body-hash key returns the
   original run, the fixed tool surface is visible, and only the requested
   GitHub mutation occurs.
5. Move callers to the OIDC exchange without changing their idempotency keys or
   stale-write preconditions, then roll the workflow revision out in controlled
   batches.

Disable the federation mapping to stop new runs. Removing the App installation
from a repository disables fixed GitHub operations for that repository without
changing the session policy.
