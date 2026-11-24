import { el } from "../dom";
import { type IconName, iconButton } from "../ui/icons";
import { createTabs, tabIdFor } from "../ui/tabs";

/** One tab of the right-hand panel: add an entry to the list `createSidePanel` receives. */
export interface SideTab {
  id: string;
  /** The tab's visible label. */
  label: string;
  /** The icon the collapsed rail shows for the tab. */
  icon: IconName;
  /** The tab's content. It lives in a scrolling area above the panel's fixed footer. */
  content: HTMLElement;
  /**
   * Called when the tab becomes visible (the panel is expanded and the tab selected) or stops
   * being visible. Use it to load lazily and to stop work nobody can see.
   */
  onVisible?(visible: boolean): void;
}

export interface SidePanelHandlers {
  onCollapse(): void;
  onExpand(): void;
}

export interface SidePanel {
  element: HTMLElement;
  /**
   * The fixed footer: below the scrolling tab content, outside it, and visible on every tab.
   * It is empty until a feature puts something in it.
   */
  footer: HTMLElement;
  setCollapsed(collapsed: boolean): void;
  /** Selects a tab, without expanding the panel. */
  select(id: string): void;
  /** The selected tab's id. */
  current(): string;
}

/**
 * Builds the right-hand panel: a tab strip with a collapse button, the selected tab's content in a
 * scrolling area, and a fixed footer. Collapsed, it is a rail with one icon per tab; an icon
 * expands the panel on its tab. The selection lasts until the window closes. Keys follow the ARIA
 * tabs pattern through `createTabs`. A tab's content must be built from `el` and text nodes.
 */
export function createSidePanel(specs: readonly SideTab[], handlers: SidePanelHandlers): SidePanel {
  const first = specs[0];
  if (first === undefined) {
    throw new Error("A side panel needs at least one tab.");
  }
  let collapsed = true;
  let selected = first.id;
  const shown = new Map<string, boolean>();

  const panes = new Map<string, HTMLElement>();
  for (const spec of specs) {
    const panelId = `side-pane-${spec.id}`;
    const pane = el(
      "div",
      {
        class: "side-pane",
        attrs: { id: panelId, role: "tabpanel", "aria-labelledby": tabIdFor(panelId) },
      },
      spec.content,
    );
    pane.hidden = spec.id !== selected;
    panes.set(spec.id, pane);
  }

  const tabs = createTabs(
    "Side panel tabs",
    specs.map((spec) => ({ id: spec.id, label: spec.label, panelId: `side-pane-${spec.id}` })),
    selected,
    (id) => choose(id),
  );
  tabs.element.classList.add("side-tabs");

  /** Tells each tab whether it is now visible, only when that changed. */
  function syncVisibility(): void {
    for (const spec of specs) {
      const visible = !collapsed && spec.id === selected;
      if ((shown.get(spec.id) ?? false) !== visible) {
        shown.set(spec.id, visible);
        spec.onVisible?.(visible);
      }
    }
  }

  function choose(id: string): void {
    selected = id;
    for (const [paneId, pane] of panes) {
      pane.hidden = paneId !== id;
    }
    syncVisibility();
  }

  const footer = el("div", { class: "side-footer" });
  const full = el(
    "div",
    { class: "terminal-full" },
    el(
      "div",
      { class: "terminal-header" },
      tabs.element,
      iconButton("panelRight", "Collapse side panel", handlers.onCollapse),
    ),
    el("div", { class: "side-panes" }, ...panes.values()),
    footer,
  );

  const railButtons = new Map<string, HTMLButtonElement>();
  const rail = el("div", { class: "panel-rail terminal-rail" });
  for (const spec of specs) {
    const railButton = iconButton(spec.icon, `Show ${spec.label.toLowerCase()} panel`, () => {
      tabs.select(spec.id);
      choose(spec.id);
      handlers.onExpand();
    });
    railButtons.set(spec.id, railButton);
    rail.append(railButton);
  }

  const element = el(
    "aside",
    { class: "terminal-panel", attrs: { id: "terminal-panel", "aria-label": "Side panel" } },
    full,
    rail,
  );
  full.hidden = collapsed;

  return {
    element,
    footer,
    current: () => selected,
    select(id) {
      if (!panes.has(id)) {
        return;
      }
      tabs.select(id);
      choose(id);
    },
    setCollapsed(next) {
      if (collapsed === next && rail.hidden === !next) {
        return;
      }
      const hadFocus = (next ? full : rail).contains(document.activeElement);
      collapsed = next;
      full.hidden = next;
      rail.hidden = !next;
      syncVisibility();
      if (hadFocus) {
        const target = next
          ? railButtons.get(selected)
          : full.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]');
        target?.focus();
      }
    },
  };
}
