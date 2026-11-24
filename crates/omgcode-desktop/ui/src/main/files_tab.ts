import { el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import type { WorkspaceEntry, WorkspaceListing } from "../ipc/types";
import { icon, iconButton } from "../ui/icons";
import type { SideTab } from "./side_panel";

/** The most rows the tree draws, however many directories are open. */
export const FILES_MAX_ROWS = 5000;

export const FILES_NO_SESSION = "Open a session to see its workspace files.";
export const FILES_EMPTY = "This folder is empty.";
export const FILES_LOADING = "Loading…";

export interface FilesTab extends SideTab {
  /** Follows the open session: clears the tree, and loads it again once visible. */
  setSession(sessionId: string | null): void;
  /**
   * Lists the workspace again, keeping the directories that are open. A hidden tab reads nothing
   * now and lists again when it is next shown, as the other session tabs do after a turn.
   */
  refresh(): void;
}

function pathKey(path: readonly string[]): string {
  return JSON.stringify(path);
}

function failureText(reason: unknown): string {
  const detail = reason instanceof Error && reason.message !== "" ? ` ${reason.message}` : "";
  return `The folder could not be listed.${detail}`;
}

/**
 * The Files tab: a lazily expanded tree of the open session's workspace. Each directory is listed
 * when first opened, through `workspace_entries`, which names a path as relative components and
 * lets Rust find the workspace from the session (ADR-0083). Names are untrusted text and go
 * through `el` only. A name that is not valid Unicode is shown but never sent back.
 */
export function createFilesTab(): FilesTab {
  let sessionId: string | null = null;
  let generation = 0;
  let visible = false;
  let loaded = false;
  let expanded = new Set<string>();

  const message = el("p", { class: "files-message", text: FILES_NO_SESSION });
  const host = el("div", { class: "files-tree" });
  const refreshButton = iconButton("refresh", "Refresh files", () => refresh());
  refreshButton.disabled = true;
  const content = el(
    "div",
    { class: "files-tab" },
    el(
      "div",
      { class: "files-toolbar" },
      el("span", { class: "files-title", text: "Workspace" }),
      refreshButton,
    ),
    message,
    host,
  );

  function showMessage(text: string | null): void {
    message.hidden = text === null;
    replaceChildren(message, text);
  }

  /** Lists one directory, or returns `null` when the session changed meanwhile. */
  async function list(path: string[]): Promise<WorkspaceListing | null> {
    const mine = generation;
    const id = sessionId;
    if (id === null) {
      return null;
    }
    const listing = await api.workspaceEntries(id, path);
    return mine === generation ? listing : null;
  }

  function rowCount(): number {
    return host.querySelectorAll("li").length;
  }

  function entryRow(entry: WorkspaceEntry, parent: string[]): HTMLLIElement {
    const item = el("li", { class: "files-item" });
    const path = [...parent, entry.name];
    const nameNode = el("span", { class: "files-name", text: entry.name });
    // Rust says which directories it would list next (too deep, or a name such as a Unix one with
    // a backslash, is refused); a lossy name is never sent back whatever the flag says.
    if (entry.kind === "directory" && entry.openable && !entry.lossy) {
      const toggle = el("button", {
        class: "files-row files-dir",
        attrs: { "aria-expanded": "false" },
      });
      toggle.type = "button";
      const chevron = el("span", { class: "files-chevron" }, icon("chevronRight"));
      toggle.append(chevron, icon("folder"), nameNode);
      const children = el("div", { class: "files-children" });
      children.hidden = true;
      let open = false;
      // Bumped on every open and close, so a listing that lands after the
      // folder closed, or after a newer open, draws nothing.
      let opening = 0;
      const setOpen = async (next: boolean): Promise<void> => {
        open = next;
        opening += 1;
        const mine = opening;
        toggle.setAttribute("aria-expanded", String(next));
        chevron.replaceChildren(icon(next ? "chevronDown" : "chevronRight"));
        children.hidden = !next;
        if (next) {
          expanded.add(pathKey(path));
          await fill(children, path, () => mine === opening);
        } else {
          expanded.delete(pathKey(path));
          // Opening lists the folder again, so a closed folder keeps no rows:
          // hidden rows would spend the row budget that open folders draw from.
          replaceChildren(children);
        }
      };
      toggle.addEventListener("click", () => void setOpen(!open));
      item.append(toggle, children);
      if (expanded.has(pathKey(path))) {
        void setOpen(true);
      }
      return item;
    }
    const row = el("div", { class: "files-row" });
    if (entry.kind === "directory") {
      row.classList.add("files-dir");
      row.append(el("span", { class: "files-chevron" }), icon("folder"), nameNode);
      row.title = entry.lossy
        ? "The name is not valid Unicode, so this folder cannot be opened here."
        : "This folder's name or depth cannot be used as a path here, so it is not opened.";
    } else {
      row.append(el("span", { class: "files-chevron" }), icon("file"), nameNode);
      if (entry.kind === "other") {
        row.title = "A link or special entry; it is not opened.";
      }
      if (entry.lossy) {
        row.title = "The name is not valid Unicode.";
      }
    }
    item.append(row);
    return item;
  }

  /**
   * Lists `path` into `target`, with loading, empty, truncated, and error text. `current` says
   * whether the answer is still wanted when it lands; the session check is in `list`. Resolves
   * `true` only when a listing, possibly empty, was drawn.
   */
  async function fill(
    target: HTMLElement,
    path: string[],
    current: () => boolean = () => true,
  ): Promise<boolean> {
    replaceChildren(target, el("p", { class: "files-note", text: FILES_LOADING }));
    let listing: WorkspaceListing | null;
    try {
      listing = await list(path);
    } catch (reason) {
      if (target.isConnected && current()) {
        replaceChildren(
          target,
          el("p", { class: "files-note files-error", text: failureText(reason) }),
        );
      }
      return false;
    }
    if (listing === null || !current()) {
      return false;
    }
    if (listing.entries.length === 0) {
      replaceChildren(target, el("p", { class: "files-note", text: FILES_EMPTY }));
      return true;
    }
    const entries = el("ul", { class: "files-list" });
    let drawn = 0;
    for (const entry of listing.entries) {
      if (rowCount() + entries.childElementCount >= FILES_MAX_ROWS) {
        break;
      }
      entries.append(entryRow(entry, path));
      drawn += 1;
    }
    replaceChildren(target, entries);
    // Only when something was left out: by the broker's bound or the row cap.
    // The count is what this folder drew, not what the broker returned.
    if (listing.truncated || drawn < listing.entries.length) {
      target.append(
        el("p", {
          class: "files-note",
          text: `Only the first ${drawn} entries are shown.`,
        }),
      );
    }
    return true;
  }

  async function loadRoot(): Promise<void> {
    if (sessionId === null) {
      return;
    }
    loaded = true;
    showMessage(null);
    const mine = generation;
    const drawn = await fill(host, []);
    // A failed root listing keeps its error text, but showing the tab again lists it again, as
    // the other session tabs read afresh on show. A newer load owns `loaded` itself.
    if (!drawn && mine === generation) {
      loaded = false;
    }
  }

  function refresh(): void {
    if (sessionId === null) {
      return;
    }
    if (!visible) {
      // Stale now, so showing the tab lists it again.
      loaded = false;
      return;
    }
    generation += 1;
    void loadRoot();
  }

  return {
    id: "files",
    label: "Files",
    icon: "folder",
    content,
    onVisible(now) {
      visible = now;
      if (now && !loaded && sessionId !== null) {
        void loadRoot();
      }
    },
    setSession(next) {
      if (next === sessionId) {
        return;
      }
      sessionId = next;
      generation += 1;
      loaded = false;
      expanded = new Set();
      replaceChildren(host);
      refreshButton.disabled = next === null;
      showMessage(next === null ? FILES_NO_SESSION : null);
      if (visible && next !== null) {
        void loadRoot();
      }
    },
    refresh,
  };
}
