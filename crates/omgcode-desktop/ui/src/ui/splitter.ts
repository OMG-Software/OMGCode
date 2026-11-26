/**
 * A movable divider between two sections of a window (ADR-0101). It follows the WAI-ARIA window
 * splitter pattern, as the main window's column separators do (ADR-0072): drag it, or focus it and
 * use the arrow keys (16 pixels, 64 with Shift), Home, and End. A double click restores the
 * section's starting size. It never collapses a section.
 *
 * One section is sized: the splitter writes its size, a whole number of pixels, to a custom
 * property on that section through `setSizeProperty`, and the section's stylesheet reads it. The
 * neighbour on the other side gives way, down to its own minimum. Sizes last until the window
 * closes; nothing stores them.
 */

import { clearSizeProperty, type SizeProperty, setSizeProperty } from "./size_property";

export const KEY_STEP = 16;
export const LARGE_KEY_STEP = 64;

/**
 * The most measuring passes one fit makes. While the neighbour is squeezed to nothing, each pass
 * narrows the section by the neighbour's whole minimum (96 pixels or more for every divider), and
 * one more pass settles it, so no divider needs more than 12; the cap only guards against a
 * stylesheet that ignores the size.
 */
const MAX_FIT_PASSES = 32;

/**
 * A vertical splitter is a vertical line that moves left and right and sets a width; a horizontal
 * one moves up and down and sets a height.
 */
export type SplitOrientation = "vertical" | "horizontal";

/** The size rules of a section along one orientation. */
export interface SplitAxis {
  /** The custom property on the section that its stylesheet reads. */
  readonly property: SizeProperty;
  readonly min: number;
  readonly max: number;
  /**
   * The size the section starts at, or `null` to keep the stylesheet's own size, such as the
   * content's height, until the splitter first moves.
   */
  readonly initial: number | null;
  /** The least size the neighbour keeps when the section grows. */
  readonly neighbourMin: number;
}

export interface SplitterOptions {
  /** Completes the accessible name "Resize the …". */
  label: string;
  /** The section the splitter sizes. It must have an `id` for `aria-controls`. */
  section: HTMLElement;
  /** The section on the other side, which gives way. */
  neighbour: HTMLElement;
  /** Whether the splitter comes after the section (right of it or below it) or before it. */
  placement: "after" | "before";
  /** The rules for a vertical splitter. Give this, `horizontal`, or both. */
  vertical?: SplitAxis;
  /** The rules for a horizontal splitter. */
  horizontal?: SplitAxis;
  /**
   * Which orientation applies now, read again whenever the window or the neighbour changes size,
   * for a layout that stacks its sections in a narrow window. Without it, the one axis given.
   */
  orientation?: () => SplitOrientation;
}

export interface Splitter {
  /** The separator element, to place between the section and its neighbour. */
  readonly element: HTMLElement;
  /** The orientation in use. */
  orientation(): SplitOrientation;
  /** The size the splitter set along the orientation in use, or `null` while the stylesheet's applies. */
  size(): number | null;
  /** Reads the orientation again and narrows the section to the room its neighbour leaves. */
  fit(): void;
  dispose(): void;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/** Builds a splitter and starts following the window's and the neighbour's size. */
export function createSplitter(options: SplitterOptions): Splitter {
  const { section, neighbour, placement } = options;
  const fallback: SplitOrientation = options.vertical !== undefined ? "vertical" : "horizontal";
  if (options[fallback] === undefined) {
    throw new Error("A splitter needs at least one axis.");
  }
  if (section.id === "") {
    throw new Error("A splitter's section needs an id.");
  }

  const element = document.createElement("div");
  element.className = "splitter";
  element.tabIndex = 0;
  element.setAttribute("role", "separator");
  element.setAttribute("aria-label", `Resize the ${options.label}`);
  element.setAttribute("aria-controls", section.id);
  element.title = "Drag to resize. Double-click restores the size.";

  /** The size the splitter set for each orientation; absent while the stylesheet's applies. */
  const sizes = new Map<SplitOrientation, number>();
  for (const orientation of ["vertical", "horizontal"] as const) {
    const initial = options[orientation]?.initial ?? null;
    if (initial !== null) {
      sizes.set(orientation, initial);
    }
  }

  function wanted(): SplitOrientation {
    const asked = options.orientation?.() ?? fallback;
    return options[asked] !== undefined ? asked : fallback;
  }

  let current = wanted();
  /** The drag in progress, and the pointer that started it: no other pointer moves or ends it. */
  let drag: { pointer: number; origin: number; startSize: number } | null = null;

  function axis(): SplitAxis {
    // `current` is always an orientation with an axis: see `wanted`.
    return options[current] ?? (options[fallback] as SplitAxis);
  }

  function extent(target: HTMLElement): number {
    const rect = target.getBoundingClientRect();
    return Math.round(current === "vertical" ? rect.width : rect.height);
  }

  /**
   * The largest size the section can have now: its maximum, or less where the neighbour would go
   * below its own minimum. A section that is not laid out (hidden, or in a test document) has no
   * measured size, so only its own limits apply; this keeps a hidden section's size for when it
   * shows again.
   */
  function upperLimit(): number {
    const rules = axis();
    const own = extent(section);
    const other = extent(neighbour);
    if (own <= 0) {
      return rules.max;
    }
    return clamp(Math.floor(own + other - rules.neighbourMin), rules.min, rules.max);
  }

  /** The size a move starts from: the one set, else the section's laid-out size. */
  function startSize(): number {
    const set = sizes.get(current);
    if (set !== undefined) {
      return set;
    }
    const shown = extent(section);
    return shown > 0 ? shown : axis().min;
  }

  function render(): void {
    const rules = axis();
    element.setAttribute("aria-orientation", current);
    element.classList.toggle("splitter-vertical", current === "vertical");
    element.classList.toggle("splitter-horizontal", current === "horizontal");
    const set = sizes.get(current);
    if (set !== undefined) {
      setSizeProperty(section, rules.property, set);
    }
    element.setAttribute("aria-valuemin", String(rules.min));
    element.setAttribute("aria-valuemax", String(upperLimit()));
    const shown = set ?? extent(section);
    if (shown > 0) {
      element.setAttribute("aria-valuenow", String(shown));
      const dimension = current === "vertical" ? "wide" : "tall";
      element.setAttribute("aria-valuetext", `${shown} pixels ${dimension}`);
    } else {
      element.removeAttribute("aria-valuenow");
      element.removeAttribute("aria-valuetext");
    }
  }

  function setSize(requested: number): void {
    const rules = axis();
    sizes.set(current, clamp(Math.round(requested), rules.min, upperLimit()));
    render();
  }

  /** Back to the starting size, or to the stylesheet's own size when there is none. */
  function restore(): void {
    const rules = axis();
    if (rules.initial === null) {
      sizes.delete(current);
      clearSizeProperty(section, rules.property);
      render();
      return;
    }
    setSize(rules.initial);
  }

  /** Growing the section moves the splitter away from it. */
  function growth(delta: number): number {
    return placement === "after" ? delta : -delta;
  }

  function onKey(event: KeyboardEvent): void {
    const step = event.shiftKey ? LARGE_KEY_STEP : KEY_STEP;
    const [back, forward] =
      current === "vertical"
        ? (["ArrowLeft", "ArrowRight"] as const)
        : (["ArrowUp", "ArrowDown"] as const);
    let next: number;
    switch (event.key) {
      case back:
        next = startSize() + growth(-step);
        break;
      case forward:
        next = startSize() + growth(step);
        break;
      case "Home":
        next = axis().min;
        break;
      case "End":
        next = axis().max;
        break;
      default:
        return;
    }
    event.preventDefault();
    setSize(next);
  }

  function position(event: PointerEvent): number {
    return current === "vertical" ? event.clientX : event.clientY;
  }

  function onPointerMove(event: PointerEvent): void {
    if (drag !== null && event.pointerId === drag.pointer) {
      setSize(drag.startSize + growth(position(event) - drag.origin));
    }
  }

  function onPointerEnd(event: PointerEvent): void {
    if (drag !== null && event.pointerId === drag.pointer) {
      endDrag();
    }
  }

  /** Ends the drag whichever pointer owns it, as disposal and an orientation change need. */
  function endDrag(): void {
    if (drag === null) {
      return;
    }
    drag = null;
    element.classList.remove("dragging");
    document.documentElement.classList.remove("resizing-columns", "resizing-rows");
    document.removeEventListener("pointermove", onPointerMove);
    document.removeEventListener("pointerup", onPointerEnd);
    document.removeEventListener("pointercancel", onPointerEnd);
  }

  function startDrag(event: PointerEvent): void {
    if (event.button !== 0) {
      return;
    }
    event.preventDefault();
    endDrag();
    drag = { pointer: event.pointerId, origin: position(event), startSize: startSize() };
    element.classList.add("dragging");
    document.documentElement.classList.add(
      current === "vertical" ? "resizing-columns" : "resizing-rows",
    );
    element.focus();
    // Listening on the document follows a pointer that leaves the thin splitter mid-drag.
    document.addEventListener("pointermove", onPointerMove);
    document.addEventListener("pointerup", onPointerEnd);
    document.addEventListener("pointercancel", onPointerEnd);
  }

  /**
   * Narrows the section until its neighbour keeps its minimum or the section reaches its own. A
   * neighbour squeezed to nothing shows only part of the shortfall, so each pass writes the size
   * and measures again: a window that shrinks past the neighbour in one step can take several.
   */
  function fitToRoom(): void {
    const next = wanted();
    if (next !== current) {
      endDrag();
      current = next;
    }
    for (let pass = 0; pass < MAX_FIT_PASSES; pass += 1) {
      const set = sizes.get(current);
      const limit = upperLimit();
      if (set === undefined || set <= limit) {
        break;
      }
      sizes.set(current, limit);
      setSizeProperty(section, axis().property, limit);
    }
    render();
  }

  element.addEventListener("keydown", onKey);
  element.addEventListener("pointerdown", startDrag);
  element.addEventListener("dblclick", restore);
  // A size the stylesheet chose is read when the splitter gains focus, so it is announced.
  element.addEventListener("focus", render);

  let frame: number | null = null;
  /**
   * Fits on the next frame. Narrowing the section inside a size-observer callback would resize
   * the observed neighbour again in the same frame, which the browser reports as an observer loop.
   */
  function fitSoon(): void {
    if (typeof requestAnimationFrame !== "function") {
      fitToRoom();
      return;
    }
    if (frame === null) {
      frame = requestAnimationFrame(() => {
        frame = null;
        fitToRoom();
      });
    }
  }

  // The window's resize event, and the neighbour's own size for a change that fires none, such as
  // a panel opening beside it. Fitting only narrows the section, so it settles after one pass.
  window.addEventListener("resize", fitToRoom);
  const observer = typeof ResizeObserver === "function" ? new ResizeObserver(fitSoon) : null;
  observer?.observe(neighbour);
  render();

  return {
    element,
    orientation: () => current,
    size: () => sizes.get(current) ?? null,
    fit: fitToRoom,
    dispose() {
      endDrag();
      observer?.disconnect();
      if (frame !== null && typeof cancelAnimationFrame === "function") {
        cancelAnimationFrame(frame);
      }
      frame = null;
      window.removeEventListener("resize", fitToRoom);
    },
  };
}
