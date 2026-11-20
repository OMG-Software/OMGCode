import { Channel, invoke } from "@tauri-apps/api/core";
import { IpcFailure, toIpcError } from "./errors";
import type {
  AppInfo,
  ChatDefaults,
  ChatModels,
  ChatPreferences,
  ChatSettings,
  ChatStatus,
  ConnectionStatus,
  SearchResult,
  SessionContext,
  SessionSkills,
  SessionSummary,
  SessionTree,
  TranscriptPage,
  TreeSort,
  TreeView,
  WorkspaceList,
} from "./types";

/** The only module that imports `@tauri-apps/api`. All IPC goes through `invokeCommand`. */
export type Invoker = (command: string, args: Record<string, unknown>) => Promise<unknown>;

export const NO_HOST_MESSAGE = "This page must run inside OMGCode.";

let injected: Invoker | null = null;

/** Replaces the transport. Tests inject a fake; `null` restores the default. */
export function setInvoker(invoker: Invoker | null): void {
  injected = invoker;
}

function tauriPresent(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** True when a transport exists: an injected fake, Tauri, or the development mock. */
export function hostAvailable(): boolean {
  return injected !== null || tauriPresent() || import.meta.env.DEV;
}

async function resolveInvoker(): Promise<Invoker> {
  if (injected !== null) {
    return injected;
  }
  if (tauriPresent()) {
    return (command, args) => invoke(command, args);
  }
  if (import.meta.env.DEV) {
    const mock = await import("../mock/backend");
    return mock.mockInvoke;
  }
  throw new IpcFailure({ code: "no_host", message: NO_HOST_MESSAGE });
}

/** Runs one command. Rejects with `IpcFailure`; the message is untrusted text. */
export async function invokeCommand<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    const invoker = await resolveInvoker();
    return (await invoker(command, args)) as T;
  } catch (reason) {
    throw new IpcFailure(toIpcError(reason));
  }
}

/**
 * Calls a command whose only argument, `on_event`, is a push channel. Inside Tauri it is a real
 * `Channel`; an injected invoker or the development mock receives a plain `{ onmessage }`
 * object instead. `onMessage` receives raw, untrusted values and must validate them.
 */
export async function invokeWithChannel(
  command: string,
  onMessage: (value: unknown) => void,
): Promise<void> {
  if (injected === null && tauriPresent()) {
    const channel = new Channel<unknown>();
    channel.onmessage = onMessage;
    await invokeCommand<null>(command, { on_event: channel });
    return;
  }
  await invokeCommand<null>(command, { on_event: { onmessage: onMessage } });
}

export const api = {
  appInfo: () => invokeCommand<AppInfo>("app_info", {}),
  sessionTree: (view: TreeView, sort: TreeSort, descending: boolean) =>
    invokeCommand<SessionTree>("session_tree", { view, sort, descending }),
  sessionSearch: (query: string, view: TreeView, limit: number) =>
    invokeCommand<SearchResult>("session_search", { query, view, limit }),
  sessionTranscript: (sessionId: string, afterSeq: number | null, limit: number) =>
    invokeCommand<TranscriptPage>("session_transcript", {
      session_id: sessionId,
      after_seq: afterSeq,
      limit,
    }),
  sessionArchive: (sessionId: string) =>
    invokeCommand<SessionSummary>("session_archive", { session_id: sessionId }),
  sessionRestore: (sessionId: string) =>
    invokeCommand<SessionSummary>("session_restore", { session_id: sessionId }),
  sessionInterrupt: (sessionId: string) =>
    invokeCommand<SessionSummary>("session_interrupt", { session_id: sessionId }),
  workspaceList: () => invokeCommand<WorkspaceList>("workspace_list", {}),
  sessionCreate: (workspaceId: string, title: string) =>
    invokeCommand<SessionSummary>("session_create", { workspace_id: workspaceId, title }),
  chatDefaults: () => invokeCommand<ChatDefaults>("chat_defaults", {}),
  /** The models the session's connection reports (ADR-0066, ADR-0070). */
  chatModels: (sessionId: string) =>
    invokeCommand<ChatModels>("chat_models", { session_id: sessionId }),
  chatPreferencesSave: (preferences: ChatPreferences) =>
    invokeCommand<ChatDefaults>("chat_preferences_save", { preferences }),
  chatSend: (sessionId: string, message: string, settings: ChatSettings, skills: string[]) =>
    invokeCommand<ChatStatus>("chat_send", { session_id: sessionId, message, settings, skills }),
  sessionSkills: (sessionId: string) =>
    invokeCommand<SessionSkills>("session_skills", { session_id: sessionId }),
  sessionContext: (sessionId: string) =>
    invokeCommand<SessionContext>("session_context", { session_id: sessionId }),
  /** The session's connection and model; `null` describes the default when no session is open. */
  connectionStatus: (sessionId: string | null) =>
    invokeCommand<ConnectionStatus>("connection_status", { session_id: sessionId }),
  /**
   * Names a profile, or `null` for the default, for one session; Rust asks for a native
   * confirmation first and records the choice only on Yes.
   */
  connectionSelect: (sessionId: string, profile: string | null) =>
    invokeCommand<ConnectionStatus>("connection_select", { session_id: sessionId, profile }),
  /** Records the session's model. No dialog: a model grants nothing. */
  sessionModelSelect: (sessionId: string, model: string) =>
    invokeCommand<ConnectionStatus>("session_model_select", { session_id: sessionId, model }),
  chatStatus: (sessionId: string) =>
    invokeCommand<ChatStatus>("chat_status", { session_id: sessionId }),
  chatCancel: (sessionId: string) =>
    invokeCommand<ChatStatus>("chat_cancel", { session_id: sessionId }),
  approvalsSummary: () => invokeCommand<{ pending: number }>("approvals_summary", {}),
  approvalsShowWindow: () => invokeCommand<null>("approvals_show_window", {}),
  settingsShowWindow: () => invokeCommand<null>("settings_show_window", {}),
  /** Shows the settings window scoped to the session's workspace; Rust resolves the workspace. */
  settingsShowWorkspace: (sessionId: string) =>
    invokeCommand<null>("settings_show_workspace", { session_id: sessionId }),
};
