import { Channel, invoke } from "@tauri-apps/api/core";
import { IpcFailure, toIpcError } from "./errors";
import type {
  AppInfo,
  ChatSettings,
  ChatStatus,
  SearchResult,
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
  chatDefaults: () => invokeCommand<ChatSettings>("chat_defaults", {}),
  chatSend: (sessionId: string, message: string, settings: ChatSettings) =>
    invokeCommand<ChatStatus>("chat_send", { session_id: sessionId, message, settings }),
  chatStatus: (sessionId: string) =>
    invokeCommand<ChatStatus>("chat_status", { session_id: sessionId }),
  chatCancel: (sessionId: string) =>
    invokeCommand<ChatStatus>("chat_cancel", { session_id: sessionId }),
  approvalsSummary: () => invokeCommand<{ pending: number }>("approvals_summary", {}),
  approvalsShowWindow: () => invokeCommand<null>("approvals_show_window", {}),
};
