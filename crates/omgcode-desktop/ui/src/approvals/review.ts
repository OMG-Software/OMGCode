import { append, el, replaceChildren } from "../dom";
import { approvalsApi } from "../ipc/approvals_client";
import { toIpcError } from "../ipc/errors";
import type { ApprovalDetail, ApprovalView } from "../ipc/types";
import { changeViews, splitChanges } from "./changes";
import {
  approvalEffect,
  approveGate,
  decisionArgs,
  networkNotes,
  operationLabel,
  REVIEW_AGAIN_CODES,
  REVIEW_DELAY_MS,
  type ReviewInput,
  WRITERS_STOPPED_TEXT,
} from "./model";

export interface ShowOptions {
  /** Shown as an error line under the decision row. */
  notice?: string;
  /** Moves focus to the review heading. Never to Approve. */
  focus?: boolean;
}

export interface ReviewPanel {
  /** The review pane: an empty state and the review itself. */
  element: HTMLElement;
  /** Shows the detail, unticks the acknowledgement and restarts the approve delay. */
  show(detail: ApprovalDetail, options?: ShowOptions): void;
  clear(): void;
  dispose(): void;
}

export interface ReviewHandlers {
  /** Reloads the detail for an id and shows `notice` with it. */
  reload(id: string, notice: string): Promise<void>;
  /** Called after a decision so the list can refresh. */
  onDecided(view: ApprovalView): void;
}

function explanation(detail: ApprovalDetail): string | null {
  if (detail.binding === "stale") {
    return detail.approval.operation === "file_write"
      ? "The request no longer matches what was recorded: the file changed since the request. Approving would have no effect."
      : "The request no longer matches what was recorded. Approving would have no effect.";
  }
  if (detail.binding === "not_found") {
    return "The recorded request could not be found. Approving would have no effect.";
  }
  return null;
}

function bindingLabel(detail: ApprovalDetail): string {
  if (detail.binding === "exact") {
    return "Exact match";
  }
  return detail.binding === "stale" ? "Stale" : "Not found";
}

/** The chip beside the title: whether a turn waits, or why the request cannot be approved. */
function statusChip(detail: ApprovalDetail): HTMLElement {
  if (detail.approval.state !== "pending") {
    return el("span", { class: "chip", text: `Already ${detail.approval.state}` });
  }
  if (detail.binding !== "exact") {
    return el("span", {
      class: "chip chip-invalid",
      text: detail.binding === "stale" ? "Out of date" : "Not found",
    });
  }
  return detail.waiting
    ? el("span", { class: "chip chip-waiting", text: "Turn waiting" })
    : el("span", { class: "chip", text: "No turn waiting" });
}

/** Builds the review panel. It renders approval fields as text and gates Approve. */
export function createReviewPanel(handlers: ReviewHandlers): ReviewPanel {
  const element = el("section", { class: "review", attrs: { "aria-label": "Review" } });
  element.hidden = true;
  const empty = el("div", { class: "empty-state review-empty" }, "Select a request to review it.");
  const pane = el("div", { class: "review-pane" }, empty, element);
  let timer: ReturnType<typeof setTimeout> | null = null;
  // Counts shown reviews. The timer belongs to the newest one, so a decision that finishes
  // after another review was shown must leave that review's timer alone.
  let shownCount = 0;

  function cancelTimer(): void {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function show(detail: ApprovalDetail, options: ShowOptions = {}): void {
    cancelTimer();
    shownCount += 1;
    const shown = shownCount;
    const input: ReviewInput = { writersStopped: false, armed: false, busy: false };
    const isFile = detail.approval.operation === "file_write";
    const pending = detail.approval.state === "pending";
    const exact = detail.binding === "exact";

    const result = el("p", { class: "result", attrs: { role: "status" } });
    if (options.notice !== undefined) {
      replaceChildren(result, options.notice);
      result.classList.add("error");
    }
    const hint = el("p", { class: "muted hint" });
    const approve = el("button", { class: "primary", text: "Approve" });
    approve.type = "button";
    const deny = el("button", { class: "danger", text: "Deny" });
    deny.type = "button";
    const checkbox = el("input", { attrs: { type: "checkbox", id: "writers-stopped" } });
    const checkLabel = el(
      "label",
      { class: "field-check", attrs: { for: "writers-stopped" } },
      checkbox,
      el("span", { text: WRITERS_STOPPED_TEXT }),
    );

    function sync(): void {
      const gate = approveGate(detail, input);
      approve.disabled = !gate.enabled;
      deny.disabled = input.busy;
      replaceChildren(hint, gate.reason ?? "");
      hint.hidden = gate.reason === null || !exact;
    }

    async function decide(decision: "approve" | "deny"): Promise<void> {
      const args = decisionArgs(decision, detail, input);
      input.busy = true;
      sync();
      try {
        const view = await approvalsApi.decide(
          detail.approval.id,
          args.decision,
          args.writersStopped,
          args.reviewToken,
        );
        if (shown === shownCount) {
          cancelTimer();
        }
        replaceChildren(result, `Decision recorded: ${view.state}.`);
        result.classList.remove("error");
        approve.disabled = true;
        deny.disabled = true;
        checkbox.disabled = true;
        handlers.onDecided(view);
      } catch (reason) {
        const error = toIpcError(reason);
        input.busy = false;
        if (REVIEW_AGAIN_CODES.has(error.code)) {
          await handlers.reload(
            detail.approval.id,
            "The review expired or the request changed. Review it again.",
          );
          return;
        }
        replaceChildren(result, `${error.message} (${error.code})`);
        result.classList.add("error");
        sync();
      }
    }

    approve.addEventListener("click", () => void decide("approve"));
    deny.addEventListener("click", () => void decide("deny"));
    checkbox.addEventListener("change", () => {
      input.writersStopped = checkbox.checked;
      sync();
    });

    const split = splitChanges(detail.tool, exact ? detail.fields : []);
    const fields = el("dl", { class: "fields" });
    for (const field of split.rest) {
      append(
        fields,
        el("dt", { text: field.label }),
        el("dd", {}, field.multiline ? el("pre", { text: field.value }) : field.value),
      );
    }
    const explain = explanation(detail);
    const effect = approvalEffect(detail);
    const heading = el("h2", {
      class: "review-title",
      text: operationLabel(detail.approval.operation),
      attrs: { tabindex: "-1" },
    });
    const technical = el(
      "details",
      { class: "technical" },
      el("summary", { text: "Technical details" }),
      el(
        "dl",
        { class: "tech-list" },
        el("dt", { text: "Binding" }),
        el("dd", { text: bindingLabel(detail) }),
        ...(exact
          ? [
              detail.tool === null ? null : el("dt", { text: "Tool" }),
              detail.tool === null ? null : el("dd", { text: detail.tool }),
              el("dt", { text: "Request SHA-256" }),
              el("dd", {}, el("code", { text: detail.approval.request_sha256 })),
            ]
          : []),
      ),
    );
    replaceChildren(
      element,
      el(
        "div",
        { class: "review-body" },
        el(
          "header",
          { class: "review-head" },
          el(
            "div",
            { class: "review-heading" },
            heading,
            exact && detail.tool !== null
              ? el("p", { class: "tool muted", text: detail.tool })
              : null,
          ),
          statusChip(detail),
        ),
        explain === null ? null : el("p", { class: "banner", text: explain }),
        ...networkNotes(detail).map((note) =>
          el("p", { class: "banner network-note", text: note }),
        ),
        exact && fields.childElementCount > 0 ? fields : null,
        ...(exact ? changeViews(split) : []),
        technical,
      ),
      el(
        "div",
        { class: "review-actions" },
        effect === null ? null : el("p", { class: "effect", text: effect }),
        pending && isFile && exact ? checkLabel : null,
        el("div", { class: "decision" }, ...(pending ? [deny, exact ? approve : null] : [])),
        hint,
        result,
      ),
    );
    empty.hidden = true;
    element.hidden = false;
    sync();
    if (options.focus === true) {
      heading.focus();
    }
    timer = setTimeout(() => {
      timer = null;
      input.armed = true;
      sync();
    }, REVIEW_DELAY_MS);
  }

  return {
    element: pane,
    show,
    clear() {
      cancelTimer();
      element.hidden = true;
      element.replaceChildren();
      empty.hidden = false;
    },
    dispose: cancelTimer,
  };
}
