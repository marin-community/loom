# Restricted GitHub sessions

Restricted sessions let a trusted GitHub Actions workflow supply a complete,
one-shot prompt without giving the agent Weaver's normal issue-solving prelude
or an unrestricted developer environment. Loom supplies the security envelope;
the workflow owns task semantics, stale-write checks, and prose policy.

The stock `github_comment` profile uses Claude over ACP with no Weaver prelude,
no repository environment or setup script, no Claude user/project/local
settings, repository-scoped read tools, and a fixed GitHub issue/PR MCP surface.
The profile selects that reviewed surface as `mcp/github/comment`; Loom expands
the set into exact tool permissions when it stamps the session and launches the
corresponding built-in adapter from its registry. Profile data cannot provide an
adapter command. New adapter families belong in `crates/loom/src/mcp/` and must
be registered by Loom before a profile can select one.
The MCP bridge calls a session-scoped Loom endpoint; Loom runs `gh` server-side
against the session's fixed repository and linked issue/PR number, so neither a
general shell nor the GitHub token enters the agent process. Anything outside
the configured Claude permission rules is rejected by Loom.
The profile intentionally has no token in its seed. Loom uses the configured
GitHub App's short-lived installation token for the fixed repository. If an App
is configured but is not installed on that repository, the operation fails
rather than falling back to a broader credential. An App-less deployment can
configure a least-privilege CI identity as the profile's write-only `GH_TOKEN`.
Personal user tokens remain exclusive to ordinary interactive sessions. The
stock policy lives in
`crates/loom/profiles/github_comment.json`, not in a schema migration. Loom
seeds a missing stock profile through normal validation and does not overwrite
later operator edits. Custom profiles use the same REST/CLI/UI or
deployment-reconciliation contract; loading policy implicitly from a managed
checkout would let repository content choose its own launch boundary and is
deliberately unsupported. Custom profiles may compose the built-in capability
sets Loom recognizes, but cannot define executable MCP adapters from repository
content.

## GitHub credential policy

| Use case | Primary credential | Fallback |
| --- | --- | --- |
| Ordinary interactive session | Launching user's personal token from **Settings → Account** | Selected profile's `GH_TOKEN`, then lower session environment layers |
| Restricted GitHub tool | Short-lived GitHub App installation token for the session's fixed repository | Profile `GH_TOKEN` only when no App is configured |
| GitHub Actions calling Loom | GitHub OIDC exchanged for a ten-minute Loom automation token | None |

The same environment variable name appears inside an ordinary session because
`git` and `gh` expect it, but the stores and trust boundaries are separate.
When a GitHub App is configured, a mint or installation failure is an error:
Loom does not silently widen authority by falling back to a personal or shared
PAT.

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
  -d "$request" "$LOOM_URL/api/runs"
```

GitHub caller keys accept up to 128 ASCII letters, digits, `.`, `_`, `:`, and
`-`. Loom namespaces them by verified repository and subject. An empty key keeps
the compatibility behavior of deduplicating one workflow run attempt; a body
hash key converges retries and reruns for the same source description.

Do not put `GH_TOKEN` in the request or prompt. Automation requests are stored
for audit and idempotency. Loom resolves a credential only while executing a
fixed GitHub tool. Prefer the configured GitHub App, whose installation token is
short-lived and scoped to the session's fixed repository. App-less deployments
can put a least-privilege token in the profile's write-only environment,
preferably as a deployment-managed Secret Manager reference. The existing
ordinary interactive launch path uses the approved requester's stored GitHub
token by default; restricted sessions do not.

## Production rollout

1. Configure and install the production GitHub App on each target repository.
   For an App-less deployment, declare `github_comment` in the Pulumi profile
   manifest with a least-privilege `GH_TOKEN` secret reference and grant the
   Loom VM access to that secret.
2. Add one `githubFederations` entry per approved caller workflow, constrained
   to the numeric repository id, exact workflow ref, event/ref where useful, and
   only `github_comment`.
3. Deploy the merged Loom image and reconcile the manifest. Audit with `loom
   profile show github_comment` and `loom federation ls`.
4. Run a synthetic issue through the direct API. Verify the prompt appears as
   the first turn without `WEAVER.md`, a duplicate body-hash key returns the
   original run, no shell or code-writing tool is visible, the agent environment
   contains no GitHub token, and only the requested GitHub mutation occurs.
5. Change the Marin prose-cleanup action from posting `@weaverbot` to the OIDC
   exchange above, keeping its current body-hash/stale-write prompt. Roll the
   action revision through `marin-community/*` consumers in batches.

Disable the federation mapping to stop new runs. On an App-less deployment,
removing the profile token also disables the fixed GitHub operations. Neither
rollback affects interactive Loom sessions.
