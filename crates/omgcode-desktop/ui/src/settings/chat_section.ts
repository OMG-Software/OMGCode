import { button, el, replaceChildren } from "../dom";
import { utf8Length } from "../format";
import type { ChatBudgets, ChatDefaultsView, WorkspaceSettingsView } from "../ipc/types";
import type { ChatDraft, SectionContext } from "./context";

/** What the chat defaults block says about where its values apply (ADR-0080). */
export const CHAT_DEFAULTS_HELP =
  "Used by every session's next message. The composer keeps auto-compact and the send key.";
export const GLOBAL_INSTRUCTIONS_HELP =
  "Sent with every message, before the workspace's own instructions, which the limit leaves room for. Instructions grant no permission.";
export const WORKSPACE_INSTRUCTIONS_HELP =
  "Sent with messages in this workspace's sessions, after the global instructions, which the limit leaves room for. They grant no permission.";
export const DEFAULTS_UNREADABLE =
  "Saved chat defaults could not be read; built-in values are shown. Saving replaces a damaged file, but is refused while another program holds it.";
export const INSTRUCTIONS_UNREADABLE =
  "Saved instructions for this workspace could not be read, and its sessions cannot send. Saving replaces them, but is refused while another program holds the file.";

/** One numeric chat default: its draft key, label, and smallest value. */
const NUMBER_FIELDS: ReadonlyArray<
  readonly [key: Exclude<keyof ChatDraft, "instructions">, label: string, min: number]
> = [
  ["context_tokens", "Context tokens", 1],
  ["max_output_tokens", "Max output tokens", 1],
  ["max_tool_calls", "Tool calls per turn", 1],
  ["turn_timeout_seconds", "Tool-enabled turn timeout (seconds)", 1],
];

/** The draft of the saved values, as the form shows them. */
export function draftOf(defaults: ChatBudgets): ChatDraft {
  return {
    context_tokens: String(defaults.context_tokens),
    max_output_tokens: String(defaults.max_output_tokens),
    max_tool_calls: String(defaults.max_tool_calls),
    turn_timeout_seconds: String(defaults.turn_timeout_seconds),
    instructions: defaults.instructions,
  };
}

/** The largest value each numeric field accepts, or `null` for none the window knows. */
function maxOf(key: keyof ChatDraft, chat: ChatDefaultsView): number | null {
  if (key === "max_output_tokens") return chat.max_output_tokens_max;
  if (key === "max_tool_calls") return chat.tool_call_limit_max;
  if (key === "turn_timeout_seconds") return chat.turn_timeout_seconds_max;
  return null;
}

/**
 * The values to save, or the first problem with the draft. Rust checks every value again; this
 * only keeps an obviously wrong value from leaving the window.
 */
export function budgetsOf(draft: ChatDraft, chat: ChatDefaultsView): ChatBudgets | string {
  const numbers: Partial<Record<keyof ChatDraft, number>> = {};
  for (const [key, label, min] of NUMBER_FIELDS) {
    const value = Number(draft[key].trim());
    const max = maxOf(key, chat);
    if (draft[key].trim() === "" || !Number.isSafeInteger(value) || value < min) {
      return `Set ${label} to a whole number of at least ${min}.`;
    }
    if (max !== null && value > max) {
      return `Set ${label} to a whole number from ${min} to ${max}.`;
    }
    numbers[key] = value;
  }
  // An unknown room is left to Rust, which reads the other text again when it saves.
  const room = chat.instructions_max_bytes;
  if (room !== null && utf8Length(draft.instructions) > room) {
    return `Shorten the global instructions to at most ${room} bytes, so they fit beside the longest workspace instructions.`;
  }
  return {
    context_tokens: numbers.context_tokens ?? 0,
    max_output_tokens: numbers.max_output_tokens ?? 0,
    max_tool_calls: numbers.max_tool_calls ?? 0,
    turn_timeout_seconds: numbers.turn_timeout_seconds ?? 0,
    instructions: draft.instructions,
  };
}

/** What a counter says when other saved instructions cannot be read now. */
export const ROOM_UNKNOWN = "room unknown until the other saved instructions can be read";

/** What the window says when a workspace's text would not fit beside the global text. */
export function workspaceTooLong(maxBytes: number): string {
  return `Shorten these instructions to at most ${maxBytes} bytes, so they fit beside the global instructions.`;
}

/**
 * A byte counter against a room. A `null` room is unknown: other saved instructions cannot be
 * read now, so the counter says so and Rust decides on save.
 */
function byteCounter(maxBytes: number | null): { element: HTMLElement; show(text: string): void } {
  const element = el("span", { class: "muted byte-counter", attrs: { "aria-live": "polite" } });
  return {
    element,
    show(text) {
      const bytes = utf8Length(text);
      replaceChildren(
        element,
        maxBytes === null ? `${bytes} bytes; ${ROOM_UNKNOWN}` : `${bytes} / ${maxBytes} bytes`,
      );
      element.classList.toggle("over", maxBytes !== null && bytes > maxBytes);
    },
  };
}

function labelled(label: string, id: string, control: HTMLElement): HTMLElement {
  return el("div", { class: "field" }, el("label", { text: label, attrs: { for: id } }), control);
}

/**
 * The chat budgets and the global custom instructions, shown beside the model connection. What
 * the operator types is kept in view state until a save succeeds, so a redraw keeps it.
 */
export function chatDefaultsBlock({
  overview,
  view,
  submit,
  actions,
}: SectionContext): HTMLElement {
  const { chat } = overview;
  const draft = view.chatDraft ?? draftOf(chat.defaults);
  const remember = (): void => {
    view.chatDraft = { ...draft };
  };
  const problem = el("p", { class: "error", attrs: { role: "alert" } });
  problem.hidden = true;
  const fields = NUMBER_FIELDS.map(([key, label, min]) => {
    const id = `chat-${key.replace(/_/g, "-")}`;
    const input = el("input", {
      attrs: { id, type: "number", min: String(min), step: "1", inputmode: "numeric" },
    });
    const max = maxOf(key, chat);
    if (max !== null) input.max = String(max);
    input.value = draft[key];
    input.addEventListener("input", () => {
      draft[key] = input.value;
      remember();
    });
    return labelled(label, id, input);
  });
  const instructions = el("textarea", {
    attrs: { id: "global-instructions", rows: "5", spellcheck: "true" },
  });
  instructions.value = draft.instructions;
  const counter = byteCounter(chat.instructions_max_bytes);
  counter.show(draft.instructions);
  instructions.addEventListener("input", () => {
    draft.instructions = instructions.value;
    counter.show(instructions.value);
    remember();
  });
  const save = button("Save chat defaults", () => {
    const budgets = budgetsOf(draft, chat);
    if (typeof budgets === "string") {
      replaceChildren(problem, budgets);
      problem.hidden = false;
      return;
    }
    const change = actions.saveChatDefaults(budgets);
    submit(async (revision) => {
      const saved = await change(revision);
      // Reached only when Rust saved them; a refusal keeps the draft.
      view.chatDraft = null;
      return saved;
    });
  });
  save.classList.add("primary");
  return el(
    "section",
    { class: "block chat-defaults" },
    el("h3", { text: "Chat defaults" }),
    el("p", { class: "muted", text: CHAT_DEFAULTS_HELP }),
    chat.source === "unreadable" ? el("p", { class: "notice", text: DEFAULTS_UNREADABLE }) : null,
    el(
      "div",
      { class: "form" },
      el("div", { class: "number-grid" }, ...fields),
      labelled("Global custom instructions", "global-instructions", instructions),
      el(
        "div",
        { class: "row-actions" },
        counter.element,
        el("span", { class: "muted", text: GLOBAL_INSTRUCTIONS_HELP }),
      ),
      problem,
      el("div", { class: "row-actions" }, save),
    ),
  );
}

/**
 * One workspace's custom instructions. A draft is kept per workspace until its save succeeds;
 * an empty save removes them.
 */
export function workspaceInstructionsBlock(
  workspace: WorkspaceSettingsView,
  { view, submit, actions }: SectionContext,
): HTMLElement {
  const id = workspace.workspace_id;
  const maxBytes = workspace.instructions_max_bytes;
  const saved = workspace.instructions;
  const problem = el("p", { class: "error", attrs: { role: "alert" } });
  problem.hidden = true;
  const text = view.instructionDrafts.get(id) ?? saved ?? "";
  const box = el("textarea", {
    attrs: { id: `workspace-instructions-${id}`, rows: "5", spellcheck: "true" },
  });
  box.value = text;
  const counter = byteCounter(maxBytes);
  counter.show(text);
  box.addEventListener("input", () => {
    view.instructionDrafts.set(id, box.value);
    counter.show(box.value);
  });
  const save = button("Save instructions", () => {
    const value = box.value;
    // Rust checks again; this only keeps text that cannot be sent from leaving the window.
    if (maxBytes !== null && utf8Length(value) > maxBytes) {
      replaceChildren(problem, workspaceTooLong(maxBytes));
      problem.hidden = false;
      return;
    }
    const change = actions.setWorkspaceInstructions(id, value);
    submit(async (revision) => {
      const result = await change(revision);
      view.instructionDrafts.delete(id);
      return result;
    });
  });
  return el(
    "div",
    { class: "workspace-instructions" },
    labelled("Custom instructions", box.id, box),
    saved === null ? el("p", { class: "notice", text: INSTRUCTIONS_UNREADABLE }) : null,
    el(
      "div",
      { class: "row-actions" },
      counter.element,
      el("span", { class: "muted", text: WORKSPACE_INSTRUCTIONS_HELP }),
    ),
    problem,
    el("div", { class: "row-actions" }, save),
  );
}
