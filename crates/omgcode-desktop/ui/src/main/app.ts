import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { createPoller } from "../poll";
import { createConnectionModel } from "./connection_state";
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

  // The open session's provider and model: one source of truth for the composer's dropdowns and
  // the footer's (ADR-0067).
  const connection = createConnectionModel();
  void connection.open(null);
  const footer = createStatusFooter({
    connection,
    onShowApprovals: () => void showApprovals(),
  });
  const conversation = createConversation({
    connection,
    onTreeChanged: () => {
      void sidebar.refresh();
      footer.refresh();
    },
  });
  const sidebar = createSidebar({
    onOpen: (session, folder, seq) => {
      void connection.open(session.session_id);
      footer.showSession(session, folder);
      void conversation.open(session, folder, seq);
    },
    onNewSession: () => void newSession.show(),
    onShowApprovals: () => void showApprovals(),
    onShowSettings: () => void showSettings(),
  });
  const newSession = createNewSessionDialog((session, folder) => {
    sidebar.setSelected(session.session_id);
    void connection.open(session.session_id);
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
      connection.dispose();
    },
  };
}
