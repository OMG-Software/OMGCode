import { button, el, neutralise, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import type { BranchView, SessionSummary } from "../ipc/types";
import type { ConnectionModel } from "./connection_state";

export interface StatusFooterDeps {
  /**
   * The open session's provider and model. The footer shows none of it; it only asks for a fresh
   * read when the window regains focus, so the composer follows a change made elsewhere.
   */
  connection: ConnectionModel;
  /** Called when the operator chooses Review in the approvals segment. */
  onShowApprovals(): void;
}

export interface StatusFooter {
  element: HTMLElement;
  /** Shows the session's workspace folder and reads its Git branch. */
  showSession(session: SessionSummary, folder: string): void;
  /** Shows how many approvals wait; the segment is hidden at zero. */
  setPending(count: number): void;
  /** Reads the connection and, with a session open, the branch again. */
  refresh(): void;
  dispose(): void;
}

/**
 * What the branch segment says. The name and the reason are display text from the core; like the
 * rest of this window, the footer shows them as text rather than in `title` tooltips.
 */
export function branchText(branch: BranchView): string {
  switch (branch.state) {
    case "branch":
      return `Branch: ${branch.name ?? ""}`;
    case "detached":
      return `Detached at ${branch.name ?? ""}`;
    case "none":
      return "Not a Git repository";
    case "unavailable":
      return branch.note === null ? "Branch unavailable" : `Branch unavailable: ${branch.note}`;
  }
}

/**
 * The bar along the bottom of the main window (ADR-0064): the open session's workspace, its Git
 * branch, and the approvals segment. It no longer offers a provider choice (ADR-0084); the composer
 * chooses the provider and model and shows a connection failure.
 */
export function createStatusFooter(deps: StatusFooterDeps): StatusFooter {
  const workspace = el("span", { class: "status-segment", text: "No session open" });
  const registration = el("span", { class: "status-segment muted" });
  const branch = el("span", { class: "status-segment muted" });
  const approvalsText = el("span");
  const approvals = el(
    "span",
    { class: "status-segment status-approvals" },
    approvalsText,
    el("span", { class: "status-dot", text: "·", attrs: { "aria-hidden": "true" } }),
    button("Review", deps.onShowApprovals, { class: "status-link" }),
  );
  approvals.hidden = true;
  // A polite live region that is always present, outside the hidden segment, so a screen reader
  // hears when a request starts waiting; Review stays outside it.
  const approvalsLive = el("span", {
    class: "visually-hidden",
    attrs: { role: "status", "aria-live": "polite" },
  });
  let pendingShown = 0;
  const element = el(
    "footer",
    { class: "status-footer" },
    workspace,
    registration,
    branch,
    approvals,
    approvalsLive,
    el("span", { class: "status-spacer" }),
  );

  let sessionId: string | null = null;
  // Each read carries the counter it started with; a stale answer is dropped.
  let branchRead = 0;
  let disposed = false;

  async function readBranch(): Promise<void> {
    const id = sessionId;
    const read = ++branchRead;
    if (id === null) {
      replaceChildren(branch);
      return;
    }
    try {
      const context = await api.sessionContext(id);
      if (disposed || read !== branchRead) {
        return;
      }
      replaceChildren(branch, neutralise(branchText(context.branch)));
      replaceChildren(
        registration,
        context.workspace_registered ? null : "(workspace not registered here)",
      );
    } catch {
      if (!disposed && read === branchRead) {
        replaceChildren(branch, "Branch unavailable");
      }
    }
  }

  function onFocus(): void {
    if (document.visibilityState === "visible") {
      footer.refresh();
    }
  }
  window.addEventListener("focus", onFocus);
  document.addEventListener("visibilitychange", onFocus);

  const footer: StatusFooter = {
    element,
    showSession(session, folder) {
      sessionId = session.session_id;
      replaceChildren(workspace, neutralise(`Workspace: ${folder}`));
      replaceChildren(registration);
      void readBranch();
    },
    setPending(count) {
      const shown = Math.max(count, 0);
      if (shown === pendingShown) {
        return;
      }
      pendingShown = shown;
      const text = shown === 1 ? "1 approval waiting" : `${shown} approvals waiting`;
      approvals.hidden = shown === 0;
      replaceChildren(approvalsText, text);
      // Announce only a change to a nonzero count; clearing to zero needs no interruption.
      replaceChildren(approvalsLive, shown === 0 ? "" : text);
    },
    refresh() {
      void deps.connection.refresh();
      void readBranch();
    },
    dispose() {
      disposed = true;
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    },
  };
  return footer;
}
