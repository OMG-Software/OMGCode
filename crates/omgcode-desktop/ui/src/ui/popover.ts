import { type Child, el } from "../dom";

export interface PopoverOptions {
  /** An extra class on the `<details>`: `align-end` and `open-up` position the panel. */
  class?: string;
  /** Accessible name for the panel when the summary text alone is unclear. */
  label?: string;
}

export interface Popover {
  element: HTMLDetailsElement;
  /** The visible summary; its children can be replaced, for example to show a count. */
  summary: HTMLElement;
  panel: HTMLElement;
  close(): void;
  dispose(): void;
}

/**
 * A small menu built on `<details>`, so it opens and closes without script and works from the
 * keyboard. Escape, or a click outside it, closes it. It holds controls only; untrusted text
 * inside it still goes through the text-node builder.
 */
export function createPopover(
  summaryText: string,
  options: PopoverOptions,
  ...content: Child[]
): Popover {
  const summary = el("summary", { text: summaryText });
  const panel = el("div", { class: "popover-panel" }, ...content);
  if (options.label !== undefined) {
    panel.setAttribute("aria-label", options.label);
  }
  const element = el(
    "details",
    { class: options.class === undefined ? "popover" : `popover ${options.class}` },
    summary,
    panel,
  );

  function close(): void {
    element.open = false;
  }

  const onPointer = (event: Event): void => {
    if (element.open && !element.contains(event.target as Node)) {
      close();
    }
  };
  const onKey = (event: KeyboardEvent): void => {
    if (event.key === "Escape" && element.open) {
      close();
      summary.focus();
    }
  };
  document.addEventListener("pointerdown", onPointer);
  element.addEventListener("keydown", onKey);

  return {
    element,
    summary,
    panel,
    close,
    dispose() {
      document.removeEventListener("pointerdown", onPointer);
      element.removeEventListener("keydown", onKey);
    },
  };
}
