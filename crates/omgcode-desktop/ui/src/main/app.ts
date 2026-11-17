import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { createPoller } from "../poll";
import { createConversation } from "./conversation";
import { createNewSessionDialog } from "./new_session";
import { createSidebar } from "./sidebar";

export const APPROVALS_POLL_MS = 3000;

export interface MainApp {
  dispose(): void;
}

/** Builds the main window inside `root` and starts the approvals-count poll. */
export function startMain(root: HTMLElement): MainApp {
  const statusText = el("span");
  const statusBar = el("div", {
    class: "statusbar",
    attrs: { role: "status", "aria-live": "polite" },
  });
  statusBar.hidden = true;

  function showApprovalNudge(): void {
    replaceChildren(
      statusBar,
      statusText,
      button("Open approvals", () => void showApprovals(), { class: "primary" }),
    );
    replaceChildren(statusText, "An approval is waiting.");
    statusBar.hidden = false;
  }

  async function showApprovals(): Promise<void> {
    try {
      await api.approvalsShowWindow();
    } catch {
      replaceChildren(statusBar, "The approvals window could not be opened.");
      statusBar.hidden = false;
    }
  }

  const conversation = createConversation({ onTreeChanged: () => void sidebar.refresh() });
  const sidebar = createSidebar({
    onOpen: (session, folder, seq) => void conversation.open(session, folder, seq),
    onNewSession: () => void newSession.show(),
    onShowApprovals: () => void showApprovals(),
  });
  const newSession = createNewSessionDialog((session, folder) => {
    sidebar.setSelected(session.session_id);
    void conversation.open(session, folder, null);
    void sidebar.refresh();
  });

  replaceChildren(
    root,
    el("div", { class: "shell" }, sidebar.element, conversation.element),
    statusBar,
    newSession.element,
  );

  let previousPending: number | null = null;
  const approvalsPoller = createPoller(async () => {
    const { pending } = await api.approvalsSummary();
    sidebar.setPending(pending);
    // The first poll counts from zero: an approval already waiting at startup is shown.
    if (pending > (previousPending ?? 0)) {
      showApprovalNudge();
    } else if (pending === 0) {
      // Remove the notice and any message it carried. `hidden` alone is not enough
      // while a stylesheet rule sets `display` on the bar (see `.statusbar[hidden]`).
      replaceChildren(statusBar);
      statusBar.hidden = true;
    }
    previousPending = pending;
  }, APPROVALS_POLL_MS);

  void sidebar.refresh();
  void approvalsPoller.tick();
  approvalsPoller.start();

  return {
    dispose() {
      approvalsPoller.stop();
      conversation.dispose();
      sidebar.dispose();
    },
  };
}
