import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { SearchHit, SessionSummary, SessionTree, TreeSort, TreeView } from "../ipc/types";
import { SEARCH_LIMIT, SEARCH_MAX_CHARS } from "./model";
import { enableTreeKeys, renderHits, renderTree } from "./tree";

export interface SidebarHandlers {
  onOpen(session: SessionSummary, folder: string, seq: number | null): void;
  onNewSession(): void;
  onShowApprovals(): void;
  onShowSettings(): void;
}

export interface Sidebar {
  element: HTMLElement;
  /** Reloads the tree, or the search results when a query is active. */
  refresh(): Promise<void>;
  setSelected(sessionId: string | null): void;
  setPending(count: number): void;
  dispose(): void;
}

const SEARCH_DEBOUNCE_MS = 250;

/** Builds the sidebar: header controls, search box, and session tree or hit list. */
export function createSidebar(handlers: SidebarHandlers): Sidebar {
  let view: TreeView = "active";
  let sort: TreeSort = "updated";
  let descending = true;
  let query = "";
  let selectedId: string | null = null;
  let lastTree: SessionTree | null = null;
  let debounce: ReturnType<typeof setTimeout> | null = null;
  let generation = 0;
  const collapsed = new Set<string>();

  const body = el("div", { class: "sidebar-body" });
  const approvalsBadge = el("span", { class: "badge", text: "0" });
  approvalsBadge.hidden = true;
  const approvalsButton = button("Approvals", handlers.onShowApprovals, { class: "ghost" });
  approvalsButton.appendChild(approvalsBadge);
  const settingsButton = button("Settings", handlers.onShowSettings, { class: "ghost" });

  const activeButton = button("Active", () => setView("active"), { class: "seg" });
  const archivedButton = button("Archived", () => setView("archived"), { class: "seg" });
  const sortSelect = el("select", { attrs: { "aria-label": "Sort sessions" } });
  for (const [value, label] of [
    ["updated", "Updated"],
    ["created", "Created"],
    ["title", "Title"],
  ] as const) {
    const option = el("option", { text: label });
    option.value = value;
    sortSelect.appendChild(option);
  }
  const directionButton = button("Newest first", () => {
    descending = !descending;
    syncControls();
    void refresh();
  });
  const search = el("input", {
    class: "search",
    attrs: { type: "search", "aria-label": "Search sessions", placeholder: "Search sessions" },
  });
  search.maxLength = SEARCH_MAX_CHARS;

  function syncControls(): void {
    activeButton.setAttribute("aria-pressed", String(view === "active"));
    archivedButton.setAttribute("aria-pressed", String(view === "archived"));
    directionButton.textContent = descending ? "Descending" : "Ascending";
    directionButton.setAttribute("aria-pressed", String(descending));
  }

  function setView(next: TreeView): void {
    view = next;
    syncControls();
    void refresh();
  }

  sortSelect.addEventListener("change", () => {
    sort = sortSelect.value as TreeSort;
    void refresh();
  });

  search.addEventListener("input", () => {
    if (debounce !== null) {
      clearTimeout(debounce);
    }
    debounce = setTimeout(() => {
      debounce = null;
      query = search.value.trim().slice(0, SEARCH_MAX_CHARS);
      void refresh();
    }, SEARCH_DEBOUNCE_MS);
  });
  search.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && search.value !== "") {
      event.preventDefault();
      search.value = "";
      query = "";
      void refresh();
    }
  });

  const header = el(
    "div",
    { class: "sidebar-header" },
    el(
      "div",
      { class: "sidebar-title-row" },
      el("h1", { text: "OMGCode" }),
      button("New session", handlers.onNewSession, { class: "primary" }),
    ),
    el(
      "div",
      { class: "seg-group", attrs: { role: "group", "aria-label": "View" } },
      activeButton,
      archivedButton,
    ),
    el("div", { class: "sort-row" }, sortSelect, directionButton),
    search,
    el("div", { class: "window-row" }, approvalsButton, settingsButton),
  );
  const element = el(
    "nav",
    { class: "sidebar", attrs: { "aria-label": "Sessions" } },
    header,
    body,
  );
  syncControls();

  function showError(reason: unknown): void {
    const error = toIpcError(reason);
    replaceChildren(body, el("p", { class: "error", text: `${error.message} (${error.code})` }));
  }

  function openHit(hit: SearchHit): void {
    selectedId = hit.session.session_id;
    handlers.onOpen(hit.session, hit.folder_display, hit.seq);
  }

  function drawTree(tree: SessionTree): void {
    const { element: treeElement, notes } = renderTree(tree, {
      selectedId,
      collapsed,
      emptyMessage:
        view === "archived"
          ? "No archived sessions."
          : "No sessions yet. Create one here, or with the OMGCode CLI.",
    });
    enableTreeKeys(treeElement, {
      activate(item) {
        if (item.dataset.kind === "group") {
          setExpanded(item.dataset.groupId ?? "", item.getAttribute("aria-expanded") !== "true");
          return;
        }
        const id = item.dataset.sessionId;
        for (const group of tree.groups) {
          const session = group.sessions.find((s) => s.session_id === id);
          if (session !== undefined) {
            selectedId = session.session_id;
            drawTree(tree);
            focusItem("sessionId", session.session_id);
            handlers.onOpen(session, group.folder_display, null);
          }
        }
      },
      toggle: setExpanded,
    });
    replaceChildren(body, tree.groups.length > 0 ? treeElement : null, ...notes);
  }

  function focusItem(key: "sessionId" | "groupId", value: string): void {
    const items = Array.from(body.querySelectorAll<HTMLElement>("[role=treeitem]"));
    const item = items.find((candidate) => candidate.dataset[key] === value);
    if (item !== undefined) {
      item.tabIndex = 0;
      item.focus();
    }
  }

  function setExpanded(groupId: string, expand: boolean): void {
    if (expand) {
      collapsed.delete(groupId);
    } else {
      collapsed.add(groupId);
    }
    if (lastTree !== null) {
      drawTree(lastTree);
      focusItem("groupId", groupId);
    }
  }

  async function refresh(): Promise<void> {
    const mine = ++generation;
    try {
      if (query === "") {
        const tree = await api.sessionTree(view, sort, descending);
        if (mine !== generation) {
          return;
        }
        lastTree = tree;
        drawTree(tree);
      } else {
        const result = await api.sessionSearch(query, view, SEARCH_LIMIT);
        if (mine !== generation) {
          return;
        }
        replaceChildren(body, renderHits(result.hits, result.truncated, openHit));
      }
    } catch (reason) {
      if (mine === generation) {
        showError(reason);
      }
    }
  }

  return {
    element,
    refresh,
    setSelected(sessionId) {
      selectedId = sessionId;
      if (query === "" && lastTree !== null) {
        drawTree(lastTree);
      }
    },
    setPending(count) {
      approvalsBadge.hidden = count <= 0;
      replaceChildren(approvalsBadge, String(count));
    },
    dispose() {
      if (debounce !== null) {
        clearTimeout(debounce);
      }
      generation += 1;
    },
  };
}
