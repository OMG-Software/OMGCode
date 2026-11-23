import { el } from "../dom";
import type { SideTab } from "./side_panel";

/** What the tab says while no terminal backend exists (ADR-0072). */
export const TERMINAL_UNAVAILABLE =
  "No terminal is available yet. This panel does not start a shell or run commands.";

/**
 * The Terminal tab. It holds the place for a terminal: it calls no command, starts no process,
 * and has no input.
 */
export function terminalTab(): SideTab {
  return {
    id: "terminal",
    label: "Terminal",
    icon: "terminal",
    content: el("p", { class: "terminal-empty", text: TERMINAL_UNAVAILABLE }),
  };
}
