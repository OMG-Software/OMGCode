import { append, el, replaceChildren } from "../dom";
import { approvalsApi } from "../ipc/approvals_client";
import { toIpcError } from "../ipc/errors";
import type { ApprovalDetail, ApprovalView } from "../ipc/types";
import {
  approvalEffect,
  approveGate,
  decisionArgs,
  REVIEW_AGAIN_CODES,
  REVIEW_DELAY_MS,
  type ReviewInput,
  WRITERS_STOPPED_TEXT,
} from "./model";

export interface ReviewPanel {
  element: HTMLElement;
  /** Shows the detail and restarts the approve delay. `notice` is shown as an error line. */
  show(detail: ApprovalDetail, notice?: string): void;
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

/** Builds the review panel. It renders approval fields as text and gates Approve. */
export function createReviewPanel(handlers: ReviewHandlers): ReviewPanel {
  const element = el("section", { class: "review", attrs: { "aria-label": "Review" } });
  element.hidden = true;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function cancelTimer(): void {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function show(detail: ApprovalDetail, notice?: string): void {
    cancelTimer();
    const input: ReviewInput = { writersStopped: false, armed: false, busy: false };
    const isFile = detail.approval.operation === "file_write";
    const pending = detail.approval.state === "pending";
    const exact = detail.binding === "exact";

    const result = el("p", { class: "result", attrs: { role: "status" } });
    if (notice !== undefined) {
      replaceChildren(result, notice);
      result.classList.add("error");
    }
    const hint = el("p", { class: "muted" });
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
        cancelTimer();
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

    const fields = el("dl", { class: "fields" });
    for (const field of detail.fields) {
      append(
        fields,
        el("dt", { text: field.label }),
        el("dd", {}, field.multiline ? el("pre", { text: field.value }) : field.value),
      );
    }
    const explain = explanation(detail);
    const effect = approvalEffect(detail);
    replaceChildren(
      element,
      el("h2", { text: "Review" }),
      explain === null ? null : el("p", { class: "banner", text: explain }),
      exact && detail.tool !== null
        ? el("p", { class: "tool", text: `Tool: ${detail.tool}` })
        : null,
      exact ? fields : null,
      exact
        ? el(
            "p",
            { class: "hash" },
            "Request SHA-256: ",
            el("code", { text: detail.approval.request_sha256 }),
          )
        : null,
      effect === null ? null : el("p", { class: "effect", text: effect }),
      pending && isFile && exact ? checkLabel : null,
      el("div", { class: "decision" }, ...(pending ? [deny, exact ? approve : null] : [])),
      hint,
      result,
    );
    element.hidden = false;
    sync();
    timer = setTimeout(() => {
      timer = null;
      input.armed = true;
      sync();
    }, REVIEW_DELAY_MS);
  }

  return {
    element,
    show,
    clear() {
      cancelTimer();
      element.hidden = true;
      element.replaceChildren();
    },
    dispose: cancelTimer,
  };
}
