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

export interface WorkspaceChoice {
  workspace_id: string;
  folder_display: string;
  model_tools: boolean;
  read: boolean;
  write: boolean;
  execute: boolean;
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
export type ApprovalOperation = "command" | "file_write" | "git_push" | "network";

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
  instructions: string;
  compact_threshold: number;
  auto_compact: boolean;
}

export type PreferencesSource = "defaults" | "saved" | "unreadable";

export interface ChatDefaults {
  settings: ChatSettings;
  preferences: PreferencesSource;
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
  read: boolean;
  write: boolean;
  execute: boolean;
  task_network: boolean;
  host_services: boolean;
  package_cache_root: string | null;
}

/** The complete preference set a permissions change sends. Every flag is required. */
export interface PermissionFlags {
  model_tools: boolean;
  read: boolean;
  write: boolean;
  execute: boolean;
  task_network: boolean;
  host_services: boolean;
}

export type ConnectionSource = "environment" | "profile" | "injected";

export interface ConnectionView {
  endpoint: string;
  source: ConnectionSource;
  profile: string | null;
}

export interface ProviderProfileView {
  name: string;
  kind: string;
  auth: string;
  endpoint: string | null;
  credential_bound: boolean;
}

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
}
