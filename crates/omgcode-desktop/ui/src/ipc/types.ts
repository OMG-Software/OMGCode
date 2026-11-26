/** Wire types shared with the Rust core. Field names are snake_case on purpose. */

export interface IpcError {
  code: string;
  message: string;
}

export type SessionStatus = "idle" | "running" | "turn_open" | "busy" | "invalid";

export interface SessionSummary {
  session_id: string;
  title: string;
  created_at: string | null;
  updated_at: string | null;
  status: SessionStatus;
  archived: boolean;
  event_count: number;
  invalid_code: string | null;
}

export interface WorkspaceGroup {
  group_id: string;
  folder_display: string;
  sessions: SessionSummary[];
}

export interface SessionTree {
  groups: WorkspaceGroup[];
  indexed_sessions: number;
  skipped_entries: number;
}

export type TreeView = "active" | "archived";
export type TreeSort = "updated" | "created" | "title";

export interface SnippetPart {
  text: string;
  matched: boolean;
}

export interface SearchHit {
  session: SessionSummary;
  folder_display: string;
  seq: number | null;
  role: "user" | "assistant" | "tool" | "summary" | "title";
  snippet: SnippetPart[];
}

export interface SearchResult {
  hits: SearchHit[];
  truncated: boolean;
}

interface EntryBase {
  seq: number;
  timestamp: string;
}

export type TranscriptEntry = EntryBase &
  (
    | {
        kind: "message";
        role: "user" | "assistant" | "tool";
        source: string;
        text: string;
        call_id: string | null;
      }
    | { kind: "turn_started"; request_id: string }
    | { kind: "turn_completed"; request_id: string }
    | { kind: "turn_failed"; request_id: string; code: string | null }
    | {
        kind: "tool_requested";
        request_id: string;
        call_id: string;
        tool: string;
        arguments: string;
      }
    | {
        kind: "tool_completed";
        request_id: string;
        call_id: string;
        outcome: "succeeded" | "failed" | "denied";
        code: string | null;
        output_bytes: number;
      }
    | {
        kind: "request_manifest";
        request_id: string;
        model: string;
        prompt_version: string;
        tools: string[];
      }
    | { kind: "context_compacted"; summary: string; compacted_messages: number; model: string }
    | { kind: "session_archived" }
    | { kind: "session_restored" }
  );

export interface TranscriptPage {
  session: SessionSummary;
  folder_display: string;
  entries: TranscriptEntry[];
  next_after_seq: number | null;
  incomplete_tail: boolean;
}

export interface ChatSettings {
  endpoint: string;
  model: string;
  context_tokens: number;
  max_output_tokens: number;
  max_tool_calls: number;
  turn_timeout_seconds: number;
  /** The global custom instructions; Rust adds the session workspace's own (ADR-0080). */
  instructions: string;
  compact_threshold: number;
  auto_compact: boolean;
}

export interface ContextMeter {
  estimated_tokens: number;
  trigger_tokens: number;
  context_tokens: number;
  compacted: boolean;
}

export interface ChatOutcome {
  finish_reason: string;
  tools_mode: "enabled" | "disabled";
  tools_disabled_reason: string | null;
  rounds: number;
  tool_calls: number;
  withheld_tool_results: number;
  context: ContextMeter | null;
}

export interface ChatStatus {
  session_id: string;
  state: "idle" | "running" | "cancelling" | "completed" | "failed";
  outcome: ChatOutcome | null;
  error: IpcError | null;
}

/** How a workspace preference is treated (ADR-0063). */
export type Access = "deny" | "prompt" | "allow";

/** Per-tool limits that can only narrow the workspace `write` preference. */
export interface WriteToolAccess {
  edit: Access;
  multi_edit: Access;
  create: Access;
  rename: Access;
  delete: Access;
  create_directory: Access;
}

export interface WorkspaceChoice {
  workspace_id: string;
  folder_display: string;
  model_tools: boolean;
  read: Access;
  write: Access;
  execute: Access;
}

export interface WorkspaceList {
  development_root_set: boolean;
  workspaces: WorkspaceChoice[];
}

export interface AppInfo {
  version: string;
  platform: "windows" | "linux" | "other";
  model_tools_supported: boolean;
}

export type ApprovalState = "pending" | "approved" | "denied" | "consumed" | "expired";
export type ApprovalOperation = "command" | "file_read" | "file_write" | "git_push" | "network";

export interface ApprovalView {
  id: string;
  session_id: string;
  session_title: string | null;
  folder_display: string | null;
  operation: ApprovalOperation;
  summary: string;
  state: ApprovalState;
  created_unix: number;
  expires_unix: number;
  request_sha256: string;
  writers_stopped: boolean;
}

export interface ApprovalList {
  now_unix: number;
  approvals: ApprovalView[];
}

export interface ApprovalField {
  label: string;
  value: string;
  multiline: boolean;
}

export interface ApprovalDetail {
  approval: ApprovalView;
  binding: "exact" | "stale" | "not_found";
  tool: string | null;
  fields: ApprovalField[];
  review_token: string | null;
  /** Whether a turn waits on this approval now; approving then runs the call in that turn. */
  waiting: boolean;
}

/**
 * Event pushed through the chat subscription channel. The text is untrusted model output and is
 * provisional: it is not the recorded reply and may be discarded.
 */
export type ChatEvent =
  | { kind: "request_started"; session_id: string; request: number }
  | { kind: "delta"; session_id: string; request: number; text: string }
  | { kind: "preview_truncated"; session_id: string; request: number }
  | { kind: "finished"; session_id: string };

function isCount(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** Checks the shape of a pushed value. Callers ignore anything that fails. */
export function isChatEvent(value: unknown): value is ChatEvent {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const record = value as Record<string, unknown>;
  if (typeof record.session_id !== "string") {
    return false;
  }
  switch (record.kind) {
    case "finished":
      return true;
    case "request_started":
    case "preview_truncated":
      return isCount(record.request);
    case "delta":
      return isCount(record.request) && typeof record.text === "string";
    default:
      return false;
  }
}

/**
 * The chat defaults the composer saves (ADR-0080). The budgets and the global instructions are
 * saved by the settings window and keep their values.
 */
export interface ComposerPreferences {
  compact_threshold: number;
  auto_compact: boolean;
  /** Enter sends and Shift+Enter adds a newline; otherwise Ctrl+Enter sends (ADR-0062). */
  enter_to_send: boolean;
}

/** The chat defaults the settings window saves beside the model connection (ADR-0080). */
export interface ChatBudgets {
  context_tokens: number;
  max_output_tokens: number;
  max_tool_calls: number;
  turn_timeout_seconds: number;
  /** Global custom instructions for every workspace. */
  instructions: string;
}

export type PreferencesSource = "defaults" | "saved" | "unreadable";

export interface ChatDefaults {
  settings: ChatSettings;
  tool_call_limit_max: number;
  turn_timeout_seconds_max: number;
  preferences: PreferencesSource;
  /** A composer keyboard choice, not a turn setting (ADR-0062). */
  enter_to_send: boolean;
  /** The range the composer's compaction slider offers (ADR-0080). */
  compact_threshold_min: number;
  compact_threshold_max: number;
  /** The slider's step from `compact_threshold_min` (ADR-0091). */
  compact_threshold_step: number;
}

/** One model the next turn's connection reports (ADR-0066). */
export interface ModelChoice {
  /** The exact id sent as `model`. Untrusted provider text. */
  id: string;
  /** The id carries Ollama's cloud tag: a naming convention, not proof either way. */
  cloud_tag: boolean;
}

/** The models of a session's connection, sorted by id (ADR-0071). */
export interface ChatModels {
  /** The session that asked. An answer for a session that is no longer shown is discarded. */
  session_id: string;
  /** The endpoint that was asked, for display only. Never compared with another endpoint. */
  endpoint: string;
  /** The provider kind of the profile that was asked, or `null` for the launch or default endpoint. */
  kind: string | null;
  models: ModelChoice[];
}

/** Skills enabled for a session's workspace. Names only. */
export interface SessionSkills {
  workspace_registered: boolean;
  skills: string[];
}

export interface WorkspaceSettingsView {
  workspace_id: string;
  folder_display: string;
  model_tools: boolean;
  read: Access;
  write: Access;
  execute: Access;
  task_network: Access;
  host_services: Access;
  /** The model `web_fetch` tool: one HTTPS GET of a public page. */
  web_fetch: Access;
  /**
   * The workspace's allowed web hosts, canonical pattern text in the order added: an exact name
   * or `*.` and a name for its subdomains. Configuration only; it grants nothing while
   * `web_fetch` is `deny`.
   */
  web_fetch_hosts: string[];
  write_tools: WriteToolAccess;
  package_cache_root: string | null;
  /** The workspace's custom instructions (ADR-0080); `null` when the saved file is unreadable. */
  instructions: string | null;
  /**
   * Largest text this workspace's save accepts: 16 KiB less the global text and headings;
   * `null` when the saved global text cannot be read now, so the room is unknown.
   */
  instructions_max_bytes: number | null;
}

/** The complete preference set a permissions change sends. Every field is required. */
export interface PermissionFlags {
  model_tools: boolean;
  read: Access;
  write: Access;
  execute: Access;
  task_network: Access;
  host_services: Access;
  web_fetch: Access;
  write_tools: WriteToolAccess;
}

export type ConnectionSource = "environment" | "profile" | "active" | "injected";

export interface ConnectionView {
  /** The endpoint the next chat turn uses. */
  endpoint: string;
  source: ConnectionSource;
  /** The profile in use, or `null` for the launch or default endpoint. */
  profile: string | null;
  /** That profile's provider kind, as the CLI names it. */
  kind: string | null;
  /** Whether this window may choose the active connection (no launch argument fixed it). */
  selectable: boolean;
}

/** One connection the main window's composer may offer (ADR-0071). Never a credential. */
export interface ConnectionChoice {
  name: string;
  /** The provider kind's stored spelling, such as `lm_studio` or `deepseek`. */
  kind: string;
  endpoint: string;
  active: boolean;
}

/** Where a session's connection comes from (ADR-0071). */
export type ConnectionOrigin = "launch" | "session" | "default";

/**
 * A session's model connection (ADR-0064, ADR-0071). `choices` is empty when a launch argument
 * fixed the connection.
 */
export interface ConnectionStatus {
  connection: ConnectionView;
  origin: ConnectionOrigin;
  /** The model chosen for the session, or the saved chat preference's model when it has none. */
  model: string;
  /** The recorded choice no longer holds; the next turn is refused until the operator chooses again. */
  stale: boolean;
  /** The per-session choices file could not be read and is treated as empty. */
  session_choices_unreadable: boolean;
  choices: ConnectionChoice[];
}

export type BranchState = "branch" | "detached" | "none" | "unavailable";

/** A workspace's Git branch, read from `HEAD` for display only. */
export interface BranchView {
  state: BranchState;
  /** The branch name, or the abbreviated commit when detached. */
  name: string | null;
  /** A fixed reason when `state` is `unavailable`. */
  note: string | null;
}

/** What the status bar shows about a session's workspace. */
export interface SessionContext {
  workspace_registered: boolean;
  branch: BranchView;
}

/** The parts of a context estimate, in display order (ADR-0087). */
export type ContextPartId =
  | "system"
  | "global_instructions"
  | "workspace_instructions"
  | "tools"
  | "summary"
  | "history"
  | "skills"
  | "framing";

/** One part of the estimate. It holds numbers and a fixed code, never instruction or message text. */
export interface ContextPart {
  id: ContextPartId;
  /** Conservative token estimate; zero when the part is not included. */
  tokens: number;
  /** Whether the next turn would send this part. */
  included: boolean;
  /** For `history`: the retained messages after the compaction boundary. */
  messages?: number;
  /** A fixed code that says why the part is not included. */
  reason?: string;
}

/** What the open session's next turn would send, by part. Nothing was sent or recorded. */
export interface ContextBreakdown {
  session_id: string;
  parts: ContextPart[];
  /** The sum of the parts. */
  estimated_input_tokens: number;
  max_output_tokens: number;
  /** The estimate at which compaction triggers. */
  effective_trigger: number;
  /** The saved context capacity, or `null` when none is saved. */
  context_tokens: number | null;
  auto_compaction: boolean;
  estimate_scope: string;
}

export type ProcessState = "running" | "finished" | "unfinished" | "refused";

export type ProcessResultKind =
  | "exited"
  | "signalled"
  | "deadline_exceeded"
  | "cancelled"
  | "revoked"
  | "not_started"
  | "supervision_failed"
  | "cleanup_unconfirmed"
  | "limit_exceeded";

export type ProcessLimit = "memory" | "processes" | "cpu_time" | "disk";

/** How a recorded run ended. */
export interface ProcessResult {
  kind: ProcessResultKind;
  exit_code: number | null;
  signal: number | null;
}

/**
 * One command-tool call of a session (ADR-0086). Every text field is untrusted model data:
 * render it as text only. No output, environment value, or credential is ever returned.
 */
export interface ProcessEntry {
  call_id: string;
  state: ProcessState;
  /** RFC 3339. */
  started_at: string;
  finished_at: string | null;
  duration_ms: number | null;
  program: string | null;
  /** Escaped to one line each and bounded; `null` when unknown. */
  arguments: string[] | null;
  /** Whether the program or arguments were shortened or dropped to a bound. */
  text_truncated: boolean;
  argument_count: number | null;
  profile: string | null;
  result: ProcessResult | null;
  limit: ProcessLimit | null;
  /** The refusal reason, or the failure code of a finished call with no run result. */
  reason: string | null;
}

/** The open session's command-tool calls, newest first, at most 100. */
export interface SessionProcesses {
  entries: ProcessEntry[];
  /**
   * Whether older calls may exist that are not listed: more than 100 matched, or the record
   * window was full, so older records were not read.
   */
  truncated: boolean;
  /** Whether the run journal could be joined. */
  run_details: "available" | "unavailable" | "busy";
}

export type WebAuditOutcome = "succeeded" | "failed" | "uncertain" | "unfinished" | "refused";

/** One target of a web audit entry: validated text, or a withheld value known only by length. */
export type WebAuditTarget = { kind: "text"; text: string } | { kind: "withheld"; bytes: number };

/**
 * One model call listed by the web audit (ADR-0098): a `web_fetch` call, or an action made after
 * the session's first successful fetch. Every text field is untrusted: render it as text only.
 */
export interface WebAuditEntry {
  call_id: string;
  /** RFC 3339. */
  time: string;
  /** The tool's wire name: `network` for a fetch, otherwise for example `read_file`. */
  tool: string;
  is_fetch: boolean;
  after_web_content: boolean;
  targets: WebAuditTarget[];
  outcome: WebAuditOutcome;
  /** The refusal reason, or the finish code of an executed call. */
  reason: string | null;
  /** `null` for a refusal. */
  authorization: "standing_allow" | "approval" | null;
  /** A command call's program and arguments, from the session journal; escaped and bounded. */
  program: string | null;
  arguments: string[] | null;
  argument_count: number | null;
  /** Whether any text was shortened or dropped to a bound. */
  text_truncated: boolean;
}

/** The open session's web fetches and later actions, newest first, at most 100. */
export interface SessionWebAudit {
  entries: WebAuditEntry[];
  /** Whether older calls may exist that are not listed. */
  truncated: boolean;
}

/** `HEAD`-to-index state of a changed path (ADR-0085). */
export type GitIndexState = "added" | "modified" | "deleted" | "renamed" | "type_change";

/** Index-to-worktree state of a changed path (ADR-0085). */
export type GitWorktreeState = "modified" | "deleted" | "untracked" | "conflicted";

/** One changed path. `path` is display text, never a path to open. */
export interface GitPathView {
  /** Relative to the workspace; a trailing `/` marks an untracked folder. */
  path: string;
  index: GitIndexState | null;
  worktree: GitWorktreeState | null;
}

/** What the Git status tab shows about a session's workspace (ADR-0085). */
export interface GitStatusView {
  branch: BranchView;
  state: "ready" | "not_repository" | "unavailable";
  /** A fixed reason when `state` is `unavailable`. */
  note: string | null;
  /** Sorted by path, at most 1,000. */
  paths: GitPathView[];
  truncated: boolean;
}

export interface ProviderProfileView {
  name: string;
  kind: string;
  auth: string;
  endpoint: string | null;
  credential_bound: boolean;
  /** A local LM Studio or Ollama profile with a recorded endpoint (ADR-0064). */
  selectable: boolean;
  /** Whether it is the active connection. */
  active: boolean;
  /** For a signed-in ChatGPT connection, the account's email, for display only (ADR-0093). */
  account_label: string | null;
}

/** The answer to `settings_sign_out_chatgpt` (ADR-0093). */
export interface ChatGptSignOutResult {
  overview: SettingsOverview;
  /** Whether OpenAI confirmed the revocation; the local sign-out is recorded either way. */
  revoked: boolean;
}

/** A local provider the settings window may record (ADR-0064). */
export type LocalProviderKind = "lm_studio" | "ollama_local";

/** A hosted provider with one fixed endpoint that Rust supplies (ADR-0071, ADR-0092). */
export type FixedHostedKind =
  | "ollama_cloud"
  | "anthropic"
  | "openai"
  | "google"
  | "meta"
  | "moonshot"
  | "tencent"
  | "thinking_machines"
  | "minimax"
  | "open_router"
  | "z_ai"
  | "deepseek"
  | "xai";

/**
 * A provider the settings window may add: a local one, a named hosted one, or any
 * OpenAI-compatible service at an HTTPS address the operator types (ADR-0092).
 */
export type NewConnectionKind = LocalProviderKind | FixedHostedKind | "openai_compatible";

export interface ToolchainProfileView {
  name: string;
  platform: "linux" | "windows";
  read_only_roots: string[];
  path_entries: string[];
  environment_names: string[];
}

export interface PackageProfileView {
  name: string;
  origins: string[];
  credentials_bound: number;
}

export interface ContainerGrantView {
  grant_id: string;
  workspace_id: string;
  directory: string;
  access: "read_execute" | "read_write";
  state: "pending" | "active" | "revoking";
}

/**
 * What the selected workspace's container needs before a run can read one proposed root
 * (ADR-0103). `not_applicable` off Windows or with no workspace selected.
 */
export type RootGrantNeed =
  | "not_applicable"
  | "readable"
  | "needs_grant"
  | "needs_administrator"
  | "unknown";

export type ToolchainFamily =
  | "rust"
  | "c_cpp"
  | "java"
  | "python"
  | "perl"
  | "go"
  | "node"
  | "dot_net"
  | "ruby";

/**
 * One toolchain profile proposal that Rust made by reading install folders (ADR-0103). Every
 * string is display-safe text; none is ever sent back. A change names a proposal by `digest`.
 */
export interface ToolchainProposalView {
  digest: string;
  family: ToolchainFamily;
  family_label: string;
  name: string;
  version: string | null;
  read_only_roots: Array<{ folder: string; grant: RootGrantNeed }>;
  path_entries: string[];
  environment: Array<{ name: string; value: string }>;
  notes: string[];
  /** Why core would refuse to add it now, or `null`. */
  refusal: string | null;
  /** A profile with this name is already recorded. */
  recorded: boolean;
}

/** The reply to the three discovery commands. */
export interface ToolchainDiscovery {
  overview: SettingsOverview;
  /** The workspace the roots report container access for, or `null`. */
  workspace_id: string | null;
  /**
   * Windows with a workspace: whether it has the active read-write grant on its own folder that
   * every run needs. `null` otherwise.
   */
  workspace_grant_active: boolean | null;
  proposals: ToolchainProposalView[];
}

export interface SkillSettingView {
  name: string;
  application: boolean | null;
  workspace_overrides: Array<{ workspace_id: string; enabled: boolean }>;
}

/** The read-only configuration shown in the settings window only. */
export interface ChatDefaultsView {
  defaults: ChatBudgets;
  source: PreferencesSource;
  /** Largest output reserve Rust's request type holds (a 32-bit count). */
  max_output_tokens_max: number;
  tool_call_limit_max: number;
  turn_timeout_seconds_max: number;
  /**
   * Largest global instruction text a save accepts, in UTF-8 bytes: 16 KiB less the longest
   * saved workspace text and the headings a turn adds (ADR-0080); `null` when a workspace's
   * saved text cannot be read now, so the room is unknown.
   */
  instructions_max_bytes: number | null;
}

export interface SettingsOverview {
  revision: number;
  development_root: string | null;
  workspaces: WorkspaceSettingsView[];
  connection: ConnectionView;
  provider_profiles: ProviderProfileView[];
  toolchain_profiles: ToolchainProfileView[];
  package_profiles: PackageProfileView[];
  container_grants: ContainerGrantView[];
  /** Whether this computer can record container grants (Windows only). */
  container_grants_supported: boolean;
  skills: SkillSettingView[];
  /** The chat defaults shown beside the model connection (ADR-0080). */
  chat: ChatDefaultsView;
  /** The workspace the window is scoped to (ADR-0071), or `null` for the full view. Display only. */
  scope: { workspace_id: string } | null;
}

/** What one entry of a workspace directory is. `other` covers links and special entries. */
export type WorkspaceEntryKind = "directory" | "file" | "other";

/** One name in a workspace directory (ADR-0083). The name is untrusted text. */
export interface WorkspaceEntry {
  name: string;
  kind: WorkspaceEntryKind;
  /** The native name is not valid Unicode: `name` is for display and is never sent back. */
  lossy: boolean;
  /**
   * A directory Rust would list next: its name, joined to the listed path, passes the same
   * checks as a request. False for files, other entries, lossy names, and refused names.
   */
  openable: boolean;
}

/** One directory of the open session's workspace: names and kinds, directories first. */
export interface WorkspaceListing {
  entries: WorkspaceEntry[];
  /** More entries exist than were returned. */
  truncated: boolean;
}
