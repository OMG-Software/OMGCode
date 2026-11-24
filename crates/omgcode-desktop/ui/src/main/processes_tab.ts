import { button, el, neutralise, replaceChildren } from "../dom";
import { formatAbsolute } from "../format";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { ProcessEntry, ProcessLimit, SessionProcesses } from "../ipc/types";
import { createPoller, type Poller } from "../poll";
import type { PanelView } from "./panel_tab";

/** What the tab says when a session has started no subprocess. */
export const PROCESSES_EMPTY = "No subprocess has run in this session.";
/**
 * What the tab says when no call is listed but older records were not read: the 1,000 newest
 * action records of all sessions held none of this session's calls.
 */
export const PROCESSES_NONE_RECENT = "No recent subprocess was found in this session.";
/**
 * The note for `truncated`. Rust also sets it when the record window was full, which does not
 * prove an older call exists, so it says "may".
 */
export const PROCESSES_TRUNCATED = "Older calls may not be shown.";
/** What the tab says while no session is open. */
export const PROCESSES_NO_SESSION = "Open a session to see its subprocesses.";
/** How often the list is read again while a call is running. */
export const PROCESSES_POLL_MS = 2000;

const STATE_LABELS: Record<ProcessEntry["state"], string> = {
  running: "running",
  finished: "finished",
  unfinished: "unfinished",
  refused: "refused",
};

const LIMIT_LABELS: Record<ProcessLimit, string> = {
  memory: "memory limit",
  processes: "process limit",
  cpu_time: "CPU time limit",
  disk: "disk growth limit",
};

const RESULT_LABELS: Record<string, string> = {
  deadline_exceeded: "deadline exceeded",
  cancelled: "cancelled",
  revoked: "access withdrawn",
  not_started: "did not start",
  supervision_failed: "supervision failed",
  cleanup_unconfirmed: "cleanup not confirmed",
};

/** The command as one line: the program, then each argument. Untrusted: show as text only. */
export function commandLine(entry: ProcessEntry): string {
  if (entry.program === null) {
    return "(command unavailable)";
  }
  const parts = [entry.program, ...(entry.arguments ?? [])];
  let line = parts.join(" ");
  if (entry.arguments === null && entry.argument_count !== null && entry.argument_count > 0) {
    line += ` (${entry.argument_count} arguments not shown)`;
  }
  if (entry.text_truncated) {
    line += " (shortened)";
  }
  return line;
}

/** A short duration: milliseconds under a second, then seconds, then minutes. */
export function formatDuration(ms: number): string {
  if (ms < 1000) {
    return `${ms} ms`;
  }
  const seconds = ms / 1000;
  if (seconds < 60) {
    return `${seconds.toFixed(1)} s`;
  }
  const minutes = Math.floor(seconds / 60);
  return `${minutes} min ${Math.floor(seconds % 60)} s`;
}

/** How a call ended, or why it did not run. Every value is plain text. */
export function outcomeText(entry: ProcessEntry): string {
  const { result } = entry;
  if (result !== null) {
    if (result.kind === "exited") {
      return `exit ${result.exit_code ?? "?"}`;
    }
    if (result.kind === "signalled") {
      return `signal ${result.signal ?? "?"}`;
    }
    if (result.kind === "limit_exceeded") {
      return `stopped: ${entry.limit === null ? "limit" : LIMIT_LABELS[entry.limit]}`;
    }
    return RESULT_LABELS[result.kind] ?? result.kind;
  }
  if (entry.state === "refused") {
    return entry.reason === null ? "refused" : `refused: ${entry.reason.replaceAll("_", " ")}`;
  }
  if (entry.state === "unfinished") {
    return "outcome unknown";
  }
  if (entry.state === "running") {
    return "running";
  }
  return entry.reason === null ? "finished" : `finished: ${entry.reason.replaceAll("_", " ")}`;
}

function renderEntry(entry: ProcessEntry): HTMLElement {
  const line = commandLine(entry);
  const meta: HTMLElement[] = [
    el("span", {
      class: `chip process-state process-state-${entry.state}`,
      text: STATE_LABELS[entry.state],
    }),
    el("span", { class: "process-time", text: formatAbsolute(entry.started_at) }),
  ];
  if (entry.duration_ms !== null) {
    meta.push(el("span", { class: "process-duration", text: formatDuration(entry.duration_ms) }));
  }
  meta.push(el("span", { class: "process-outcome", text: outcomeText(entry) }));
  if (entry.profile !== null) {
    meta.push(el("span", { class: "process-profile", text: entry.profile }));
  }
  const command = el("div", { class: "process-command", text: line });
  // Rust escaped the text already; the tooltip is an attribute, so it is neutralised here too.
  command.title = neutralise(line);
  return el(
    "li",
    { class: "process-entry" },
    command,
    el("div", { class: "process-meta" }, ...meta),
  );
}

/**
 * The "background tasks" view: the open session's subprocesses, which are its model
 * command-tool calls (ADR-0086). Read-only: it cancels, restarts, and runs nothing. While a
 * call is running it reads the list again every two seconds, and it stops when nothing runs.
 */
export function createProcessesTab(): PanelView {
  let sessionId: string | null = null;
  let latest = 0;
  let disposed = false;
  let current: SessionProcesses | null = null;

  const status = el("p", { class: "processes-status" });
  const list = el("ul", { class: "processes-list", attrs: { "aria-label": "Subprocesses" } });
  const notes = el("div", { class: "processes-notes" });
  const refreshButton = button("Refresh", () => refresh(), { class: "processes-refresh" });
  const element = el(
    "section",
    { class: "processes-tab", attrs: { "aria-label": "Background tasks" } },
    el("div", { class: "processes-toolbar" }, status, refreshButton),
    notes,
    list,
  );

  const poller: Poller = createPoller(
    async () => {
      await load();
    },
    PROCESSES_POLL_MS,
    (reason) => showError(reason),
  );

  function showError(reason: unknown): void {
    const error = toIpcError(reason);
    replaceChildren(notes, el("p", { class: "error", text: `${error.message} (${error.code})` }));
  }

  function render(): void {
    const data = current;
    refreshButton.disabled = sessionId === null;
    if (sessionId === null || data === null) {
      replaceChildren(list);
      replaceChildren(notes);
      status.textContent = sessionId === null ? PROCESSES_NO_SESSION : "";
      return;
    }
    // With nothing listed, `truncated` can only mean the record window was full, so the empty
    // text must not claim the session never ran anything.
    status.textContent =
      data.entries.length > 0 ? "" : data.truncated ? PROCESSES_NONE_RECENT : PROCESSES_EMPTY;
    replaceChildren(list, ...data.entries.map(renderEntry));
    const lines: HTMLElement[] = [];
    if (data.truncated) {
      lines.push(el("p", { class: "processes-note", text: PROCESSES_TRUNCATED }));
    }
    if (data.run_details === "unavailable") {
      lines.push(
        el("p", {
          class: "processes-note",
          text: "Run details are unavailable, so results and durations are not shown.",
        }),
      );
    }
    if (data.run_details === "busy") {
      lines.push(
        el("p", {
          class: "processes-note",
          text: "Run details are busy; results and durations appear on the next read.",
        }),
      );
    }
    replaceChildren(notes, ...lines);
  }

  function syncPolling(): void {
    const running = current?.entries.some((entry) => entry.state === "running") ?? false;
    if (running && !disposed) {
      poller.start();
    } else {
      poller.stop();
    }
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
      const data = await api.sessionProcesses(wanted);
      if (ticket !== latest || wanted !== sessionId || disposed) {
        return;
      }
      current = data;
      replaceChildren(notes);
      render();
      syncPolling();
    } catch (reason) {
      if (ticket !== latest || wanted !== sessionId || disposed) {
        return;
      }
      showError(reason);
      // A failed read keeps the last list and keeps polling only while it showed a running call.
      syncPolling();
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
      poller.stop();
      render();
      replaceChildren(notes);
      if (next !== null) {
        refresh();
      }
    },
    refresh,
    dispose() {
      disposed = true;
      latest += 1;
      poller.stop();
    },
  };
}
