import { el, replaceChildren } from "../dom";
import { formatCountdown, formatUnix } from "../format";
import { approvalsApi } from "../ipc/approvals_client";
import { toIpcError } from "../ipc/errors";
import type { ApprovalList, ApprovalView } from "../ipc/types";
import { createPoller } from "../poll";
import { operationLabel, orderApprovals, remainingSeconds } from "./model";
import { createReviewPanel } from "./review";

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

/**
 * Builds the approvals window. It never renders transcript text: it shows only the
 * fields the core returns for each approval.
 */
export function startApprovals(root: HTMLElement): ApprovalsApp {
  let received: ApprovalList = { now_unix: 0, approvals: [] };
  let receivedAtMs = performance.now();
  let selectedId: string | null = null;
  let rows = new Map<string, RowRef>();
  let focusedOnce = false;
  let decidedId: string | null = null;

  const emptyNote = el("p", { class: "empty", text: "No approvals are pending." });
  const pendingList = el("ul", { class: "approvals-ul" });
  const decidedHeading = el("h2", { text: "Recently decided" });
  const decidedList = el("ul", { class: "approvals-ul" });
  const listBox = el(
    "div",
    { class: "approval-list", attrs: { tabindex: "-1" } },
    emptyNote,
    pendingList,
    decidedHeading,
    decidedList,
  );
  const errorLine = el("p", { class: "error", attrs: { role: "status" } });
  errorLine.hidden = true;
  const review = createReviewPanel({
    reload: (id, notice) => loadDetail(id, notice),
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
      el("h1", { text: "Tool approvals" }),
      el("p", {
        class: "intro",
        text: "Approving lets a request run once. If its turn is waiting, it runs as soon as you approve; otherwise the model's next identical request runs it.",
      }),
      errorLine,
      listBox,
      review.element,
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
    const who = [approval.session_title, approval.folder_display]
      .filter((part): part is string => part !== null)
      .join(" · ");
    const body = [
      el("span", { class: "op", text: operationLabel(approval.operation) }),
      el("span", { class: "who clamp-1", text: who === "" ? "Unknown session" : who }),
      el("span", { class: "summary clamp-3", text: approval.summary }),
      el("span", { class: "when", text: `Created ${formatUnix(approval.created_unix)}` }),
    ];
    if (!pending) {
      const li = el(
        "li",
        { class: "approval decided" },
        ...body,
        el("span", { class: "state", text: approval.state }),
      );
      return { li, signature, expires: null, countdown: null, button: null };
    }
    const countdown = el("span", { class: "countdown" });
    const select = el("button", { class: "approval-button" }, ...body, countdown);
    select.type = "button";
    select.setAttribute("aria-pressed", String(approval.id === selectedId));
    select.dataset.approvalId = approval.id;
    select.addEventListener("click", () => {
      selectedId = approval.id;
      void loadDetail(approval.id);
      syncSelection();
    });
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
    reconcile(pendingList, pendingNodes);
    reconcile(decidedList, decidedNodes);
    emptyNote.hidden = pending.length > 0;
    pendingList.hidden = pending.length === 0;
    decidedHeading.hidden = decided.length === 0;
    decidedList.hidden = decided.length === 0;
    updateCountdowns();
    if (activeId !== undefined) {
      const focused = rows.get(activeId)?.button;
      if (focused !== null && focused !== undefined && document.activeElement !== focused) {
        focused.focus();
      }
    }
    if (!focusedOnce) {
      focusedOnce = true;
      (listBox.querySelector<HTMLElement>(".approval-button") ?? listBox).focus();
    }
  }

  async function loadDetail(id: string, notice?: string): Promise<void> {
    try {
      const detail = await approvalsApi.detail(id);
      if (selectedId === id) {
        review.show(detail, notice);
      }
    } catch (reason) {
      if (selectedId === id) {
        const error = toIpcError(reason);
        review.clear();
        showError(`${error.message} (${error.code})`);
      }
    }
  }

  function showError(message: string | null): void {
    replaceChildren(errorLine, message ?? "");
    errorLine.hidden = message === null;
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
    const selected = received.approvals.find((a) => a.id === selectedId);
    if (
      selectedId !== null &&
      (selected === undefined || selected.state !== "pending") &&
      selectedId !== decidedId
    ) {
      selectedId = null;
      review.clear();
    }
  }

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
