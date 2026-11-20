import { append, el, neutralise, text } from "../dom";
import { formatAbsolute } from "../format";
import type { TranscriptEntry } from "../ipc/types";
import { renderPng } from "./capture";
import type { ChatPreview } from "./chat_runner";
import { copyPng, copyText, feedbackButton } from "./clipboard";
import { renderMarkdown } from "./markdown";
import { plainText } from "./plain_text";

const ROLE_LABELS = { user: "You", assistant: "Assistant", tool: "Tool" } as const;

function entryShell(entry: TranscriptEntry, className: string): HTMLElement {
  return el("article", { class: `entry ${className}`, attrs: { "data-seq": String(entry.seq) } });
}

function head(label: string, entry: TranscriptEntry, extra?: string): HTMLElement {
  return el(
    "div",
    { class: "entry-head" },
    el("span", { class: "entry-label", text: label, attrs: { title: neutralise(label) } }),
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
export function createLiveEntry(model: string | null = null): LiveEntry {
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
      el("span", {
        class: "entry-label",
        text: `${replyLabel(model)} (generating, not saved yet)`,
      }),
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

type MessageEntry = Extract<TranscriptEntry, { kind: "message" }>;

/** What an entry needs from the entries before it. */
export interface EntryContext {
  /** The model of the current turn's latest request manifest, or null when none is loaded. */
  model: string | null;
}

/**
 * The label of an assistant reply: the model id recorded in the turn's request manifest, or
 * "Assistant" when no manifest precedes the reply. The id is untrusted text chosen by the model
 * server and only names the model that was asked; it is not an identity check.
 */
export function replyLabel(model: string | null): string {
  return model === null || model.trim() === "" ? ROLE_LABELS.assistant : model;
}

/**
 * The context of each entry, in order. A request manifest sets the model for the replies after
 * it, and a turn start clears it, so a reply is never labelled with an earlier turn's model.
 * Transcripts load from the oldest entry, so every earlier manifest of a turn is present.
 */
export function entryContexts(entries: readonly TranscriptEntry[]): EntryContext[] {
  let model: string | null = null;
  return entries.map((entry) => {
    if (entry.kind === "turn_started") {
      model = null;
    } else if (entry.kind === "request_manifest") {
      model = entry.model;
    }
    return { model };
  });
}

/**
 * The actions of a recorded reply (ADR-0068): copy its Markdown source, copy it as plain text,
 * and copy an image of the whole entry. Each writes to the clipboard only on a click; nothing
 * reads it. The row is left out of the image. The source is copied with the same visible markers
 * the screen shows in place of bidirectional overrides.
 */
function replyActions(entry: MessageEntry, node: HTMLElement, body: HTMLElement): HTMLElement {
  return el(
    "div",
    {
      class: "reply-actions",
      attrs: { role: "group", "aria-label": "Reply actions", "data-capture": "skip" },
    },
    feedbackButton("Copy Markdown", "Copied", () => copyText(neutralise(entry.text)), {
      class: "reply-action",
    }),
    feedbackButton("Copy text", "Copied", () => copyText(plainText(body)), {
      class: "reply-action",
    }),
    feedbackButton("Copy image", "Copied", () => copyPng(renderPng(node)), {
      class: "reply-action",
    }),
  );
}

/**
 * A recorded assistant reply is rendered as Markdown built from elements and text nodes
 * (ADR-0067); a reply too long to parse, and every other role, is shown as plain text.
 */
function messageBody(entry: MessageEntry): HTMLElement {
  const rendered = entry.role === "assistant" ? renderMarkdown(entry.text) : null;
  return rendered ?? el("div", { class: "bubble-text", text: entry.text });
}

/** Renders one transcript entry. Every string is shown as text or inert Markdown elements. */
export function renderEntry(
  entry: TranscriptEntry,
  context: EntryContext = { model: null },
): HTMLElement {
  switch (entry.kind) {
    case "message": {
      const node = entryShell(entry, `bubble bubble-${entry.role}`);
      const body = messageBody(entry);
      if (entry.role !== "assistant") {
        append(node, head(ROLE_LABELS[entry.role], entry, entry.source), body);
        return node;
      }
      append(
        node,
        head(replyLabel(context.model), entry, entry.source),
        body,
        replyActions(entry, node, body),
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
