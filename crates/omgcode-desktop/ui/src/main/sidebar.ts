import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { toIpcError } from "../ipc/errors";
import type { SearchHit, SessionSummary, SessionTree, TreeSort, TreeView } from "../ipc/types";
import { brandMark, iconButton } from "../ui/icons";
import { createPopover } from "../ui/popover";
import { segmented } from "../ui/segmented";
import { SEARCH_LIMIT, SEARCH_MAX_CHARS } from "./model";
import { enableTreeKeys, refreshTimes, renderHits, renderTree, visibleItems } from "./tree";

export interface SidebarHandlers {
  onOpen(session: SessionSummary, folder: string, seq: number | null): void;
  onNewSession(): void;
  onShowApprovals(): void;
  onShowSettings(): void;
  /** Asks the layout to collapse the panel to its rail of icons. */
  onCollapse(): void;
  /** Asks the layout to show the full panel again. */
  onExpand(): void;
}

export interface Sidebar {
  element: HTMLElement;
  /** Reloads the tree, or the search results when a query is active. */
  refresh(): Promise<void>;
  setSelected(sessionId: string | null): void;
  setPending(count: number): void;
  /** Shows the rail of icons instead of the full panel, or the full panel again. */
  setCollapsed(collapsed: boolean): void;
  dispose(): void;
}

const SEARCH_DEBOUNCE_MS = 250;
/** How often the rows' ages are drawn again, so "5m" does not go stale. */
const TIME_REFRESH_MS = 30_000;

/**
 * Builds the sidebar: a title row with New session, the search box with a View menu (active or
 * archived, sort, direction), the session tree or hit list, and the Approvals and Settings rows.
 * Collapsed, it shows a rail of icons for the same actions instead (ADR-0072).
 */
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
  const approvalsButton = button("Approvals", handlers.onShowApprovals, {
    class: "ghost nav-row",
  });
  approvalsButton.appendChild(approvalsBadge);
  const settingsButton = button("Settings", handlers.onShowSettings, { class: "ghost nav-row" });

  const viewHolder = el("div", { class: "view-choice" });
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
  const directionButton = button("Descending", () => {
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
    // The segmented control never changes its own state; it is drawn again from `view`.
    replaceChildren(
      viewHolder,
      segmented<TreeView>({
        label: "View",
        choices: [
          ["active", "Active"],
          ["archived", "Archived"],
        ],
        value: view,
        onChoose: setView,
      }),
    );
    directionButton.textContent = descending ? "Descending" : "Ascending";
    directionButton.setAttribute("aria-pressed", String(descending));
  }

  function setView(next: TreeView): void {
    view = next;
    syncControls();
    viewHolder.querySelector<HTMLElement>('[aria-checked="true"]')?.focus();
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

  const viewMenu = createPopover(
    "View",
    { class: "align-end", label: "View options" },
    el("div", { class: "field" }, el("span", { text: "Show" }), viewHolder),
    el("label", { class: "field" }, el("span", { text: "Sort by" }), sortSelect),
    el("div", { class: "field" }, el("span", { text: "Order" }), directionButton),
  );

  const header = el(
    "div",
    { class: "sidebar-header" },
    el(
      "div",
      { class: "sidebar-title-row" },
      el("h1", { class: "brand-title" }, brandMark(), "OMGCode"),
      el(
        "div",
        { class: "sidebar-title-actions" },
        button("New session", handlers.onNewSession, { class: "primary" }),
        iconButton("panelLeft", "Collapse sessions panel", handlers.onCollapse),
      ),
    ),
    el("div", { class: "search-row" }, search, viewMenu.element),
  );
  const full = el(
    "div",
    { class: "sidebar-full" },
    header,
    body,
    el("div", { class: "sidebar-footer" }, approvalsButton, settingsButton),
  );

  // The rail repeats the panel's actions as icons. Search and Sessions expand the panel first.
  const railBadge = el("span", { class: "badge rail-badge", text: "0" });
  railBadge.hidden = true;
  const railApprovals = iconButton("approvals", "Approvals", handlers.onShowApprovals);
  railApprovals.appendChild(railBadge);
  const railExpand = iconButton("panelLeft", "Expand sessions panel", handlers.onExpand);
  const rail = el(
    "div",
    { class: "panel-rail sidebar-rail" },
    railExpand,
    iconButton("plus", "New session", handlers.onNewSession),
    iconButton("search", "Search sessions", () => {
      handlers.onExpand();
      search.focus();
    }),
    iconButton("sessions", "Sessions", () => {
      handlers.onExpand();
      focusCurrentItem();
    }),
    el("div", { class: "rail-spacer" }),
    railApprovals,
    iconButton("settings", "Settings", handlers.onShowSettings),
  );
  rail.hidden = true;

  const element = el(
    "nav",
    { class: "sidebar", attrs: { id: "sessions-panel", "aria-label": "Sessions" } },
    full,
    rail,
  );
  syncControls();

  /**
   * Focuses the open session's row, or the collapsed group that holds it, or the tree's first
   * visible row, or the search box. A row inside a collapsed group cannot take focus. The focused
   * row becomes the tree's one tab stop.
   */
  function focusCurrentItem(): void {
    const rows = visibleItems(body);
    const selected = body.querySelector<HTMLElement>('[role=treeitem][aria-selected="true"]');
    const item =
      rows.find((row) => row === selected) ??
      rows.find((row) => selected !== null && row.contains(selected)) ??
      rows[0];
    if (item === undefined) {
      search.focus();
      return;
    }
    for (const row of body.querySelectorAll<HTMLElement>("[role=treeitem]")) {
      row.tabIndex = -1;
    }
    item.tabIndex = 0;
    item.focus();
  }

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

  const timeTimer = setInterval(() => refreshTimes(body, Date.now()), TIME_REFRESH_MS);

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
      railBadge.hidden = count <= 0;
      replaceChildren(railBadge, String(count));
      const label = count > 0 ? `Approvals, ${count} waiting` : "Approvals";
      railApprovals.setAttribute("aria-label", label);
      railApprovals.title = label;
    },
    setCollapsed(collapsed) {
      if (rail.hidden === !collapsed) {
        return;
      }
      // Focus inside the view being hidden would fall to the page; move it to the other view.
      const hadFocus = (collapsed ? full : rail).contains(document.activeElement);
      full.hidden = collapsed;
      rail.hidden = !collapsed;
      if (hadFocus && collapsed) {
        railExpand.focus();
      } else if (hadFocus) {
        focusCurrentItem();
      }
    },
    dispose() {
      viewMenu.dispose();
      clearInterval(timeTimer);
      if (debounce !== null) {
        clearTimeout(debounce);
      }
      generation += 1;
    },
  };
}
