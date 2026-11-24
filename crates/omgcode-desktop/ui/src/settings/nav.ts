import { button, el, neutralise, replaceChildren } from "../dom";
import { brandMark, icon } from "../ui/icons";
import type { SectionId } from "./context";

/** The sections in the order the navigation lists them. */
export const SECTIONS: ReadonlyArray<readonly [SectionId, string]> = [
  ["model", "Model"],
  ["workspaces", "Workspaces"],
  ["advanced", "Advanced"],
];

/** What the navigation shows; the window keeps the state and passes it on every draw. */
export interface NavState {
  section: SectionId;
  /** Whether a workspace, not the Workspaces overview, is the page shown. */
  workspacePage: boolean;
  workspaceId: string | null;
  /** Whether the workspace list under Workspaces is open. */
  expanded: boolean;
  workspaces: ReadonlyArray<{ id: string; label: string }>;
}

export interface NavHandlers {
  onSelect(id: SectionId): void;
  onSelectWorkspace(id: string): void;
  onToggleWorkspaces(): void;
  onRefresh(): void;
}

export interface Nav {
  element: HTMLElement;
  /** Marks the page shown with `aria-current="page"` and lists the workspaces. */
  update(state: NavState): void;
  setRevision(revision: number | null): void;
  /** Disables every control while a change waits. */
  setDisabled(value: boolean): void;
}

const CHILDREN_ID = "nav-workspaces";

/**
 * The left column: the section buttons, with the workspaces as a collapsible group under
 * Workspaces, then Refresh and the settings revision.
 */
export function createNav(handlers: NavHandlers): Nav {
  const buttons = new Map<SectionId, HTMLButtonElement>();
  const list = el("div", { class: "nav-list" });
  const children = el("div", { class: "nav-children", attrs: { id: CHILDREN_ID } });
  const toggle = button("", handlers.onToggleWorkspaces, {
    class: "nav-toggle",
    attrs: { "aria-label": "Workspaces list", "aria-controls": CHILDREN_ID },
  });
  toggle.title = "Workspaces list";
  toggle.append(icon("chevronDown"));
  let disabled = false;
  for (const [id, label] of SECTIONS) {
    const choice = button(label, () => handlers.onSelect(id), { class: "nav-item" });
    buttons.set(id, choice);
    if (id === "workspaces") {
      list.append(el("div", { class: "nav-group-head" }, choice, toggle), children);
    } else {
      list.append(choice);
    }
  }
  const refresh = button("Refresh", handlers.onRefresh);
  const revision = el("span", { class: "revision" });
  const element = el(
    "nav",
    { class: "settings-nav", attrs: { "aria-label": "Settings sections" } },
    el("h1", { class: "brand-title" }, brandMark(), "Settings"),
    list,
    el("div", { class: "nav-foot" }, refresh, revision),
  );

  function update(state: NavState): void {
    for (const [key, choice] of buttons) {
      const current = key === state.section && !(key === "workspaces" && state.workspacePage);
      if (current) {
        choice.setAttribute("aria-current", "page");
      } else {
        choice.removeAttribute("aria-current");
      }
    }
    // The children are rebuilt, so a focused one gets focus back on its replacement.
    const active = document.activeElement;
    const focused = active instanceof HTMLElement ? active.dataset.workspaceId : undefined;
    const items = state.workspaces.map((workspace) => {
      const item = button(workspace.label, () => handlers.onSelectWorkspace(workspace.id), {
        class: "nav-child",
        // An attribute skips the text-node neutralising, so the folder text is neutralised here.
        attrs: { title: neutralise(workspace.label) },
      });
      item.dataset.workspaceId = workspace.id;
      item.disabled = disabled;
      const shown =
        state.section === "workspaces" && state.workspacePage && state.workspaceId === workspace.id;
      if (shown) {
        item.setAttribute("aria-current", "page");
      }
      return item;
    });
    replaceChildren(children, ...items);
    children.hidden = !state.expanded || items.length === 0;
    toggle.hidden = items.length === 0;
    toggle.setAttribute("aria-expanded", String(state.expanded));
    if (focused !== undefined) {
      items.find((item) => item.dataset.workspaceId === focused)?.focus();
    }
  }

  return {
    element,
    update,
    setRevision(value) {
      replaceChildren(revision, value === null ? "" : `Revision ${value}`);
    },
    setDisabled(value) {
      disabled = value;
      refresh.disabled = value;
      toggle.disabled = value;
      for (const choice of buttons.values()) {
        choice.disabled = value;
      }
      for (const child of Array.from(children.querySelectorAll("button"))) {
        child.disabled = value;
      }
    },
  };
}
