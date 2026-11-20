/**
 * The main window's columns: the sessions panel, the conversation, and the terminal panel, with
 * a separator between each pair (ADR-0072). A separator follows the WAI-ARIA window splitter
 * pattern: drag it, or focus it and use the arrow keys, Home, End, and Enter.
 *
 * The only inline style the frontend sets is here: each panel's width, a whole number of pixels
 * this module computes and clamps, written through the CSSOM as a custom property on the shell.
 * No other value reaches a style. Widths last until the window closes; nothing stores them.
 */

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
export const KEY_STEP = 16;
export const LARGE_KEY_STEP = 64;
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
  const right = makeState("right", parts.right, "terminal panel");
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

  /** The widest this panel can be while the conversation keeps its minimum, if known. */
  function maxWidth(state: PanelState): number {
    const shellWidth = element.clientWidth;
    if (shellWidth <= 0) {
      // Not laid out (hidden, or a test document): only the panel's own limit applies.
      return state.limits.max;
    }
    const room = shellWidth - CENTER_MIN_WIDTH - trackWidth(other(state)) - 2 * SEPARATOR_WIDTH;
    return clamp(Math.floor(room), state.limits.min, state.limits.max);
  }

  function setWidthProperty(state: PanelState): void {
    // The one inline style in the frontend: a clamped whole number of pixels (see above).
    element.style.setProperty(`--${state.side}-width`, `${state.width}px`);
  }

  function render(state: PanelState): void {
    element.classList.toggle(`${state.side}-collapsed`, state.collapsed);
    setWidthProperty(state);
    const { separator } = state;
    separator.setAttribute("aria-valuemax", String(maxWidth(state)));
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

  function setWidth(state: PanelState, requested: number): void {
    state.width = clamp(Math.round(requested), state.limits.min, maxWidth(state));
    state.collapsed = false;
    render(state);
    render(other(state));
  }

  function setCollapsed(state: PanelState, collapsed: boolean): void {
    if (state.collapsed === collapsed) {
      return;
    }
    state.collapsed = collapsed;
    if (!collapsed) {
      state.width = clamp(state.width, state.limits.min, maxWidth(state));
    }
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

  /** Narrows the panels after the window narrows: the terminal panel gives way first. */
  function fit(): void {
    for (const state of [right, left]) {
      if (!state.collapsed && state.width > maxWidth(state)) {
        state.width = maxWidth(state);
      }
    }
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
