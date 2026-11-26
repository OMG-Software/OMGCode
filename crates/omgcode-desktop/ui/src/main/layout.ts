/**
 * The main window's columns: the sessions panel, the conversation, and the terminal panel, with
 * a separator between each pair (ADR-0072). A separator follows the WAI-ARIA window splitter
 * pattern: drag it, or focus it and use the arrow keys, Home, End, and Enter.
 *
 * Each panel's width is a whole number of pixels this module computes and clamps, written as a
 * custom property on the shell through `setSizeProperty`, the frontend's one style write
 * (ADR-0101). Widths last until the window closes; nothing stores them. The other windows'
 * dividers, which never collapse a section, use `createSplitter` with the same keys.
 */

import { setSizeProperty } from "../ui/size_property";
import { KEY_STEP, LARGE_KEY_STEP } from "../ui/splitter";

export { KEY_STEP, LARGE_KEY_STEP };

export interface PanelLimits {
  readonly min: number;
  readonly max: number;
  readonly initial: number;
  readonly collapsed: boolean;
}

export const LEFT_PANEL: PanelLimits = { min: 200, max: 480, initial: 260, collapsed: false };
// The terminal panel opens collapsed: it has no backend yet, and the conversation keeps the room.
export const RIGHT_PANEL: PanelLimits = { min: 240, max: 720, initial: 360, collapsed: true };
/** The conversation's width that resizing a panel does not take from it. */
export const CENTER_MIN_WIDTH = 360;
/** Matches `--rail-width` in main.css. */
export const RAIL_WIDTH = 44;
/** Matches `--separator-width` in main.css. */
export const SEPARATOR_WIDTH = 1;
/** Dragging a panel this far below its minimum collapses it. */
export const COLLAPSE_SNAP = 80;

type Side = "left" | "right";

/** A panel that shows a full view or, collapsed, a rail of icons. */
export interface CollapsiblePanel {
  /** The panel; it must have an `id` for the separator's `aria-controls`. */
  element: HTMLElement;
  setCollapsed(collapsed: boolean): void;
}

export interface PanelControl {
  readonly separator: HTMLElement;
  isCollapsed(): boolean;
  setCollapsed(collapsed: boolean): void;
  /** The expanded width in pixels, kept while collapsed. */
  width(): number;
}

export interface ShellLayout {
  element: HTMLElement;
  left: PanelControl;
  right: PanelControl;
  /** Applies the limits again for the shell's current width. */
  fit(): void;
  dispose(): void;
}

interface PanelState {
  side: Side;
  limits: PanelLimits;
  label: string;
  panel: CollapsiblePanel;
  width: number;
  collapsed: boolean;
  separator: HTMLElement;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/** Builds the shell grid around three columns and starts following the window's width. */
export function createShellLayout(parts: {
  left: CollapsiblePanel;
  center: HTMLElement;
  right: CollapsiblePanel;
}): ShellLayout {
  const element = document.createElement("div");
  element.className = "shell";
  let drag: { state: PanelState; startX: number; startWidth: number } | null = null;

  function makeState(side: Side, panel: CollapsiblePanel, label: string): PanelState {
    const limits = side === "left" ? LEFT_PANEL : RIGHT_PANEL;
    const separator = document.createElement("div");
    separator.className = `separator separator-${side}`;
    separator.tabIndex = 0;
    separator.setAttribute("role", "separator");
    separator.setAttribute("aria-orientation", "vertical");
    separator.setAttribute("aria-label", `Resize the ${label}`);
    separator.setAttribute("aria-controls", panel.element.id);
    separator.setAttribute("aria-valuemin", String(limits.min));
    separator.title = "Drag to resize. Enter collapses or expands.";
    return {
      side,
      limits,
      label,
      panel,
      width: limits.initial,
      collapsed: limits.collapsed,
      separator,
    };
  }

  const left = makeState("left", parts.left, "sessions panel");
  const right = makeState("right", parts.right, "side panel");
  element.append(
    left.panel.element,
    left.separator,
    parts.center,
    right.separator,
    right.panel.element,
  );

  function other(state: PanelState): PanelState {
    return state === left ? right : left;
  }

  function trackWidth(state: PanelState): number {
    return state.collapsed ? RAIL_WIDTH : state.width;
  }

  /** The narrowest the other panel's track can become: its rail, or its minimum width. */
  function leastTrack(state: PanelState): number {
    return state.collapsed ? RAIL_WIDTH : state.limits.min;
  }

  /**
   * The widest this panel can be while the conversation keeps its minimum and the other panel
   * keeps the track `otherTrack`. Without a laid-out shell (hidden, or a test document) only the
   * panel's own limits apply.
   */
  function widthBeside(state: PanelState, otherTrack: number): number {
    const shellWidth = element.clientWidth;
    if (shellWidth <= 0) {
      return state.limits.max;
    }
    const room = shellWidth - CENTER_MIN_WIDTH - otherTrack - 2 * SEPARATOR_WIDTH;
    return clamp(Math.floor(room), state.limits.min, state.limits.max);
  }

  /** The room the other panel, at its current width, leaves this one. */
  function roomLeft(state: PanelState): number {
    return widthBeside(state, trackWidth(other(state)));
  }

  /** The widest this panel can grow: the other panel then narrows, down to its minimum. */
  function growLimit(state: PanelState): number {
    return widthBeside(state, leastTrack(other(state)));
  }

  function setWidthProperty(state: PanelState): void {
    // A clamped whole number of pixels (see above), through the frontend's one style write.
    setSizeProperty(element, `--${state.side}-width`, state.width);
  }

  function render(state: PanelState): void {
    element.classList.toggle(`${state.side}-collapsed`, state.collapsed);
    setWidthProperty(state);
    const { separator } = state;
    separator.setAttribute("aria-valuemax", String(growLimit(state)));
    separator.setAttribute(
      "aria-valuenow",
      String(state.collapsed ? state.limits.min : state.width),
    );
    separator.setAttribute(
      "aria-valuetext",
      state.collapsed ? "Collapsed" : `${state.width} pixels wide`,
    );
    state.panel.setCollapsed(state.collapsed);
  }

  /**
   * True when the expanded panels at their minimums, and the collapsed ones as rails, leave the
   * conversation less than its minimum. Unknown when the shell is not laid out: then false.
   */
  function minimumsOverflow(): boolean {
    const shellWidth = element.clientWidth;
    if (shellWidth <= 0) {
      return false;
    }
    return (
      shellWidth - leastTrack(left) - leastTrack(right) - 2 * SEPARATOR_WIDTH < CENTER_MIN_WIDTH
    );
  }

  /** Narrows an expanded panel to the room the conversation and the other panel leave it. */
  function shrinkToFit(state: PanelState): void {
    if (!state.collapsed && state.width > roomLeft(state)) {
      state.width = roomLeft(state);
    }
  }

  /**
   * Shows `state` at `requested` pixels, at most `limit(state)`. The other panel gives way: it
   * collapses when even both minimums leave the conversation too little, and otherwise narrows
   * to the room left. `state` cannot always give way itself, because it never goes below its
   * own minimum.
   */
  function expand(state: PanelState, requested: number, limit: (s: PanelState) => number): void {
    state.collapsed = false;
    const rest = other(state);
    if (!rest.collapsed && minimumsOverflow()) {
      rest.collapsed = true;
    }
    state.width = clamp(Math.round(requested), state.limits.min, limit(state));
    shrinkToFit(rest);
    render(state);
    render(rest);
  }

  /** A drag or key asks for a width: the panel may grow into the other, down to its minimum. */
  function setWidth(state: PanelState, requested: number): void {
    expand(state, requested, growLimit);
  }

  function setCollapsed(state: PanelState, collapsed: boolean): void {
    if (state.collapsed === collapsed) {
      return;
    }
    if (!collapsed) {
      // Expanding takes the free room first; the other panel narrows only as far as this one's
      // minimum needs.
      expand(state, state.width, roomLeft);
      return;
    }
    state.collapsed = true;
    render(state);
    render(other(state));
  }

  /** Growing a panel moves its separator away from the panel's own edge of the window. */
  function grow(state: PanelState, deltaX: number): number {
    return state.side === "left" ? deltaX : -deltaX;
  }

  function onKey(state: PanelState, event: KeyboardEvent): void {
    const step = event.shiftKey ? LARGE_KEY_STEP : KEY_STEP;
    const current = state.collapsed ? state.limits.min - step : state.width;
    let next: number | null = null;
    switch (event.key) {
      case "ArrowLeft":
        next = current + grow(state, -step);
        break;
      case "ArrowRight":
        next = current + grow(state, step);
        break;
      case "Home":
        next = state.limits.min;
        break;
      case "End":
        next = state.limits.max;
        break;
      case "Enter":
        event.preventDefault();
        setCollapsed(state, !state.collapsed);
        return;
      default:
        return;
    }
    event.preventDefault();
    if (state.collapsed && next < state.limits.min) {
      // A key that shrinks a collapsed panel leaves it collapsed.
      return;
    }
    setWidth(state, next);
  }

  function onPointerMove(event: PointerEvent): void {
    if (drag === null) {
      return;
    }
    const { state, startX, startWidth } = drag;
    const requested = startWidth + grow(state, event.clientX - startX);
    if (requested < state.limits.min - COLLAPSE_SNAP) {
      setCollapsed(state, true);
    } else {
      setWidth(state, requested);
    }
  }

  function endDrag(): void {
    if (drag === null) {
      return;
    }
    drag = null;
    element.classList.remove("resizing");
    document.removeEventListener("pointermove", onPointerMove);
    document.removeEventListener("pointerup", endDrag);
    document.removeEventListener("pointercancel", endDrag);
  }

  function startDrag(state: PanelState, event: PointerEvent): void {
    if (event.button !== 0) {
      return;
    }
    event.preventDefault();
    endDrag();
    drag = { state, startX: event.clientX, startWidth: trackWidth(state) };
    element.classList.add("resizing");
    state.separator.focus();
    // Listening on the document follows a pointer that leaves the thin separator mid-drag.
    document.addEventListener("pointermove", onPointerMove);
    document.addEventListener("pointerup", endDrag);
    document.addEventListener("pointercancel", endDrag);
  }

  for (const state of [left, right]) {
    state.separator.addEventListener("keydown", (event) => onKey(state, event));
    state.separator.addEventListener("pointerdown", (event) => startDrag(state, event));
    state.separator.addEventListener("dblclick", () => setCollapsed(state, !state.collapsed));
  }

  /**
   * Fits the panels after the shell's width changes, the terminal panel giving way first: a
   * panel collapses while both minimums leave the conversation too little, then each expanded
   * panel narrows to the room left. A panel collapsed here stays collapsed when the window widens.
   */
  function fit(): void {
    for (const state of [right, left]) {
      if (!state.collapsed && minimumsOverflow()) {
        state.collapsed = true;
      }
    }
    shrinkToFit(right);
    shrinkToFit(left);
    render(left);
    render(right);
  }

  // The window's resize event, and the shell's own size for a change that fires no such event.
  // Fitting twice is harmless, and it changes only the columns inside the shell, so it cannot
  // trigger the observer again.
  window.addEventListener("resize", fit);
  const observer = typeof ResizeObserver === "function" ? new ResizeObserver(fit) : null;
  observer?.observe(element);
  render(left);
  render(right);

  function control(state: PanelState): PanelControl {
    return {
      separator: state.separator,
      isCollapsed: () => state.collapsed,
      setCollapsed: (collapsed) => setCollapsed(state, collapsed),
      width: () => state.width,
    };
  }

  return {
    element,
    left: control(left),
    right: control(right),
    fit,
    dispose() {
      endDrag();
      observer?.disconnect();
      window.removeEventListener("resize", fit);
    },
  };
}
