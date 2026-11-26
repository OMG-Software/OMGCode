import { vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type {
  ChatDefaultsView,
  ChatSettings,
  ConnectionStatus,
  ReasoningEffort,
  SessionSummary,
  SessionTree,
  TranscriptEntry,
  TranscriptPage,
} from "../src/ipc/types";

export type Handler = (args: Record<string, unknown>) => unknown;

export interface Call {
  command: string;
  args: Record<string, unknown>;
}

export interface FakeIpc {
  calls: Call[];
  callsTo(command: string): Call[];
  /** Replaces or adds a handler. */
  on(command: string, handler: Handler): void;
}

export const SETTINGS: ChatSettings = {
  endpoint: "http://127.0.0.1:1234/v1",
  model: "test-model",
  context_tokens: 8192,
  max_output_tokens: 1024,
  max_tool_calls: 128,
  turn_timeout_seconds: 120,
  instructions: "",
  compact_threshold: 200_000,
  auto_compact: true,
};

export const CHAT_BUDGET_BOUNDS = {
  tool_call_limit_max: 256,
  turn_timeout_seconds_max: 3600,
  compact_threshold_min: 128_000,
  compact_threshold_max: 1_000_000,
  compact_threshold_step: 8_000,
};

/** The settings window's chat defaults (ADR-0080): the shared fixture's saved values. */
export function chatDefaultsView(overrides: Partial<ChatDefaultsView> = {}): ChatDefaultsView {
  return {
    defaults: {
      context_tokens: SETTINGS.context_tokens,
      max_output_tokens: SETTINGS.max_output_tokens,
      max_tool_calls: SETTINGS.max_tool_calls,
      turn_timeout_seconds: SETTINGS.turn_timeout_seconds,
      instructions: SETTINGS.instructions,
    },
    source: "saved",
    max_output_tokens_max: 4_294_967_295,
    tool_call_limit_max: 256,
    turn_timeout_seconds_max: 3600,
    instructions_max_bytes: 16 * 1024,
    ...overrides,
  };
}

export function rejection(code: string, message: string): never {
  throw { code, message };
}

/** Installs a fake transport. Unknown commands reject with `unknown_command`. */
export function installFakeIpc(handlers: Record<string, Handler> = {}): FakeIpc {
  const table = new Map<string, Handler>(Object.entries(handlers));
  const calls: Call[] = [];
  setInvoker(async (command, args) => {
    calls.push({ command, args });
    const handler = table.get(command);
    if (handler === undefined) {
      return rejection("unknown_command", command);
    }
    return handler(args);
  });
  return {
    calls,
    callsTo: (command) => calls.filter((c) => c.command === command),
    on: (command, handler) => {
      table.set(command, handler);
    },
  };
}

/** The default-connection status the main window reads for a session (ADR-0071). */
export function connectionStatus(overrides: Partial<ConnectionStatus> = {}): ConnectionStatus {
  return {
    connection: {
      endpoint: SETTINGS.endpoint,
      source: "active",
      profile: null,
      kind: null,
      selectable: true,
    },
    origin: "default",
    model: SETTINGS.model,
    // A loopback connection takes every effort core accepts (ADR-0102); none is chosen.
    reasoning_effort: null,
    reasoning_efforts: [...ALL_EFFORTS],
    stale: false,
    session_choices_unreadable: false,
    choices: [],
    ...overrides,
  };
}

/** Every reasoning effort core accepts, in its order (ADR-0102). */
export const ALL_EFFORTS: readonly ReasoningEffort[] = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

/** Handlers every main-window test needs. */
export function mainHandlers(overrides: Record<string, Handler> = {}): Record<string, Handler> {
  return {
    session_tree: () => tree([]),
    chat_defaults: () => ({
      settings: { ...SETTINGS },
      ...CHAT_BUDGET_BOUNDS,
      preferences: "defaults",
      enter_to_send: true,
    }),
    // The composer's model choice (ADR-0066): the default model is listed.
    chat_models: (args) => ({
      session_id: args.session_id as string,
      endpoint: SETTINGS.endpoint,
      kind: null,
      models: [{ id: SETTINGS.model, cloud_tag: false }],
    }),
    session_skills: () => ({ workspace_registered: true, skills: [] }),
    settings_show_window: () => null,
    settings_show_workspace: () => null,
    approvals_summary: () => ({ pending: 0 }),
    approvals_show_window: () => null,
    // The status bar (ADR-0064): the default connection, nothing else to choose.
    connection_status: () => connectionStatus(),
    // Records nothing; the status read back keeps the fixture model.
    session_model_select: (args) => connectionStatus({ model: args.model as string }),
    session_effort_select: (args) =>
      connectionStatus({ reasoning_effort: args.reasoning_effort as ReasoningEffort | null }),
    session_context: () => ({
      workspace_registered: true,
      branch: { state: "branch", name: "main", note: null },
    }),
    ...overrides,
  };
}

export function session(overrides: Partial<SessionSummary> = {}): SessionSummary {
  return {
    session_id: "11111111-1111-4111-8111-111111111111",
    title: "A session",
    created_at: "2026-01-01T10:00:00Z",
    updated_at: "2026-01-02T10:00:00Z",
    status: "idle",
    archived: false,
    event_count: 3,
    invalid_code: null,
    ...overrides,
  };
}

export function tree(
  groups: Array<{ id: string; folder: string; sessions: SessionSummary[] }>,
  skipped = 0,
): SessionTree {
  return {
    groups: groups.map((g) => ({
      group_id: g.id,
      folder_display: g.folder,
      sessions: g.sessions,
    })),
    indexed_sessions: groups.reduce((n, g) => n + g.sessions.length, 0),
    skipped_entries: skipped,
  };
}

export function userMessage(seq: number, text: string): TranscriptEntry {
  return {
    seq,
    timestamp: "2026-01-02T10:00:00Z",
    kind: "message",
    role: "user",
    source: "chat",
    text,
    call_id: null,
  };
}

export function page(
  summary: SessionSummary,
  entries: TranscriptEntry[],
  extra: Partial<TranscriptPage> = {},
): TranscriptPage {
  return {
    session: summary,
    folder_display: "C:\\work",
    entries,
    next_after_seq: null,
    incomplete_tail: false,
    ...extra,
  };
}

/** Lets pending promises and fake timers run for `ms` milliseconds. */
export async function settle(ms = 0): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

export function mountRoot(): HTMLElement {
  document.body.replaceChildren();
  const root = document.createElement("div");
  root.id = "app";
  document.body.appendChild(root);
  return root;
}

/** The icon button whose accessible name (also its tooltip) is `label`. */
export function buttonByLabel(scope: ParentNode, label: string): HTMLButtonElement {
  const found = Array.from(scope.querySelectorAll("button")).find(
    (b) => b.getAttribute("aria-label") === label,
  );
  if (found === undefined) {
    throw new Error(`No button labelled ${label}`);
  }
  return found;
}

export function buttonByText(scope: ParentNode, label: string): HTMLButtonElement {
  const found = Array.from(scope.querySelectorAll("button")).find(
    (b) => b.textContent?.trim().startsWith(label) === true,
  );
  if (found === undefined) {
    throw new Error(`No button starting with "${label}"`);
  }
  return found;
}

/** Chooses `value` in a `<select>` as the operator would. */
export function choose(select: HTMLSelectElement, value: string): void {
  select.value = value;
  select.dispatchEvent(new Event("change", { bubbles: true }));
}

/** The model choice in the side panel's model box (ADR-0089). */
export function modelSelect(scope: ParentNode): HTMLSelectElement {
  const found = scope.querySelector<HTMLSelectElement>('.model-box select[aria-label="Model"]');
  if (found === null) {
    throw new Error("No model choice");
  }
  return found;
}

/** The model box's Refresh button; the side panel tabs have Refresh buttons of their own. */
export function modelRefresh(scope: ParentNode): HTMLButtonElement {
  const box = scope.querySelector<HTMLElement>(".model-box");
  if (box === null) {
    throw new Error("No model box");
  }
  return buttonByText(box, "Refresh");
}

export function hasButton(scope: ParentNode, label: string): boolean {
  return Array.from(scope.querySelectorAll("button")).some(
    (b) => b.textContent?.trim().startsWith(label) === true,
  );
}

export const HOSTILE_IMG = "<img src=x onerror=alert(1)>";
