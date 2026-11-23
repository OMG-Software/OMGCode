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

/** Saved composer defaults (ADR-0059). They grant nothing and carry no endpoint. */
export interface ChatPreferences {
  model: string;
  context_tokens: number;
  max_output_tokens: number;
  max_tool_calls: number;
  instructions: string;
  compact_threshold: number;
  auto_compact: boolean;
  /** Enter sends and Shift+Enter adds a newline; otherwise Ctrl+Enter sends (ADR-0062). */
  enter_to_send: boolean;
}

export type PreferencesSource = "defaults" | "saved" | "unreadable";

export interface ChatDefaults {
  settings: ChatSettings;
  preferences: PreferencesSource;
  /** A composer keyboard choice, not a turn setting (ADR-0062). */
  enter_to_send: boolean;
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
  write_tools: WriteToolAccess;
  package_cache_root: string | null;
}

/** The complete preference set a permissions change sends. Every field is required. */
export interface PermissionFlags {
  model_tools: boolean;
  read: Access;
  write: Access;
  execute: Access;
  task_network: Access;
  host_services: Access;
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

/** One connection the main window's status bar may offer (ADR-0064). Never a credential. */
export interface ConnectionChoice {
  name: string;
  /** `lm_studio`, `ollama_local`, or `ollama_cloud`. */
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
}

/** A local provider the settings window may record (ADR-0064). */
export type LocalProviderKind = "lm_studio" | "ollama_local";

/** A provider the settings window may add: a local one, or hosted Ollama Cloud (ADR-0071). */
export type NewConnectionKind = LocalProviderKind | "ollama_cloud";

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

export interface SkillSettingView {
  name: string;
  application: boolean | null;
  workspace_overrides: Array<{ workspace_id: string; enabled: boolean }>;
}

/** The read-only configuration shown in the settings window only. */
export interface SettingsOverview {
  revision: number;
  development_root: string | null;
  workspaces: WorkspaceSettingsView[];
  connection: ConnectionView;
  provider_profiles: ProviderProfileView[];
  toolchain_profiles: ToolchainProfileView[];
  package_profiles: PackageProfileView[];
  container_grants: ContainerGrantView[];
  skills: SkillSettingView[];
  /** The workspace the window is scoped to (ADR-0071), or `null` for the full view. Display only. */
  scope: { workspace_id: string } | null;
}
