import { button, el, neutralise, replaceChildren } from "../dom";
import { api } from "../ipc/client";
import { CANCELLED_CODE, toIpcError } from "../ipc/errors";
import type { BranchView, ConnectionStatus, SessionSummary } from "../ipc/types";
import { connectionLabel, DEFAULT_ENDPOINTS, providerName } from "../providers";

/** The value of the provider entry that chooses the default endpoint. */
export const DEFAULT_CONNECTION = "";

/** Shown when the operator closes the confirmation or answers No. */
export const CONNECTION_UNCHANGED = "The model provider was not changed.";

export interface StatusFooterDeps {
  /** Called after the connection changed here, so the composer reads the new endpoint. */
  onConnectionChanged(): void;
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
 * branch, and the model provider. Choosing a provider names a profile only; Rust asks for a native
 * Yes/No confirmation before anything changes, so this window cannot switch on its own.
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
  const provider = el("select", {
    class: "status-provider",
    attrs: { "aria-label": "Model provider" },
  });
  provider.disabled = true;
  const message = el("span", { class: "status-message", attrs: { role: "status" } });
  const element = el(
    "footer",
    { class: "status-footer" },
    workspace,
    registration,
    branch,
    approvals,
    el("span", { class: "status-spacer" }),
    message,
    el("label", { class: "status-segment" }, el("span", { text: "Model" }), provider),
  );

  let sessionId: string | null = null;
  // Each read carries the counter it started with; a stale answer is dropped.
  let branchRead = 0;
  let connectionRead = 0;
  let choosing = false;
  let disposed = false;

  function renderConnection(status: ConnectionStatus): void {
    const current = status.connection;
    provider.replaceChildren();
    if (!current.selectable) {
      const label = `Set at launch: ${providerName(current.kind)} (${current.endpoint})`;
      provider.append(el("option", { text: neutralise(label), attrs: { value: "" } }));
      provider.disabled = true;
      return;
    }
    provider.append(
      el("option", {
        text: `LM Studio: default (${DEFAULT_ENDPOINTS.lm_studio})`,
        attrs: { value: DEFAULT_CONNECTION },
      }),
    );
    for (const choice of status.choices) {
      provider.append(
        el("option", {
          text: neutralise(connectionLabel(choice.kind, choice.name, choice.endpoint)),
          attrs: { value: choice.name },
        }),
      );
    }
    provider.value = status.choices.find((choice) => choice.active)?.name ?? DEFAULT_CONNECTION;
    provider.disabled = choosing;
  }

  async function readConnection(): Promise<void> {
    const read = ++connectionRead;
    try {
      const status = await api.connectionStatus();
      if (!disposed && read === connectionRead) {
        renderConnection(status);
      }
    } catch (reason) {
      if (!disposed && read === connectionRead) {
        const error = toIpcError(reason);
        replaceChildren(message, `${error.message} (${error.code})`);
      }
    }
  }

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

  async function choose(): Promise<void> {
    const wanted = provider.value === DEFAULT_CONNECTION ? null : provider.value;
    choosing = true;
    provider.disabled = true;
    replaceChildren(message);
    try {
      renderConnection(await api.connectionSelect(wanted));
      deps.onConnectionChanged();
    } catch (reason) {
      const error = toIpcError(reason);
      replaceChildren(
        message,
        error.code === CANCELLED_CODE ? CONNECTION_UNCHANGED : `${error.message} (${error.code})`,
      );
    } finally {
      choosing = false;
      // The recorded choice decides what the dropdown shows, after a Yes, a No, or a failure.
      await readConnection();
    }
  }

  provider.addEventListener("change", () => void choose());

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
      approvals.hidden = count <= 0;
      replaceChildren(
        approvalsText,
        count === 1 ? "1 approval waiting" : `${count} approvals waiting`,
      );
    },
    refresh() {
      if (!choosing) {
        void readConnection();
      }
      void readBranch();
    },
    dispose() {
      disposed = true;
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    },
  };
  void readConnection();
  return footer;
}
