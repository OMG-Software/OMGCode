import { el, replaceChildren } from "../dom";
import { formatCountdown, formatUnix } from "../format";
import { approvalsApi } from "../ipc/approvals_client";
import { toIpcError } from "../ipc/errors";
import type { ApprovalList, ApprovalView } from "../ipc/types";
import { createPoller } from "../poll";
import { createTabs } from "../ui/tabs";
import { operationLabel, orderApprovals, remainingSeconds } from "./model";
import { createReviewPanel, type ShowOptions } from "./review";

export const LIST_POLL_MS = 3000;
const COUNTDOWN_MS = 1000;

export interface ApprovalsApp {
  dispose(): void;
}

interface RowRef {
  li: HTMLElement;
  /** Everything the row displays; a row is rebuilt only when this changes. */
  signature: string;
  /** Pending rows only. */
  expires: number | null;
  countdown: HTMLElement | null;
  button: HTMLButtonElement | null;
}

type QueueTab = "pending" | "history";

function signatureOf(approval: ApprovalView): string {
  return JSON.stringify([
    approval.state,
    approval.summary,
    approval.expires_unix,
    approval.created_unix,
    approval.session_title,
    approval.folder_display,
    approval.operation,
    approval.writers_stopped,
  ]);
}

/** Makes `parent` hold exactly `wanted`, in order, moving only nodes that are out of place. */
function reconcile(parent: HTMLElement, wanted: readonly HTMLElement[]): void {
  let cursor = parent.firstChild;
  for (const node of wanted) {
    if (node === cursor) {
      cursor = cursor.nextSibling;
    } else {
      parent.insertBefore(node, cursor);
    }
  }
  while (cursor !== null) {
    const next = cursor.nextSibling;
    parent.removeChild(cursor);
    cursor = next;
  }
}

function stateChip(state: ApprovalView["state"]): HTMLElement {
  const known = state === "approved" || state === "denied";
  return el("span", { class: known ? `chip chip-${state}` : "chip", text: state });
}

/**
 * Builds the approvals window. It never renders transcript text: it shows only the
 * fields the core returns for each approval.
 */
export function startApprovals(root: HTMLElement): ApprovalsApp {
  let received: ApprovalList = { now_unix: 0, approvals: [] };
  let receivedAtMs = performance.now();
  let selectedId: string | null = null;
  // Counts detail requests, so a late answer to an older one is dropped.
  let detailRequest = 0;
  let rows = new Map<string, RowRef>();
  let pendingOrder: string[] = [];
  let focusedOnce = false;
  let decidedId: string | null = null;

  const badge = el("span", { class: "badge", attrs: { "aria-label": "Pending approvals" } });
  badge.hidden = true;
  const emptyNote = el("div", { class: "empty-state" }, "No approvals are waiting.");
  const pendingList = el("ul", { class: "approvals-ul" });
  const historyEmpty = el("div", { class: "empty-state" }, "Nothing has been decided yet.");
  const historyList = el("ul", { class: "approvals-ul" });
  const pendingPanel = el("div", { class: "queue-panel" }, emptyNote, pendingList);
  const historyPanel = el("div", { class: "queue-panel" }, historyEmpty, historyList);
  historyPanel.hidden = true;

  function showTab(id: QueueTab): void {
    pendingPanel.hidden = id !== "pending";
    historyPanel.hidden = id !== "history";
  }

  const tabs = createTabs<QueueTab>(
    "Approval queue",
    [
      { id: "pending", label: "Pending (0)" },
      { id: "history", label: "History (0)" },
    ],
    "pending",
    showTab,
  );
  const queue = el(
    "aside",
    { class: "queue", attrs: { tabindex: "-1", "aria-label": "Queue" } },
    tabs.element,
    pendingPanel,
    historyPanel,
  );
  const errorLine = el("p", { class: "error", attrs: { role: "status" } });
  errorLine.hidden = true;
  const noticeLine = el("p", { class: "notice", attrs: { role: "status" } });
  noticeLine.hidden = true;
  const review = createReviewPanel({
    reload: (id, notice) => loadDetail(id, { notice, focus: true }),
    onDecided: (view) => {
      decidedId = view.id;
      void poller.tick();
    },
  });

  replaceChildren(
    root,
    el(
      "main",
      { class: "approvals" },
      el(
        "header",
        { class: "approvals-header" },
        el("h1", { text: "Approvals" }),
        badge,
        el("p", {
          class: "intro",
          text: "Approving lets a request run once. If its turn is waiting, it runs as soon as you approve; otherwise the model's next identical request runs it.",
        }),
      ),
      errorLine,
      noticeLine,
      el("div", { class: "approvals-body" }, queue, review.element),
    ),
  );

  function elapsedMs(): number {
    return performance.now() - receivedAtMs;
  }

  function updateCountdowns(): void {
    for (const row of rows.values()) {
      if (row.expires === null || row.countdown === null) {
        continue;
      }
      const left = remainingSeconds(row.expires, received.now_unix, elapsedMs());
      const label = left === 0 ? "expired" : `expires in ${formatCountdown(left)}`;
      if (row.countdown.textContent !== label) {
        replaceChildren(row.countdown, label);
      }
      row.li.classList.toggle("expired", left === 0);
    }
  }

  function syncSelection(): void {
    for (const [id, row] of rows) {
      row.button?.setAttribute("aria-pressed", String(id === selectedId));
    }
  }

  function approvalRow(approval: ApprovalView, pending: boolean): RowRef {
    const signature = signatureOf(approval);
    const title = [
      el("span", { class: "op", text: operationLabel(approval.operation) }),
      el("span", { class: "summary clamp-1", text: approval.summary }),
    ];
    if (!pending) {
      const li = el(
        "li",
        { class: "approval decided" },
        el(
          "span",
          { class: "row-text" },
          el("span", { class: "row-title" }, ...title),
          el("span", { class: "when", text: `Created ${formatUnix(approval.created_unix)}` }),
        ),
        stateChip(approval.state),
      );
      return { li, signature, expires: null, countdown: null, button: null };
    }
    const who = [approval.session_title, approval.folder_display]
      .filter((part): part is string => part !== null)
      .join(" · ");
    const countdown = el("span", { class: "countdown" });
    const select = el(
      "button",
      { class: "approval-button" },
      el(
        "span",
        { class: "row-text" },
        el("span", { class: "row-title" }, ...title),
        el("span", { class: "who clamp-1", text: who === "" ? "Unknown session" : who }),
      ),
      countdown,
    );
    select.type = "button";
    select.setAttribute("aria-pressed", String(approval.id === selectedId));
    select.dataset.approvalId = approval.id;
    select.addEventListener("click", () => selectApproval(approval.id, { focus: true }));
    return {
      li: el("li", { class: "approval pending" }, select),
      signature,
      expires: approval.expires_unix,
      countdown,
      button: select,
    };
  }

  function draw(): void {
    const activeId =
      document.activeElement instanceof HTMLElement
        ? document.activeElement.dataset.approvalId
        : undefined;
    const { pending, decided } = orderApprovals(received.approvals);
    const next = new Map<string, RowRef>();
    const rowsFor = (list: ApprovalView[], isPending: boolean): HTMLElement[] =>
      list.map((approval) => {
        const old = rows.get(approval.id);
        const row =
          old !== undefined && old.signature === signatureOf(approval)
            ? old
            : approvalRow(approval, isPending);
        next.set(approval.id, row);
        return row.li;
      });
    const pendingNodes = rowsFor(pending, true);
    const decidedNodes = rowsFor(decided, false);
    rows = next;
    pendingOrder = pending.map((a) => a.id);
    reconcile(pendingList, pendingNodes);
    reconcile(historyList, decidedNodes);
    emptyNote.hidden = pending.length > 0;
    pendingList.hidden = pending.length === 0;
    historyEmpty.hidden = decided.length > 0;
    historyList.hidden = decided.length === 0;
    tabs.setLabel("pending", `Pending (${pending.length})`);
    tabs.setLabel("history", `History (${decided.length})`);
    const count = String(pending.length);
    if (badge.textContent !== count) {
      replaceChildren(badge, count);
    }
    badge.hidden = pending.length === 0;
    syncSelection();
    updateCountdowns();
    if (activeId !== undefined) {
      const focused = rows.get(activeId)?.button;
      if (focused !== null && focused !== undefined && document.activeElement !== focused) {
        focused.focus();
      }
    }
  }

  function showNotice(message: string | null): void {
    replaceChildren(noticeLine, message ?? "");
    noticeLine.hidden = message === null;
  }

  function showError(message: string | null): void {
    replaceChildren(errorLine, message ?? "");
    errorLine.hidden = message === null;
  }

  /**
   * Loads one review. Only the newest request's answer is shown: a new detail request replaces
   * the approval's review token in core, so an older answer arriving late would carry a token
   * that no longer works.
   */
  async function loadDetail(id: string, options: ShowOptions = {}): Promise<void> {
    detailRequest += 1;
    const request = detailRequest;
    const current = (): boolean => request === detailRequest && selectedId === id;
    try {
      const detail = await approvalsApi.detail(id);
      if (current()) {
        review.show(detail, options);
      }
    } catch (reason) {
      if (current()) {
        const error = toIpcError(reason);
        review.clear();
        showError(`${error.message} (${error.code})`);
      }
    }
  }

  /** Selects a request (or none) and loads its review. Every call starts a fresh review. */
  function selectApproval(id: string | null, options: ShowOptions = {}): void {
    selectedId = id;
    syncSelection();
    if (id === null) {
      review.clear();
      return;
    }
    void loadDetail(id, options);
  }

  function focusSelectedRow(): void {
    if (tabs.current() !== "pending") {
      tabs.select("pending");
      showTab("pending");
    }
    const row = selectedId === null ? undefined : rows.get(selectedId);
    (row?.button ?? queue).focus();
  }

  /**
   * Keeps a pending request selected. It changes the selection only when the selected one is
   * no longer pending; a poll that merely adds a request leaves it alone.
   */
  function autoSelect(): void {
    const { pending } = orderApprovals(received.approvals);
    const selected = received.approvals.find((a) => a.id === selectedId);
    if (selectedId !== null && selected?.state === "pending") {
      return;
    }
    const first = pending[0]?.id ?? null;
    if (selectedId !== null && selectedId === decidedId) {
      if (first === null) {
        return;
      }
      decidedId = null;
      showNotice(`Decision recorded: ${selected?.state ?? "recorded"}.`);
      selectApproval(first, { focus: true });
      return;
    }
    if (first !== selectedId) {
      selectApproval(first);
    }
  }

  async function refresh(): Promise<void> {
    try {
      received = await approvalsApi.list();
      receivedAtMs = performance.now();
      showError(null);
    } catch (reason) {
      const error = toIpcError(reason);
      showError(`${error.message} (${error.code})`);
      return;
    }
    draw();
    autoSelect();
    if (!focusedOnce) {
      focusedOnce = true;
      const first = selectedId === null ? undefined : rows.get(selectedId)?.button;
      (first ?? pendingList.querySelector<HTMLElement>(".approval-button") ?? queue).focus();
    }
  }

  pendingList.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") {
      return;
    }
    const target = event.target;
    const current = target instanceof HTMLElement ? target.dataset.approvalId : undefined;
    const index = current === undefined ? -1 : pendingOrder.indexOf(current);
    if (index < 0) {
      return;
    }
    event.preventDefault();
    const nextId = pendingOrder[index + (event.key === "ArrowDown" ? 1 : -1)];
    if (nextId === undefined) {
      return;
    }
    selectApproval(nextId);
    rows.get(nextId)?.button?.focus();
  });
  review.element.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      focusSelectedRow();
    }
  });

  const poller = createPoller(refresh, LIST_POLL_MS);
  const countdownTimer = setInterval(updateCountdowns, COUNTDOWN_MS);
  void poller.tick();
  poller.start();

  return {
    dispose() {
      poller.stop();
      clearInterval(countdownTimer);
      review.dispose();
    },
  };
}
