import { el } from "../dom";
import { iconButton } from "../ui/icons";

export interface TerminalPanelHandlers {
  onCollapse(): void;
  onExpand(): void;
}

export interface TerminalPanel {
  element: HTMLElement;
  setCollapsed(collapsed: boolean): void;
}

/** What the panel says while no terminal backend exists (ADR-0072). */
export const TERMINAL_UNAVAILABLE =
  "No terminal is available yet. This panel does not start a shell or run commands.";

/**
 * Builds the right-hand terminal panel. It holds the place for a terminal: it calls no command,
 * starts no process, and has no input. Collapsed, it shows one icon that expands it.
 */
export function createTerminalPanel(handlers: TerminalPanelHandlers): TerminalPanel {
  const full = el(
    "div",
    { class: "terminal-full" },
    el(
      "div",
      { class: "terminal-header" },
      el("h2", { text: "Terminal" }),
      iconButton("panelRight", "Collapse terminal panel", handlers.onCollapse),
    ),
    el("p", { class: "terminal-empty", text: TERMINAL_UNAVAILABLE }),
  );
  const railExpand = iconButton("terminal", "Show terminal panel", handlers.onExpand);
  const rail = el("div", { class: "panel-rail terminal-rail" }, railExpand);
  const element = el(
    "aside",
    { class: "terminal-panel", attrs: { id: "terminal-panel", "aria-label": "Terminal" } },
    full,
    rail,
  );

  return {
    element,
    setCollapsed(collapsed) {
      if (rail.hidden === !collapsed && full.hidden === collapsed) {
        return;
      }
      const hadFocus = (collapsed ? full : rail).contains(document.activeElement);
      full.hidden = collapsed;
      rail.hidden = !collapsed;
      if (hadFocus) {
        (collapsed ? railExpand : full.querySelector<HTMLElement>("button"))?.focus();
      }
    },
  };
}
