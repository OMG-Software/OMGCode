import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { IpcFailure } from "../ipc/errors";
import type { GitIndexState, GitPathView, GitStatusView, GitWorktreeState } from "../ipc/types";
import { branchText } from "./status_footer";

/** What a right-hand panel tab provides to the main window. */
export interface PanelView {
  element: HTMLElement;
  /** Shows the given session's data, or clears the view with `null`. */
  showSession(sessionId: string | null): void;
  /** Reads the open session's data again. */
  refresh(): void;
  dispose(): void;
}

const INDEX_LETTER: Readonly<Record<GitIndexState, [letter: string, tip: string]>> = {
  added: ["A", "Staged: added"],
  modified: ["M", "Staged: modified"],
  deleted: ["D", "Staged: deleted"],
  renamed: ["R", "Staged: renamed"],
  type_change: ["T", "Staged: type changed"],
};

const WORKTREE_LETTER: Readonly<Record<GitWorktreeState, [letter: string, tip: string]>> = {
  modified: ["M", "Changed: modified, not staged"],
  deleted: ["D", "Changed: deleted, not staged"],
  untracked: ["?", "Untracked: not in the repository"],
  conflicted: ["U", "Conflicted: merge conflict not resolved"],
};

interface Row {
  path: string;
  letter: string;
  tip: string;
}

interface Group {
  title: string;
  rows: Row[];
}

/** Splits the changed paths into the four groups. A path may be both staged and changed. */
export function groupPaths(paths: readonly GitPathView[]): Group[] {
  const staged: Row[] = [];
  const changed: Row[] = [];
  const untracked: Row[] = [];
  const conflicted: Row[] = [];
  for (const entry of paths) {
    if (entry.index !== null) {
      const [letter, tip] = INDEX_LETTER[entry.index];
      staged.push({ path: entry.path, letter, tip });
    }
    if (entry.worktree !== null) {
      const [letter, tip] = WORKTREE_LETTER[entry.worktree];
      const row = { path: entry.path, letter, tip };
      if (entry.worktree === "untracked") {
        untracked.push(row);
      } else if (entry.worktree === "conflicted") {
        conflicted.push(row);
      } else {
        changed.push(row);
      }
    }
  }
  return [
    { title: "Staged", rows: staged },
    { title: "Changed", rows: changed },
    { title: "Untracked", rows: untracked },
    { title: "Conflicted", rows: conflicted },
  ].filter((group) => group.rows.length > 0);
}

/**
 * The Git status tab (ADR-0085): the open session's workspace branch and its changed paths,
 * read-only. Rust reads the repository in process with no program run and resolves the
 * workspace from the session's record; this view sends only the session id. Every name is
 * shown as a text node. Tooltips hold fixed text only, never a path.
 */
export function createGitTab(): PanelView {
  const branch = el("span", { class: "git-branch" });
  const refreshButton = button("Refresh", () => view.refresh(), { class: "git-refresh" });
  const body = el("div", { class: "git-body" });
  const element = el(
    "div",
    { class: "git-tab" },
    el("div", { class: "git-head" }, branch, refreshButton),
    body,
  );

  let sessionId: string | null = null;
  // Each read carries the counter it started with; a stale answer is dropped.
  let reads = 0;
  let disposed = false;

  function message(text: string, role?: "alert"): HTMLElement {
    return el("p", {
      class: role === undefined ? "git-message muted" : "git-message git-error",
      text,
      attrs: role === undefined ? {} : { role },
    });
  }

  function clear(text: string): void {
    replaceChildren(branch);
    replaceChildren(body, message(text));
    refreshButton.disabled = sessionId === null;
  }

  function render(status: GitStatusView): void {
    if (status.state === "not_repository") {
      replaceChildren(branch);
      replaceChildren(body, message("Not a Git repository."));
      return;
    }
    replaceChildren(branch, branchText(status.branch));
    if (status.state === "unavailable") {
      const reason = status.note === null ? "" : `: ${status.note}`;
      replaceChildren(body, message(`Git status unavailable${reason}`, "alert"));
      return;
    }
    if (status.paths.length === 0) {
      replaceChildren(body, message("No changes."));
      return;
    }
    const sections = groupPaths(status.paths).map((group) =>
      el(
        "section",
        { class: "git-group" },
        el("h3", { class: "git-group-title", text: `${group.title} (${group.rows.length})` }),
        el(
          "ul",
          { class: "git-list" },
          ...group.rows.map((row) =>
            el(
              "li",
              { class: "git-row" },
              el("span", {
                class: "git-state",
                text: row.letter,
                attrs: { title: row.tip, "aria-label": row.tip },
              }),
              el("span", { class: "git-path", text: row.path }),
            ),
          ),
        ),
      ),
    );
    replaceChildren(
      body,
      ...sections,
      status.truncated
        ? message(`Showing the first ${status.paths.length} changed paths; more changed.`)
        : null,
    );
  }

  async function load(): Promise<void> {
    const id = sessionId;
    const read = ++reads;
    if (id === null) {
      clear("No session open.");
      return;
    }
    refreshButton.disabled = false;
    try {
      const status = await api.workspaceGitStatus(id);
      if (!disposed && read === reads) {
        render(status);
      }
    } catch (error) {
      if (!disposed && read === reads) {
        replaceChildren(branch);
        const reason =
          error instanceof IpcFailure ? error.message : "The status could not be read.";
        replaceChildren(body, message(`Git status unavailable: ${reason}`, "alert"));
      }
    }
  }

  const view: PanelView = {
    element,
    showSession(id) {
      sessionId = id;
      if (id !== null) {
        // The old session's list must not stay up while the new one loads.
        replaceChildren(branch);
        replaceChildren(body, message("Reading status."));
      }
      void load();
    },
    refresh() {
      if (sessionId !== null) {
        void load();
      }
    },
    dispose() {
      disposed = true;
    },
  };
  clear("No session open.");
  return view;
}
