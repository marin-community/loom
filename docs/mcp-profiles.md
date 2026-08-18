# MCP and profile control plane

Profiles choose provider-neutral MCP capability groups. Loom resolves that
selection into immutable, content-addressed policy before launch; agent adapters
translate the snapshot into provider-specific runtime configuration only at the
final boundary.

## Ownership

- **The registry describes capability.** Each builtin or custom entry has a
  stable identity, group, version or revision, content digest, exact tool names
  and schemas, and an adapter launch descriptor.
- **A profile selects and pins policy.** `mcp_access` is `none`, `all`, or an
  explicit group list. Saving the profile resolves enabled registry content and
  pins the exact identities, digests, custom revisions, and tool surface to that
  profile revision.
- **A profile owns deployment instructions.** Its optional multiline
  `instructions` are appended to the first prompt for every origin selecting
  that profile. They are visible through the profile API and Settings UI and
  should contain organization workflow and response conventions, never secrets.
- **A session owns launch history.** Launch copies the resolved profile and MCP
  policy into the concrete snapshot stamped for that runtime launch. Recovery,
  adoption, and an unchanged profile do not re-resolve the current registry;
  an explicit handoff can replace the snapshot after resolving again.
- **Adapters own translation.** Claude, Codex, and later runtimes receive their
  native `mcpServers` and permission representation from the same stamped
  provider-neutral policy.

This separates operator intent from executable configuration. Profiles never
store arbitrary MCP commands, and provider-native permission strings do not
name Loom integrations.

## Registry

Builtins are trusted code shipped with Loom. Their content digest covers the
adapter identity, capability metadata, ordered tools, and advertised schemas.
Builtins provide resource-shaped context, channel, artifact, and session
families, plus compatible fixed-repository GitHub, fixed-thread messaging, and
self-history adapters. They call Loom's typed REST client and return
machine-readable `structuredContent`; service credentials and provider routing
remain in the server.

The resource families use consistent verbs:

```text
loom_context.get
loom_channel.list|get|read|send|wait|ack|open|subscribe
loom_artifact.list|get|write|delete|history|threads|comment|resolve
loom_session.get|status|history|search
```

`channel: "self"` and `session: "self"` resolve through `GET /api/self`.
Artifact writes are create-or-append operations and may supply `base_rev` for
optimistic concurrency. Channel sends may supply `idempotency_key` and return
one receipt per runtime or external binding.

Custom definitions are administrator-authored Python MCP servers stored as
immutable sqlite revisions under absolute identities such as
`/engineering/search/docs`; the first segment is the selectable group. Saving a
definition runs real MCP initialization, `tools/list`, and optional tests
through `uv run --script`. Failed or disabled definitions cannot enter a newly
saved profile revision.

Custom source starts with a cleared environment plus Loom-controlled interpreter
and uv-cache paths and session-scoped Loom API context. Runtime discovery and
calls are filtered to the exact stamped tools even if a script later advertises
more. Custom definitions cannot shadow builtin-reserved groups or enter a
restricted profile.

Custom code is dependency-contained operator code, not an operating-system
sandbox. Repository content cannot provide executable MCP configuration.

## Revisions and resolution

Profile, profile-environment, and MCP-policy edits share an optimistic profile
revision. Creating and cloning profiles are insert-only. A retired profile name
keeps a monotonic tombstone generation so a stale preview cannot match a later
profile recreated under the same name.

The launch resolver returns:

- the selected profile and resolver revisions;
- concrete agent, model, effort, protocol, and mode;
- launch policy and provenance;
- exact MCP capability identities and revisions;
- capacity and validation results.

Create must present those revisions. Drift returns a conflict with a fresh
preview rather than silently changing the launch. The resulting
`resolved_launch` and MCP snapshot are the auditable runtime contract.

Registry edits do not widen sessions or saved profile revisions. Saving a
profile again is the explicit reconciliation point. Loom refuses to remove a
server pinned by a profile, or the last server in a group still named by an
explicit profile selection.

## Restricted sessions

Restricted profiles add a tighter trust boundary:

- they are strict, environment-cleared ACP automation profiles;
- they suppress repository-controlled agent configuration and the normal Weaver
  prelude;
- they admit only reviewed builtin capabilities and reject custom MCPs;
- exact expanded permissions are stamped on the session;
- unmatched permission requests are rejected;
- server-side tools fix the repository and external thread from trusted session
  context;
- provider credentials remain in Loom and never enter the agent process.

The stock `github_comment` profile carries policy, not credentials. Loom calls
GitHub through its App client while executing the fixed operation, minting a
short-lived repository-scoped installation token internally.

See [Restricted sessions](restricted-sessions.md) for automation identity,
idempotency, and deployment boundaries.

## Operator surface

```sh
loom mcp ls
loom mcp show mcp/github/comment@v1
loom mcp add /engineering/search/docs --file server.py --tests test_mcp.py
loom profile add ops --agent codex --mcp github,messaging
loom profile show ops --effective
loom profile probe ops
```

Settings exposes the same registry and resolved profile views. API and CLI reads
show environment names, source kinds, references, and update times but never
literal secret values.
