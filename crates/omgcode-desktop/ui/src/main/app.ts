import { button, el, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { createPoller } from "../poll";
import { createConnectionModel } from "./connection_state";
import { createContextTab } from "./context_tab";
import { createConversation } from "./conversation";
import { createFilesTab } from "./files_tab";
import { createGitTab } from "./git_tab";
import { createShellLayout } from "./layout";
import { createModelBox, MODEL_RAIL_LABEL } from "./model_box";
import { createNewSessionDialog } from "./new_session";
import { panelTab } from "./panel_tab";
import { createProcessesTab } from "./processes_tab";
import { createSidePanel } from "./side_panel";
import { createSidebar } from "./sidebar";
import { createStatusFooter } from "./status_footer";
import { terminalTab } from "./terminal_panel";
import { createWebTab } from "./web_tab";

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

  // The open session's provider and model: one source of truth for the model box's dropdowns and
  // the open session's status read (ADR-0071). The box sits in the side panel footer (ADR-0089).
  const connection = createConnectionModel();
  void connection.open(null);
  const modelBox = createModelBox(connection);
  const footer = createStatusFooter({
    connection,
    onShowApprovals: () => void showApprovals(),
  });
  const conversation = createConversation({
    connection,
    modelBox,
    onTreeChanged: () => {
      void sidebar.refresh();
      footer.refresh();
      // A turn may have edited files, run commands, or grown the history. Each tab reads again
      // only while visible; a hidden one reads when it is next shown.
      files.refresh();
      for (const tab of sessionTabs) {
        tab.refresh();
      }
    },
  });
  const sidebar = createSidebar({
    onOpen: (session, folder, seq) => {
      void connection.open(session.session_id);
      showSession(session.session_id);
      footer.showSession(session, folder);
      void conversation.open(session, folder, seq);
    },
    onNewSession: () => void newSession.show(),
    onShowApprovals: () => void showApprovals(),
    onShowSettings: () => void showSettings(),
    onCollapse: () => layout.left.setCollapsed(true),
    onExpand: () => layout.left.setCollapsed(false),
  });
  // The right-hand panel's tabs (ADR-0072, ADR-0083, ADR-0085 to ADR-0087, ADR-0098). One entry adds one
  // tab. The session views read only while their tab is visible.
  const files = createFilesTab();
  const sessionTabs = [
    panelTab({ id: "git", label: "Git", icon: "gitStatus" }, createGitTab()),
    panelTab({ id: "processes", label: "Processes", icon: "processes" }, createProcessesTab()),
    panelTab({ id: "web", label: "Web", icon: "web" }, createWebTab()),
    panelTab({ id: "context", label: "Context", icon: "context" }, createContextTab()),
  ];
  function showSession(sessionId: string): void {
    files.setSession(sessionId);
    for (const tab of sessionTabs) {
      tab.setSession(sessionId);
    }
  }
  const sidePanel = createSidePanel(
    [terminalTab(), files, ...sessionTabs],
    {
      onCollapse: () => layout.right.setCollapsed(true),
      onExpand: () => layout.right.setCollapsed(false),
    },
    { railActions: [{ icon: "model", label: MODEL_RAIL_LABEL }] },
  );
  sidePanel.footer.append(modelBox.element);
  // Sessions, conversation, and the side panel, with a resizable, collapsible panel each side (ADR-0072).
  const layout = createShellLayout({
    left: sidebar,
    center: conversation.element,
    right: sidePanel,
  });
  const newSession = createNewSessionDialog((session, folder) => {
    sidebar.setSelected(session.session_id);
    void connection.open(session.session_id);
    showSession(session.session_id);
    footer.showSession(session, folder);
    void conversation.open(session, folder, null);
    void sidebar.refresh();
  });

  replaceChildren(root, layout.element, statusBar, footer.element, newSession.element);

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
      layout.dispose();
      conversation.dispose();
      modelBox.dispose();
      for (const tab of sessionTabs) {
        tab.dispose();
      }
      sidebar.dispose();
      footer.dispose();
      connection.dispose();
    },
  };
}
