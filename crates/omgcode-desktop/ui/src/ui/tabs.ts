import { el, replaceChildren } from "../dom";
import { arrowStep } from "./segmented";

export interface TabSpec<T extends string> {
  id: T;
  label: string;
  /**
   * The document id of the panel this tab shows. The tab gets `aria-controls` for it and the id
   * `<panelId>-tab`, which the caller gives the panel as `aria-labelledby`.
   */
  panelId?: string;
}

/** The id a tab carries when its spec names a panel. */
export function tabIdFor(panelId: string): string {
  return `${panelId}-tab`;
}

export interface Tabs<T extends string> {
  element: HTMLElement;
  /** The selected tab. */
  current(): T;
  /** Selects a tab without calling `onSelect`. */
  select(id: T): void;
  /** Replaces a tab's visible label, for example to show a count. */
  setLabel(id: T, label: string): void;
}

/**
 * A tab strip with tablist semantics. Arrow keys move focus and select, as the ARIA tabs
 * pattern describes. The caller owns the panels and shows the one `onSelect` names; the
 * selection lives in memory only, because the frontend uses no web storage.
 */
export function createTabs<T extends string>(
  label: string,
  specs: ReadonlyArray<TabSpec<T>>,
  initial: T,
  onSelect: (id: T) => void,
): Tabs<T> {
  const element = el("div", { class: "tabs", attrs: { role: "tablist", "aria-label": label } });
  const buttons = new Map<T, HTMLButtonElement>();
  let selected = initial;

  function sync(): void {
    for (const [id, tab] of buttons) {
      const on = id === selected;
      tab.setAttribute("aria-selected", String(on));
      tab.tabIndex = on ? 0 : -1;
    }
  }

  for (const spec of specs) {
    const tab = el("button", {
      class: "tab",
      text: spec.label,
      attrs: { role: "tab", "data-tab": spec.id },
    });
    tab.type = "button";
    if (spec.panelId !== undefined) {
      tab.id = tabIdFor(spec.panelId);
      tab.setAttribute("aria-controls", spec.panelId);
    }
    tab.addEventListener("click", () => choose(spec.id));
    buttons.set(spec.id, tab);
    element.append(tab);
  }

  function choose(id: T): void {
    if (id === selected) {
      return;
    }
    selected = id;
    sync();
    onSelect(id);
  }

  element.addEventListener("keydown", (event) => {
    const step = arrowStep(event.key);
    if (step === 0) {
      return;
    }
    const ids = specs.map((s) => s.id);
    const index = ids.indexOf(selected);
    const next = ids[(index + step + ids.length) % ids.length];
    if (next === undefined) {
      return;
    }
    event.preventDefault();
    choose(next);
    buttons.get(next)?.focus();
  });

  sync();
  return {
    element,
    current: () => selected,
    select(id) {
      selected = id;
      sync();
    },
    setLabel(id, text) {
      const tab = buttons.get(id);
      if (tab !== undefined && tab.textContent !== text) {
        replaceChildren(tab, text);
      }
    },
  };
}
