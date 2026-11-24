import { el } from "../dom";

export interface SegmentedOptions<T extends string> {
  /** Accessible name of the group. */
  label: string;
  /** The choices in display order, as [value, visible label]. */
  choices: ReadonlyArray<readonly [T, string]>;
  /** The value shown as chosen, or `null` when none is, such as for a mixed set. */
  value: T | null;
  /** Called with a newly chosen value; never with the current one. */
  onChoose: (value: T) => void;
  /** An extra class on the group, such as `access`. */
  class?: string;
}

/**
 * A single-choice control drawn as joined buttons, with radio-group semantics: one tab stop,
 * arrow keys move between choices, and Space or Enter chooses. It never changes its own state;
 * the caller re-renders it from recorded state, so a refused change cannot leave it showing a
 * value the core did not accept.
 */
export function segmented<T extends string>(options: SegmentedOptions<T>): HTMLElement {
  const group = el("div", {
    class: options.class === undefined ? "segmented" : `segmented ${options.class}`,
    attrs: { role: "radiogroup", "aria-label": options.label },
  });
  const buttons: HTMLButtonElement[] = [];
  for (const [value, label] of options.choices) {
    const chosen = value === options.value;
    const choice = el("button", {
      class: "seg-choice",
      text: label,
      attrs: {
        role: "radio",
        "aria-checked": String(chosen),
        "data-value": value,
        tabindex: chosen ? "0" : "-1",
      },
    });
    choice.type = "button";
    choice.addEventListener("click", () => {
      if (value !== options.value && !choice.disabled) {
        options.onChoose(value);
      }
    });
    buttons.push(choice);
    group.append(choice);
  }
  if (!buttons.some((b) => b.tabIndex === 0) && buttons[0] !== undefined) {
    buttons[0].tabIndex = 0;
  }
  group.addEventListener("keydown", (event) => {
    const step = arrowStep(event.key);
    if (step === 0) {
      return;
    }
    const index = buttons.indexOf(event.target as HTMLButtonElement);
    if (index < 0) {
      return;
    }
    event.preventDefault();
    const next = buttons[(index + step + buttons.length) % buttons.length];
    next?.focus();
  });
  return group;
}

/** -1, 1, or 0 for a key that does not move. Focus moves; choosing needs Space, Enter, or a click. */
export function arrowStep(key: string): -1 | 0 | 1 {
  if (key === "ArrowLeft" || key === "ArrowUp") {
    return -1;
  }
  if (key === "ArrowRight" || key === "ArrowDown") {
    return 1;
  }
  return 0;
}
