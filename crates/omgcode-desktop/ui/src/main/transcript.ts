import { append, el, neutralise, text } from "../dom";
import { formatAbsolute } from "../format";
import type { TranscriptEntry } from "../ipc/types";
import type { ChatPreview } from "./chat_runner";

const ROLE_LABELS = { user: "You", assistant: "Assistant", tool: "Tool" } as const;

function entryShell(entry: TranscriptEntry, className: string): HTMLElement {
  return el("article", { class: `entry ${className}`, attrs: { "data-seq": String(entry.seq) } });
}

function head(label: string, entry: TranscriptEntry, extra?: string): HTMLElement {
  return el(
    "div",
    { class: "entry-head" },
    el("span", { class: "entry-label", text: label }),
    extra === undefined ? null : el("span", { class: "entry-extra", text: extra }),
    el("span", { class: "entry-time", text: formatAbsolute(entry.timestamp) }),
  );
}

function note(entry: TranscriptEntry, className: string, message: string): HTMLElement {
  const node = entryShell(entry, className);
  append(node, el("span", { class: "note-text", text: message }));
  return node;
}

/** A thin divider line with centred muted text, for turn notes and request metadata. */
function noteLine(entry: TranscriptEntry, className: string, message: string): HTMLElement {
  return note(entry, `${className} note-line`, message);
}

/** Longest argument preview in a collapsed tool row, in UTF-16 code units. */
export const TOOL_PREVIEW_CHARS = 160;

/**
 * One line of a tool call's arguments for the collapsed row: runs of whitespace become one
 * space and the text is cut at `TOOL_PREVIEW_CHARS`. It is still untrusted text; callers show it
 * only as a text node. The expanded row carries the full arguments.
 */
export function argumentPreview(args: string): string {
  const flat = args.replace(/\s+/g, " ").trim();
  if (flat.length <= TOOL_PREVIEW_CHARS) {
    return flat;
  }
  // Do not end on the first half of a surrogate pair.
  const last = flat.charCodeAt(TOOL_PREVIEW_CHARS - 1);
  const end = last >= 0xd800 && last <= 0xdbff ? TOOL_PREVIEW_CHARS - 1 : TOOL_PREVIEW_CHARS;
  return `${flat.slice(0, end)}…`;
}

export interface LiveEntry {
  element: HTMLElement;
  /** Updates the text node in place; nothing is rebuilt. */
  update(preview: ChatPreview): void;
}

/**
 * Builds the provisional entry shown while a model reply is generated. The text is untrusted and
 * lives in one text node. It is not the recorded reply and goes away when the turn settles.
 */
export function createLiveEntry(): LiveEntry {
  const body = text("");
  const cutOff = el("p", {
    class: "muted live-note",
    text: "The preview is cut off. The saved reply will appear when the turn ends.",
  });
  cutOff.hidden = true;
  const textBox = el("div", { class: "bubble-text", attrs: { "aria-live": "polite" } });
  textBox.appendChild(body);
  const element = el(
    "article",
    { class: "entry bubble bubble-assistant bubble-live" },
    el(
      "div",
      { class: "entry-head" },
      el("span", { class: "entry-label", text: "Assistant (generating, not saved yet)" }),
    ),
    textBox,
    cutOff,
  );
  return {
    element,
    update(preview) {
      body.data = neutralise(preview.text);
      cutOff.hidden = !preview.truncated;
    },
  };
}

/** Renders one transcript entry. Every string is shown as text. */
export function renderEntry(entry: TranscriptEntry): HTMLElement {
  switch (entry.kind) {
    case "message": {
      const node = entryShell(entry, `bubble bubble-${entry.role}`);
      append(
        node,
        head(ROLE_LABELS[entry.role], entry, entry.source),
        el("div", { class: "bubble-text", text: entry.text }),
      );
      return node;
    }
    case "tool_requested": {
      // A compact row: the tool name and a clamped one-line preview in the summary, the full
      // arguments in the body that opens on demand.
      const node = el("details", {
        class: "entry tool-block",
        attrs: { "data-seq": String(entry.seq) },
      });
      append(
        node,
        el(
          "summary",
          { class: "tool-summary" },
          el("span", { class: "tool-label", text: "Tool request" }),
          el("span", { class: "tool-name", text: entry.tool }),
          el("span", { class: "tool-preview clamp-1", text: argumentPreview(entry.arguments) }),
          el("span", { class: "entry-time", text: formatAbsolute(entry.timestamp) }),
        ),
        el("pre", { class: "tool-args", text: entry.arguments }),
      );
      return node;
    }
    case "tool_completed": {
      const code = entry.code === null ? "" : ` (${entry.code})`;
      return note(
        entry,
        `tool-result tool-${entry.outcome}`,
        `Tool result: ${entry.outcome}${code}, ${entry.output_bytes} bytes of output`,
      );
    }
    case "turn_started":
      return noteLine(entry, "turn", "Turn started");
    case "turn_completed":
      return noteLine(entry, "turn", "Turn completed");
    case "turn_failed":
      return noteLine(
        entry,
        "turn turn-failed",
        entry.code === null ? "Turn failed" : `Turn failed: ${entry.code}`,
      );
    case "request_manifest":
      return noteLine(
        entry,
        "manifest",
        `Request to ${entry.model}, prompt ${entry.prompt_version}, tools: ${
          entry.tools.length === 0 ? "none" : entry.tools.join(", ")
        }`,
      );
    case "context_compacted": {
      const node = entryShell(entry, "summary-block");
      append(
        node,
        head(
          "Summary of earlier conversation (generated, unverified)",
          entry,
          `${entry.compacted_messages} messages, ${entry.model}`,
        ),
        el("div", { class: "bubble-text", text: entry.summary }),
      );
      return node;
    }
    case "session_archived":
      return noteLine(entry, "divider", "Session archived");
    case "session_restored":
      return noteLine(entry, "divider", "Session restored");
    default:
      return assertNever(entry);
  }
}

function assertNever(value: never): never {
  throw new Error(`Unhandled transcript entry: ${JSON.stringify(value)}`);
}
