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
      const node = entryShell(entry, "tool-block");
      append(
        node,
        head("Tool request", entry, entry.tool),
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
      return note(entry, "turn", "Turn started");
    case "turn_completed":
      return note(entry, "turn", "Turn completed");
    case "turn_failed":
      return note(
        entry,
        "turn turn-failed",
        entry.code === null ? "Turn failed" : `Turn failed: ${entry.code}`,
      );
    case "request_manifest":
      return note(
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
      return note(entry, "divider", "Session archived");
    case "session_restored":
      return note(entry, "divider", "Session restored");
    default:
      return assertNever(entry);
  }
}

function assertNever(value: never): never {
  throw new Error(`Unhandled transcript entry: ${JSON.stringify(value)}`);
}
