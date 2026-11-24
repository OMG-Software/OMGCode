import type { IconName } from "../ui/icons";
import type { SideTab } from "./side_panel";

/** A session view for the right-hand panel: Git status, Processes, or Context. */
export interface PanelView {
  element: HTMLElement;
  /** Shows the given session's data, or clears the view with `null`. */
  showSession(sessionId: string | null): void;
  /** Reads the open session's data again. */
  refresh(): void;
  dispose(): void;
}

/** A side panel tab that shows one session view. */
export interface PanelTab extends SideTab {
  /** The open session, or `null`. Read only while the tab is visible. */
  setSession(sessionId: string | null): void;
  /** Reads again when the tab is visible, after something may have changed (a turn ended). */
  refresh(): void;
  dispose(): void;
}

/**
 * Wraps a view as a side panel tab that reads only while visible (ADR-0083). A hidden tab is
 * cleared, so it reads nothing, polls nothing, and keeps no stale data; showing it again shows the
 * open session afresh. This keeps the Git status run and the processes poll off for a tab nobody
 * can see.
 */
export function panelTab(
  spec: { id: string; label: string; icon: IconName },
  view: PanelView,
): PanelTab {
  let session: string | null = null;
  let visible = false;

  return {
    ...spec,
    content: view.element,
    onVisible(now) {
      visible = now;
      view.showSession(now ? session : null);
    },
    setSession(next) {
      if (next === session) {
        return;
      }
      session = next;
      if (visible) {
        view.showSession(next);
      }
    },
    refresh() {
      if (visible && session !== null) {
        view.refresh();
      }
    },
    dispose() {
      view.dispose();
    },
  };
}
