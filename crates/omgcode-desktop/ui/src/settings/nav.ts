import { button, el, replaceChildren } from "../dom";
import type { SectionId } from "./context";

/** The sections in the order the navigation lists them. */
export const SECTIONS: ReadonlyArray<readonly [SectionId, string]> = [
  ["model", "Model"],
  ["workspaces", "Workspaces"],
  ["advanced", "Advanced"],
];

export interface Nav {
  element: HTMLElement;
  /** Marks the section shown, with `aria-current="page"`. */
  select(id: SectionId): void;
  setRevision(revision: number | null): void;
  /** Disables every control while a change waits. */
  setDisabled(value: boolean): void;
}

/** The left column: the section buttons, then Refresh and the settings revision. */
export function createNav(
  initial: SectionId,
  onSelect: (id: SectionId) => void,
  onRefresh: () => void,
): Nav {
  const buttons = new Map<SectionId, HTMLButtonElement>();
  const list = el("div", { class: "nav-list" });
  for (const [id, label] of SECTIONS) {
    const choice = button(label, () => onSelect(id), { class: "nav-item" });
    buttons.set(id, choice);
    list.append(choice);
  }
  const refresh = button("Refresh", onRefresh);
  const revision = el("span", { class: "revision" });
  const element = el(
    "nav",
    { class: "settings-nav", attrs: { "aria-label": "Settings sections" } },
    el("h1", { text: "Settings" }),
    list,
    el("div", { class: "nav-foot" }, refresh, revision),
  );

  function select(id: SectionId): void {
    for (const [key, choice] of buttons) {
      if (key === id) {
        choice.setAttribute("aria-current", "page");
      } else {
        choice.removeAttribute("aria-current");
      }
    }
  }
  select(initial);

  return {
    element,
    select,
    setRevision(value) {
      replaceChildren(revision, value === null ? "" : `Revision ${value}`);
    },
    setDisabled(value) {
      refresh.disabled = value;
      for (const choice of buttons.values()) {
        choice.disabled = value;
      }
    },
  };
}
