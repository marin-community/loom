// What the server declares, the server generates: every DTO the API returns or
// accepts now lives in `api/generated.ts`, which
// `cargo run -p weaver-api --bin generate-types` writes from the OpenAPI document
// `/api/openapi.json` serves. This file used to restate all of them by hand,
// which is exactly how a mirror drifts.
//
// Two things are left here, and nothing else belongs:
//
//  1. **Names.** Rust spells its wire types `SessionView`, `ReviewDto`,
//     `TagView`; the SPA has always spelled them `Session`, `Review`, `Tag`.
//     The re-export block below is that mapping, in one place, and it is the
//     only thing standing between a renamed Rust DTO and a compile error here.
//  2. **Decodes of opaque JSON.** A handful of fields are `serde_json::Value`
//     on the wire — a chat block's `payload`, a watch's `trigger`/`scope`, the
//     ACP adapter's command and configuration metadata — so the generated type
//     for them is `unknown`, correctly. The browser is the only thing that
//     knows their shape, so the shapes stay here. Same for the `sessions.chat`
//     SSE frames, which are not an operation's `Output` at all, and for the
//     error bodies an `ApiError` carries.
//
// Anything you are tempted to add here that an operation already declares
// belongs in the Rust DTO instead — then regenerate.

export type {
  AcpCost,
  AcpUsage,
  AnchorDto as Anchor,
  AgentChoiceView as AgentChoice,
  AgentEnvVarView as EnvVar,
  AgentMetadataView as AgentMetadata,
  ArtifactMeta,
  ArtifactRefs,
  ArtifactVersion,
  ArtifactView,
  AuthMethods,
  Block as IrisBlock,
  BranchSummaryView as BranchSummary,
  BranchView as Branch,
  ChangeAnchorDto as ChangeAnchor,
  ChangeBaseDto as ChangeBase,
  ChangeContentDto as ChangeContent,
  ChangeFileDto as ChangeFile,
  ChangeFileStatusDto as ChangeFileStatus,
  ChangeHunkDto as ChangeHunk,
  ChangeLineDto as ChangeLine,
  ChangeLineKindDto as ChangeLineKind,
  ChangePathDto as ChangePath,
  ChangeSetDto as ChangeSet,
  ChangeSideDto as ChangeSide,
  ChangeSourceDto as ChangeSource,
  ChannelBindingView as ChannelBinding,
  ChannelDeliveryView as ChannelDelivery,
  ChannelMessageView as ChannelMessage,
  ChannelSubscriptionView as ChannelSubscription,
  ChannelView as Channel,
  ChatBlockView as ChatBlock,
  CommentDto as Comment,
  CreatedTokenView as CreatedToken,
  CustomAgentView as CustomAgent,
  CustomMcpView as CustomMcp,
  DiagnosticFederation,
  DiagnosticProblemSummary,
  DiagnosticProfileCapacity,
  DiagnosticRunCount,
  DiagnosticRunFailure,
  DiagnosticSessionCount,
  DiagnosticsStatusOutput as ServerStatus,
  DiagnosticsView as Diagnostics,
  EffectivePermissionsView as EffectivePermissions,
  Event as WeaverEvent,
  GithubConfigView as GithubConfig,
  GithubStatus,
  HistoryPageView as HistoryPage,
  HistoryRecordView as HistoryRecord,
  IssueAction,
  IssueActionsResult,
  IssueRefStatus,
  IssueTagInput,
  IssueView as Issue,
  LaunchCapacityView as LaunchCapacity,
  LaunchOverrides,
  LaunchProvenanceView as LaunchProvenance,
  LaunchSelection,
  Log as IrisLog,
  LogLineView as LogLine,
  McpAccess,
  McpAdapterView as McpAdapter,
  McpCapabilitySetView as McpCapabilitySet,
  McpRegistryView as McpRegistry,
  MeView as Me,
  Message as IrisMessage,
  MigrationStreamView as MigrationStream,
  PermissionRequestView as PermissionRequest,
  ProfileEnvMutationReq as ProfileEnvMutation,
  ProfileEnvView as ProfileEnv,
  ProfileView as Profile,
  ProgramView,
  PromptResult as PromptAck,
  RecentRepoView as RecentRepo,
  RepoBranchView as RepoBranch,
  RepoEnvVarView as RepoEnvVar,
  RepoRevisionValidationView as RepoRevisionValidation,
  RepoView as ManagedRepo,
  ResolvedLaunchPolicyView as ResolvedLaunchPolicy,
  ResolvedLaunchView as ResolvedLaunch,
  ResumptionCueView as ResumptionCue,
  ResumptionEvidenceView as ResumptionEvidence,
  ReviewCommentDto as ReviewComment,
  ReviewDto as Review,
  ReviewSubjectDto as ReviewSubject,
  RunView as AutomationRun,
  ScratchFileView as ScratchFile,
  ScratchLimitsView as ScratchLimits,
  SessionChatView as ChatSnapshot,
  SessionCreatorFilter,
  SessionCustomMcpView as SessionCustomMcp,
  SessionGithubAccessView as SessionGithubAccess,
  SessionGroupOrderReq as SessionGroupOrder,
  SessionGroupView as SessionGroup,
  SessionIdeInfoView as IdeInfo,
  SessionLayoutItemKind,
  SessionLayoutView as SessionLayout,
  SessionMcpPolicyView as SessionMcpPolicy,
  SessionPlacementDefaultView as SessionPlacementDefault,
  SessionPlacementSelectorKind,
  SessionPlacementView as SessionPlacement,
  SessionSearchAttention,
  SessionSearchStatus,
  SessionSpaceView as SessionSpace,
  SessionSummaryView as SessionSummary,
  SessionTransitionView as SessionTransition,
  SessionView as Session,
  SettingKind,
  SettingSource,
  SettingView,
  SettingsEnvelope,
  SlackAccessView as SlackAccess,
  SlackConnectionStatusOutput as SlackStatus,
  SlackIdentityView as SlackIdentity,
  SlackSocketView as SlackSocket,
  TagView as Tag,
  TaskView as TaskRecord,
  ThreadDto as Thread,
  TokenView as Token,
  UserPreferenceView,
  UserPreferencesEnvelope,
  UserRole,
  UserView as User,
  WatchRunResult,
  WatchRunView as WatchRun,
  WatchView as Watch,
} from './api/generated';

// The operations whose input a component builds and hands to `api.ts` whole.
// These are the *request* half of the same generated table, named for the
// caller rather than the route.
export type {
  AgentsCustomCreateInput as CustomAgentInput,
  McpsCustomCreateInput as CustomMcpInput,
  SessionsHandoffInput as HandoffInput,
  WatchesCreateInput as WatchCreateInput,
} from './api/generated';

import type {
  AcpCost,
  ChatBlockView,
  CloneProfileEnvironmentReq,
  ProfilesCloneInput,
  ProfilesUpdateInput,
  WatchesUpdateInput,
} from './api/generated';

/** Every field present, recursively.
 *
 *  An editor's draft is not a request. The server lets a caller omit anything
 *  carrying a serde default, and the generated request type says so — but a
 *  form holds a value for each of those fields and reads them back without
 *  asking. Wrapping the request type states that without restating its fields,
 *  and the draft still satisfies the request on the way out. */
export type Complete<T> = T extends (infer U)[]
  ? Complete<U>[]
  : T extends object
    ? { [K in keyof T]-?: Complete<T[K]> }
    : T;

/** The profile editor's draft. `profiles.update` rather than `.create` because
 *  it is the superset — it also carries the `expected_revision` guard — and the
 *  editor serves both. */
export type ProfileInput = Complete<ProfilesUpdateInput>;
export type CloneProfileEnvironment = Complete<CloneProfileEnvironmentReq>;

/** `profiles.clone` names its source profile with a `source` operand, which
 *  `api.ts` takes as its own argument; a caller's body is everything else. */
export type CloneProfileInput = Omit<ProfilesCloneInput, 'source'>;
/** `watches.update` names the watch it edits with a `key` operand; `api.ts`
 *  supplies that from the id, so an editor's body is everything else. */
export type WatchUpdateInput = Omit<WatchesUpdateInput, 'key'>;

// --- Opaque payloads the server carries but does not declare ---------------
// `ChatBlockView.payload` is `serde_json::Value` on the wire: the journal
// stores whatever the ACP adapter produced, keyed by the block's `kind`. The
// browser is the only reader that knows the shapes, so they live here and a
// component narrows `payload` to one of them by switching on `kind`.

/** The closed set of chat-journal block kinds. The wire type is a bare string —
 *  this is the browser's reading of it, and switching on it is how a
 *  `ChatBlock.payload` gets a type. */
export type ChatBlockKind =
  | 'user_message'
  | 'agent_message'
  | 'thought'
  | 'tool_call'
  | 'plan'
  | 'permission_request'
  | 'mode_change'
  | 'usage'
  | 'turn_end'
  | 'handoff';

export interface UserMessagePayload {
  text: string;
  by: string | null;
  steered?: boolean;
}
export interface AgentMessagePayload {
  text: string;
}
export interface ThoughtPayload {
  text: string;
  ms: number | null;
}
export interface ToolTextContent {
  type: 'text';
  text: string;
}
export interface ToolDiffContent {
  type: 'diff';
  path: string;
  old: string | null;
  new: string;
}
export interface ToolImageContent {
  type: 'image';
  data: string;
  mime_type: string;
  uri: string | null;
}
export type ToolContent = ToolTextContent | ToolDiffContent | ToolImageContent;
export interface ToolLocation {
  path: string;
  line: number | null;
}
export interface ToolCallPayload {
  tool_call_id: string;
  title: string;
  tool_kind: string;
  status: string;
  content: ToolContent[];
  locations: ToolLocation[];
}
export interface PlanEntry {
  content: string;
  status: string;
}
export interface PlanPayload {
  entries: PlanEntry[];
}
export interface PermissionOption {
  option_id: string;
  name: string;
  kind: string;
}
export interface PermissionOutcome {
  option_id?: string;
  cancelled?: boolean;
  by: string;
  at: string;
}
export interface PermissionPayload {
  request_id: string;
  tool_call_id: string | null;
  title: string;
  options: PermissionOption[];
  effective_mode?: string | null;
  outcome: PermissionOutcome | null;
}
export interface UsagePayload {
  used: number | null;
  size: number | null;
  cost?: AcpCost | null;
  reset?: boolean;
}
export interface TurnEndPayload {
  stop_reason: string;
}
export interface HandoffPayload {
  from: string;
  to: string;
  model: string;
  effort: string;
  prompt_version?: number;
  summary_status?: 'generated' | 'unavailable';
  summary_model?: string | null;
  summary?: string | null;
  through_turn?: number | null;
  through_seq?: number | null;
}

// --- ACP adapter metadata --------------------------------------------------
// `AcpMetadataView`'s three lists are `Vec<serde_json::Value>` in Rust — Loom
// passes the adapter's own command/configuration descriptors through without
// interpreting them, so the generated type is `unknown[]`. The composer does
// interpret them, and this is its reading.

export interface AcpCommand {
  name: string;
  description: string;
  input?: { type?: string; hint?: string } | null;
}
export interface AcpConfigChoice {
  value: string;
  name: string;
  description?: string | null;
}
export interface AcpConfigGroup {
  group: string;
  name: string;
  options: AcpConfigChoice[];
}
export interface AcpConfigOption {
  id: string;
  name: string;
  description?: string | null;
  category?: string | null;
  type: string;
  currentValue: string | boolean;
  options?: AcpConfigChoice[] | AcpConfigGroup[];
}
export interface AcpMode {
  id: string;
  name: string;
  description?: string | null;
}
export interface AcpMetadata {
  commands: AcpCommand[];
  config_options: AcpConfigOption[];
  modes: AcpMode[];
  steering_supported: boolean;
}

// --- `sessions.chat.stream` SSE frames -------------------------------------
// A stream operation's declared `Output` is the stream, not the frames on it,
// so these have no generated counterpart. They mirror loom's `acp/mod.rs`.

/** `block` — a whole journaled block (upsert by `(turn, seq)`). Same shape as a
 *  snapshot block; a resolved `permission_request` re-emits its own block. */
export type SseBlock = ChatBlockView;
/** `delta` — a streamed chunk of the in-flight message/thought (append to a
 *  shadow block until the whole block journals). */
export interface SseDelta {
  turn: number;
  kind: 'agent_message' | 'thought';
  text: string;
}
/** `tool` — live tool-call state, before it reaches a terminal status (then a
 *  `tool_call` block supersedes it). */
export interface SseTool {
  turn: number;
  tool_call_id: string;
  title: string;
  tool_kind: string;
  status: string;
  content: ToolContent[];
  locations: ToolLocation[];
}
/** `turn` — the turn drove live (`started`) or ended (`ended` + stop reason). */
export interface SseTurn {
  turn: number;
  state: 'started' | 'ended';
  effective_mode?: string | null;
  stop_reason?: string;
}

/** `queue` — the complete durable next-turn prompt after a queue mutation. */
export interface SseQueue {
  pending_prompt: string | null;
}

// --- Watch configuration ---------------------------------------------------
// A watch's `trigger`, `scope`, `params`, and a run's `actions` are stored as
// JSON and served as `serde_json::Value`, so the generated types are `unknown`.
// The watch editor is what gives them shape.

/** A watch's trigger — its subscription manifest, parsed. A scheduled trigger
 *  carries a `cron` (or `every`) cadence; a reactive one subscribes to one or
 *  more normalized trigger events via `on` (each `"name"` or `"name=level"`).
 *  `event`/`level` are the legacy single-event shape, still honoured. An
 *  optional `repo` pins it to one repository. Mirrors weaver-core's `Trigger`. */
export interface WatchTrigger {
  cron?: string;
  every?: string;
  on?: string[];
  event?: string;
  level?: string;
  repo?: string;
}

/** The fleet slice a round surveys. Both filters are optional; omitting them
 *  surveys everything the watch may see. */
export interface WatchScope {
  attention?: string;
  repo?: string;
}

/** One action a round recorded — a mark, nudge, interrupt, or a stubbed
 *  "would do X" from a dry-run. The shape is loose (the engine writes free-form
 *  JSON); these are the fields the panel renders when present. */
export interface WatchAction {
  /** The session the action targeted, when it targets one. */
  session?: string;
  /** A performed action's verb (e.g. `mark`, `nudge`). */
  action?: string;
  /** A dry-run's stubbed verb — what it *would* have done. */
  would?: string;
  /** The triage level a `mark` stamped. */
  level?: string;
  /** A one-line reason / note. */
  note?: string;
  /** The message body of a nudge. */
  text?: string;
  [key: string]: unknown;
}

// --- Error bodies ----------------------------------------------------------
// No operation declares these: they are the JSON an `ApiError` carries when a
// request fails, which the registry describes only as a 200 `Output`.

/** One invalid ID or precondition in an atomic issue action's error details. */
export interface IssueActionProblem {
  id: number;
  code: string;
  error: string;
}
