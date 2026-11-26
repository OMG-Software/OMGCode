import { button, el, neutralise, replaceChildren } from "../dom";
import { formatAbsolute, formatTimeAgo } from "../format";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { SessionWebAudit, WebAuditEntry, WebAuditTarget } from "../ipc/types";
import type { PanelView } from "./panel_tab";

/** What the tab says when the session fetched nothing and no listed action followed a fetch. */
export const WEB_EMPTY = "This session has not fetched web content.";
/**
 * What the tab says when no call is listed but older records were not read: the 1,000 newest
 * action records of all sessions held none of this session's web calls.
 */
export const WEB_NONE_RECENT = "No recent web fetch was found in this session.";
/**
 * The note for `truncated`. Rust also sets it when the record window was full, which does not
 * prove an older call exists, so it says "may".
 */
export const WEB_TRUNCATED = "Older calls may not be shown.";
/** What the tab says while no session is open. */
export const WEB_NO_SESSION = "Open a session to review its web fetches.";
/** The note when another writer holds the model action journal. */
export const WEB_BUSY = "The model action journal is in use. Refresh to read it again.";
/** The note when the model action journal fails validation; nothing is listed. */
export const WEB_INVALID =
  "The model action journal failed validation, so this view lists nothing. Check it with the trusted CLI.";
/** The line above the list: what the entries are. */
export const WEB_INTRO =
  "Each web fetch, then every action the model made after the first successful fetch, newest first. Web content can steer the model, so review these.";

const OUTCOME_LABELS: Record<WebAuditEntry["outcome"], string> = {
  succeeded: "succeeded",
  failed: "failed",
  uncertain: "outcome uncertain",
  unfinished: "unfinished",
  refused: "refused",
};

const AUTHORIZATION_LABELS = {
  standing_allow: "allowed by the workspace setting",
  approval: "approved by the operator",
} as const;

/** A tool's wire name as words: `read_file` reads "read file"; the fetch tool reads "web fetch". */
export function toolLabel(entry: WebAuditEntry): string {
  return entry.is_fetch ? "web fetch" : entry.tool.replaceAll("_", " ");
}

/** One target as text. A withheld value is known only by its length, never by a name. */
export function targetText(target: WebAuditTarget): string {
  return target.kind === "text" ? target.text : `(withheld value, ${target.bytes} bytes)`;
}

/** A command as one line: the program, then each argument. Untrusted: show as text only. */
function commandLine(entry: WebAuditEntry): string {
  if (entry.program === null) {
    return "(command unavailable)";
  }
  let line = [entry.program, ...(entry.arguments ?? [])].join(" ");
  if (entry.arguments === null && entry.argument_count !== null && entry.argument_count > 0) {
    line += ` (${entry.argument_count} arguments not shown)`;
  }
  return line;
}

/** What the call acted on: its targets, or for a command its program and arguments. */
export function targetLines(entry: WebAuditEntry): string[] {
  let lines: string[];
  if (entry.tool === "run_command") {
    lines = [commandLine(entry)];
  } else if (entry.targets.length > 0) {
    lines = entry.targets.map(targetText);
  } else {
    lines = ["(no target recorded)"];
  }
  if (entry.text_truncated) {
    lines = [...lines.slice(0, -1), `${lines[lines.length - 1]} (shortened)`];
  }
  return lines;
}

/** How a call ended, or why it was refused. Every value is plain text. */
export function outcomeText(entry: WebAuditEntry): string {
  const label = OUTCOME_LABELS[entry.outcome];
  return entry.reason === null ? label : `${label}: ${entry.reason.replaceAll("_", " ")}`;
}

/** How a call was authorised. A refused call ran under no authority. */
export function authorizationText(entry: WebAuditEntry): string {
  return entry.authorization === null ? "not run" : AUTHORIZATION_LABELS[entry.authorization];
}

/** The time since a call, such as "5m ago", with "now" and dates left as they read. */
function sinceText(iso: string, now: number): string {
  const age = formatTimeAgo(iso, now);
  return /^\d+[mhdw]$/.test(age) ? `${age} ago` : age;
}

function renderEntry(entry: WebAuditEntry, now: number): HTMLElement {
  const tooltip = neutralise(`${formatAbsolute(entry.time)}, call ${entry.call_id}`);
  const meta = el(
    "div",
    { class: "web-meta" },
    el("span", { class: "web-time", text: sinceText(entry.time, now) }),
    el("span", { class: `web-outcome web-outcome-${entry.outcome}`, text: outcomeText(entry) }),
    el("span", { class: "web-authorization", text: authorizationText(entry) }),
  );
  meta.title = tooltip;
  const targets = el(
    "div",
    { class: "web-targets" },
    ...targetLines(entry).map((line) => el("div", { class: "web-target", text: line })),
  );
  return el(
    "li",
    { class: entry.is_fetch ? "web-entry web-entry-fetch" : "web-entry web-entry-after" },
    el(
      "div",
      { class: "web-heading" },
      el("span", {
        class: entry.is_fetch ? "chip web-tool web-tool-fetch" : "chip web-tool",
        text: toolLabel(entry),
      }),
    ),
    targets,
    meta,
  );
}

/**
 * The "Web" view: the open session's `web_fetch` calls and every action the model made after
 * the first successful fetch (ADR-0098). Read-only: it fetches, approves, and runs nothing, and
 * every URL, path, and command is inert text, never a link. It reads when it becomes visible, on
 * **Refresh**, and after a turn ends; it never polls.
 */
export function createWebTab(): PanelView {
  let sessionId: string | null = null;
  let latest = 0;
  let disposed = false;
  let current: SessionWebAudit | null = null;

  const status = el("p", { class: "web-status" });
  const intro = el("p", { class: "web-intro", text: WEB_INTRO });
  const list = el("ul", {
    class: "web-list",
    attrs: { "aria-label": "Web fetches and later actions" },
  });
  const notes = el("div", { class: "web-notes" });
  const refreshButton = button("Refresh", () => refresh(), { class: "web-refresh" });
  const element = el(
    "section",
    { class: "web-tab", attrs: { "aria-label": "Web fetches" } },
    el("div", { class: "web-toolbar" }, status, refreshButton),
    intro,
    notes,
    list,
  );

  function showError(reason: unknown): void {
    const error = toIpcError(reason);
    if (error.code === "busy") {
      replaceChildren(notes, el("p", { class: "web-note", text: WEB_BUSY }));
    } else if (error.code === "invalid_journal") {
      current = null;
      render();
      replaceChildren(notes, el("p", { class: "error", text: WEB_INVALID }));
    } else {
      replaceChildren(notes, el("p", { class: "error", text: `${error.message} (${error.code})` }));
    }
  }

  function render(): void {
    const data = current;
    refreshButton.disabled = sessionId === null;
    intro.hidden = data === null || data.entries.length === 0;
    if (sessionId === null || data === null) {
      replaceChildren(list);
      replaceChildren(notes);
      status.textContent = sessionId === null ? WEB_NO_SESSION : "";
      return;
    }
    // With nothing listed, `truncated` can only mean the record window was full, so the empty
    // text must not claim the session never fetched.
    status.textContent =
      data.entries.length > 0 ? "" : data.truncated ? WEB_NONE_RECENT : WEB_EMPTY;
    const now = Date.now();
    replaceChildren(list, ...data.entries.map((entry) => renderEntry(entry, now)));
    replaceChildren(
      notes,
      ...(data.truncated ? [el("p", { class: "web-note", text: WEB_TRUNCATED })] : []),
    );
  }

  /** Reads the list. The newest request wins; an answer for another session is dropped. */
  async function load(): Promise<void> {
    const wanted = sessionId;
    if (wanted === null || disposed) {
      return;
    }
    latest += 1;
    const ticket = latest;
    try {
      const data = await api.sessionWebAudit(wanted);
      if (ticket !== latest || wanted !== sessionId || disposed) {
        return;
      }
      current = data;
      render();
    } catch (reason) {
      if (ticket !== latest || wanted !== sessionId || disposed) {
        return;
      }
      showError(reason);
    }
  }

  function refresh(): void {
    void load();
  }

  render();

  return {
    element,
    showSession(next) {
      if (disposed || next === sessionId) {
        return;
      }
      sessionId = next;
      latest += 1;
      current = null;
      render();
      if (next !== null) {
        refresh();
      }
    },
    refresh,
    dispose() {
      disposed = true;
      latest += 1;
    },
  };
}
