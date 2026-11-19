import { utf8Length } from "../format";
import type { ChatOutcome, ChatSettings, SessionStatus, SessionSummary } from "../ipc/types";

export const SEARCH_MAX_CHARS = 256;
export const TITLE_MAX_BYTES = 256;
export const MESSAGE_MAX_BYTES = 16 * 1024;
export const TRANSCRIPT_PAGE_SIZE = 200;
export const SEARCH_LIMIT = 50;

export type LoadFailure = "busy" | "invalid" | null;

export interface ComposerState {
  visible: boolean;
  enabled: boolean;
  /** Why the composer is disabled, shown as text. */
  reason: string | null;
}

/** Decides whether the composer is shown and usable for the open session. */
export function composerState(
  summary: SessionSummary | null,
  chatActive: boolean,
  failure: LoadFailure = null,
): ComposerState {
  if (summary === null || failure !== null || summary.archived) {
    return { visible: false, enabled: false, reason: null };
  }
  if (summary.status === "busy" || summary.status === "invalid") {
    return { visible: false, enabled: false, reason: null };
  }
  if (chatActive || summary.status === "running") {
    return { visible: true, enabled: false, reason: "A turn is running." };
  }
  if (summary.status === "turn_open") {
    return {
      visible: true,
      enabled: false,
      reason: "The journal has an unfinished turn. Interrupt it before sending.",
    };
  }
  return { visible: true, enabled: true, reason: null };
}

export interface ActionSet {
  archive: boolean;
  restore: boolean;
  interrupt: boolean;
  cancel: boolean;
}

/** Decides which header actions apply to the open session. */
export function sessionActions(summary: SessionSummary, chatActive: boolean): ActionSet {
  return {
    archive: !summary.archived && summary.status === "idle" && !chatActive,
    restore: summary.archived,
    interrupt: summary.status === "turn_open" && !chatActive,
    cancel: summary.status === "running" || chatActive,
  };
}

const STATUS_LABELS: Record<SessionStatus, string | null> = {
  idle: null,
  running: "Running",
  turn_open: "Turn open",
  busy: "In use",
  invalid: "Unreadable",
};

/** Chip label for a status, or null when nothing is shown (idle). */
export function statusLabel(status: SessionStatus): string | null {
  return STATUS_LABELS[status];
}

/** Whether the model settings allow sending. */
export function settingsValid(settings: ChatSettings): boolean {
  return (
    settings.model.trim() !== "" &&
    Number.isFinite(settings.context_tokens) &&
    settings.context_tokens > 0
  );
}

/** Returns a problem with the message, or null when it can be sent. */
export function messageProblem(message: string): string | null {
  if (message.trim() === "") {
    return "Write a message first.";
  }
  if (utf8Length(message) > MESSAGE_MAX_BYTES) {
    return "The message is longer than 16 KiB.";
  }
  return null;
}

/** The parts of a `keydown` event that decide whether the composer sends. */
export interface SendKey {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  isComposing: boolean;
  keyCode: number;
}

/**
 * Whether a key press in the message box sends the message (ADR-0062). Ctrl+Enter (Cmd+Enter)
 * always sends. With `enterToSend`, a plain Enter sends too, and Shift+Enter or Alt+Enter keeps
 * the textarea's newline. An Enter that confirms an input-method composition never sends:
 * `isComposing` covers most engines, and `keyCode` 229 covers WebKit, which can report the
 * confirming Enter after the composition has ended.
 */
export function isSendKey(event: SendKey, enterToSend: boolean): boolean {
  if (event.key !== "Enter" || event.isComposing || event.keyCode === 229) {
    return false;
  }
  if (event.ctrlKey || event.metaKey) {
    return true;
  }
  return enterToSend && !event.shiftKey && !event.altKey;
}

/** Returns a problem with a session title, or null when it is acceptable. */
export function titleProblem(title: string): string | null {
  if (title.trim() === "") {
    return "Enter a title.";
  }
  if (utf8Length(title.trim()) > TITLE_MAX_BYTES) {
    return "The title is longer than 256 bytes.";
  }
  return null;
}

/** One-line summary of a completed chat turn. */
export function outcomeLine(outcome: ChatOutcome): string {
  const tools =
    outcome.tools_mode === "enabled"
      ? "tools enabled"
      : `tools disabled${outcome.tools_disabled_reason === null ? "" : ` (${outcome.tools_disabled_reason})`}`;
  const parts = [
    `Finished: ${outcome.finish_reason}`,
    tools,
    `${outcome.rounds} rounds`,
    `${outcome.tool_calls} tool calls`,
    `${outcome.withheld_tool_results} withheld tool results`,
  ];
  if (outcome.context !== null) {
    const c = outcome.context;
    parts.push(
      `context ${c.estimated_tokens} of ${c.context_tokens} tokens, compaction at ${c.trigger_tokens}`,
    );
  }
  return `${parts.join(", ")}.`;
}
