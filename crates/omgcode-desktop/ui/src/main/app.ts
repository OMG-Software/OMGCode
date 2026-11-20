import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { createPoller } from "../poll";
import { createConversation } from "./conversation";
import { createNewSessionDialog } from "./new_session";
import { createSidebar } from "./sidebar";
import { createStatusFooter } from "./status_footer";

export const APPROVALS_POLL_MS = 3000;

export interface MainApp {
  dispose(): void;
}

/** Builds the main window inside `root` and starts the approvals-count poll. */
export function startMain(root: HTMLElement): MainApp {
  const statusBar = el("div", {
    class: "statusbar",
    attrs: { role: "status", "aria-live": "polite" },
  });
  statusBar.hidden = true;

  function clearNotice(): void {
    // `hidden` alone is not enough while a stylesheet rule sets `display` on
    // the bar (see `.statusbar[hidden]`), so the content is removed too.
    replaceChildren(statusBar);
    statusBar.hidden = true;
  }

  function showError(text: string): void {
    replaceChildren(statusBar, el("span", { text }), button("Dismiss", clearNotice));
    statusBar.hidden = false;
  }

  async function showApprovals(): Promise<void> {
    try {
      await api.approvalsShowWindow();
    } catch {
      showError("The approvals window could not be opened.");
    }
  }

  async function showSettings(): Promise<void> {
    try {
      await api.settingsShowWindow();
    } catch {
      showError("The settings window could not be opened.");
    }
  }

  const footer = createStatusFooter({
    // The composer shows the endpoint the next turn uses; read it again after a switch.
    onConnectionChanged: () => void conversation.refreshEndpoint().catch(() => undefined),
    onShowApprovals: () => void showApprovals(),
  });
  const conversation = createConversation({
    onTreeChanged: () => {
      void sidebar.refresh();
      footer.refresh();
    },
    onConnectionChanged: () => footer.refresh(),
  });
  const sidebar = createSidebar({
    onOpen: (session, folder, seq) => {
      footer.showSession(session, folder);
      void conversation.open(session, folder, seq);
    },
    onNewSession: () => void newSession.show(),
    onShowApprovals: () => void showApprovals(),
    onShowSettings: () => void showSettings(),
  });
  const newSession = createNewSessionDialog((session, folder) => {
    sidebar.setSelected(session.session_id);
    footer.showSession(session, folder);
    void conversation.open(session, folder, null);
    void sidebar.refresh();
  });

  replaceChildren(
    root,
    el("div", { class: "shell" }, sidebar.element, conversation.element),
    statusBar,
    footer.element,
    newSession.element,
  );

  const approvalsPoller = createPoller(async () => {
    const { pending } = await api.approvalsSummary();
    sidebar.setPending(pending);
    footer.setPending(pending);
  }, APPROVALS_POLL_MS);

  void sidebar.refresh();
  void approvalsPoller.tick();
  approvalsPoller.start();

  return {
    dispose() {
      approvalsPoller.stop();
      conversation.dispose();
      sidebar.dispose();
      footer.dispose();
    },
  };
}
