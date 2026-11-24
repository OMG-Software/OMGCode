import { append, el, neutralise, replaceChildren, text } from "../dom";
import { formatAbsolute, formatTimeAgo } from "../format";
import type { SearchHit, SessionSummary, SessionTree, SnippetPart } from "../ipc/types";
import { statusLabel } from "./model";

export interface TreeOptions {
  selectedId: string | null;
  collapsed: ReadonlySet<string>;
  emptyMessage: string;
}

function statusChip(session: SessionSummary): HTMLElement | null {
  const label = statusLabel(session.status);
  return label === null ? null : el("span", { class: `chip chip-${session.status}`, text: label });
}

/** Attribute that holds a row's RFC 3339 update time so the age can be drawn again in place. */
const UPDATED_ATTR = "data-updated";

/** Rewrites every session row's age against `now` without rebuilding the tree. */
export function refreshTimes(root: HTMLElement, now: number): void {
  for (const node of root.querySelectorAll<HTMLElement>(`.tree-time[${UPDATED_ATTR}]`)) {
    node.textContent = formatTimeAgo(node.getAttribute(UPDATED_ATTR), now);
  }
}

function sessionRow(session: SessionSummary, selected: boolean, now: number): HTMLElement {
  const item = el("li", {
    class: "tree-session",
    attrs: {
      role: "treeitem",
      tabindex: "-1",
      "data-kind": "session",
      "data-session-id": session.session_id,
      "aria-selected": selected ? "true" : "false",
      // An attribute does not go through the text-node builder, so the untrusted title is
      // neutralised here, as the conversation header's tooltip is.
      title: `${neutralise(session.title)}
Updated ${formatAbsolute(session.updated_at)}`,
    },
  });
  const time = el("span", {
    class: "tree-time",
    text: formatTimeAgo(session.updated_at, now),
  });
  if (session.updated_at !== null) {
    time.setAttribute(UPDATED_ATTR, session.updated_at);
  }
  const top = el(
    "div",
    { class: "row" },
    el("span", { class: "tree-title clamp-1", text: session.title }),
    statusChip(session),
    time,
  );
  append(item, top);
  if (session.status === "invalid" && session.invalid_code !== null) {
    append(item, el("div", { class: "row-meta row-error", text: session.invalid_code }));
  }
  return item;
}

/** Builds the grouped session tree. Arrow-key navigation is added by `enableTreeKeys`. */
export function renderTree(
  tree: SessionTree,
  options: TreeOptions,
  now: number = Date.now(),
): { element: HTMLElement; notes: HTMLElement[] } {
  const root = el("ul", {
    class: "tree",
    attrs: { role: "tree", "aria-label": "Sessions" },
  });
  for (const group of tree.groups) {
    const collapsed = options.collapsed.has(group.group_id);
    const header = el(
      "div",
      { class: "row group-row" },
      el("span", { class: "caret", text: collapsed ? "+" : "-" }),
      el("span", { class: "tree-title clamp-1", text: group.folder_display }),
      el("span", { class: "count", text: String(group.sessions.length) }),
    );
    const children = el("ul", { class: "tree-children", attrs: { role: "group" } });
    children.hidden = collapsed;
    for (const session of group.sessions) {
      children.appendChild(sessionRow(session, session.session_id === options.selectedId, now));
    }
    const item = el(
      "li",
      {
        class: "tree-group",
        attrs: {
          role: "treeitem",
          tabindex: "-1",
          "data-kind": "group",
          "data-group-id": group.group_id,
          "aria-expanded": collapsed ? "false" : "true",
        },
      },
      header,
      children,
    );
    root.appendChild(item);
  }
  const notes: HTMLElement[] = [];
  if (tree.groups.length === 0) {
    notes.push(el("p", { class: "empty", text: options.emptyMessage }));
  }
  if (tree.skipped_entries > 0) {
    notes.push(
      el("p", {
        class: "note",
        text: `${tree.skipped_entries} entries in session storage were skipped.`,
      }),
    );
  }
  return { element: root, notes };
}

/** The tree's rows that are not inside a collapsed group, in order. */
export function visibleItems(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>('[role="treeitem"]')).filter(
    (item) => item.parentElement?.closest('[aria-expanded="false"]') === null,
  );
}

/** Gives the tree a roving tabindex and arrow, Home, End, Enter and Space handling. */
export function enableTreeKeys(
  root: HTMLElement,
  handlers: {
    activate(item: HTMLElement): void;
    toggle(groupId: string, expand: boolean): void;
  },
): void {
  const items = visibleItems(root);
  const current = items.find((i) => i.getAttribute("aria-selected") === "true") ?? items[0];
  if (current !== undefined) {
    current.tabIndex = 0;
  }
  const focusItem = (item: HTMLElement | undefined): void => {
    if (item === undefined) {
      return;
    }
    for (const other of root.querySelectorAll<HTMLElement>('[role="treeitem"]')) {
      other.tabIndex = -1;
    }
    item.tabIndex = 0;
    item.focus();
  };
  root.addEventListener("keydown", (event) => {
    const target = (event.target as HTMLElement).closest<HTMLElement>('[role="treeitem"]');
    if (target === null) {
      return;
    }
    const list = visibleItems(root);
    const index = list.indexOf(target);
    const isGroup = target.dataset.kind === "group";
    const groupId = target.dataset.groupId ?? "";
    switch (event.key) {
      case "ArrowDown":
        focusItem(list[index + 1]);
        break;
      case "ArrowUp":
        focusItem(list[index - 1]);
        break;
      case "Home":
        focusItem(list[0]);
        break;
      case "End":
        focusItem(list[list.length - 1]);
        break;
      case "ArrowRight":
        if (isGroup && target.getAttribute("aria-expanded") === "false") {
          handlers.toggle(groupId, true);
        } else if (isGroup) {
          focusItem(list[index + 1]);
        }
        break;
      case "ArrowLeft":
        if (isGroup && target.getAttribute("aria-expanded") === "true") {
          handlers.toggle(groupId, false);
        } else if (!isGroup) {
          focusItem(target.parentElement?.closest<HTMLElement>('[role="treeitem"]') ?? undefined);
        }
        break;
      case "Enter":
      case " ":
        handlers.activate(target);
        break;
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
  });
  root.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement).closest<HTMLElement>('[role="treeitem"]');
    if (target !== null) {
      handlers.activate(target);
    }
  });
}

function snippetNodes(parts: SnippetPart[]): Node[] {
  return parts.map((part) => (part.matched ? el("mark", { text: part.text }) : text(part.text)));
}

/** Builds the flat list of search hits. */
export function renderHits(
  hits: SearchHit[],
  truncated: boolean,
  onOpen: (hit: SearchHit) => void,
): HTMLElement {
  const list = el("ul", { class: "hits" });
  if (hits.length === 0) {
    replaceChildren(list, el("li", { class: "empty", text: "No matches." }));
  }
  for (const hit of hits) {
    const open = el("button", { class: "hit" });
    open.type = "button";
    open.addEventListener("click", () => onOpen(hit));
    append(
      open,
      el("span", { class: "hit-title clamp-1", text: hit.session.title }),
      el("span", { class: "hit-meta clamp-1", text: hit.folder_display }),
      el("span", { class: "hit-meta", text: hit.role }),
      el("span", { class: "hit-snippet clamp-3" }, ...snippetNodes(hit.snippet)),
    );
    list.appendChild(el("li", {}, open));
  }
  const wrapper = el("div", {}, list);
  if (truncated) {
    append(wrapper, el("p", { class: "note", text: "More results exist; refine the search." }));
  }
  return wrapper;
}
