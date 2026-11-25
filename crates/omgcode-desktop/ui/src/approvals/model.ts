import type { ApprovalDetail, ApprovalOperation, ApprovalView } from "../ipc/types";

export const REVIEW_DELAY_MS = 1000;
export const WRITERS_STOPPED_TEXT =
  "I have stopped every other program that could write to this file. OMGCode cannot detect a concurrent writer.";

const OPERATION_LABELS: Record<ApprovalOperation, string> = {
  command: "Command",
  file_read: "File read",
  file_write: "File change",
  git_push: "Git push",
  network: "Web fetch",
};

/** The fixed caution for a web fetch approval: what approving sends, and to whom. */
export const NETWORK_CAUTION =
  "Approving sends this exact URL, including its path and query, to that host.";
/** Shown when the review says the host is not on the workspace allowlist now. */
export const NOT_ON_ALLOWLIST_TEXT = "This host is not on the workspace allowlist.";
/** The review field that says whether the host is on the allowlist now: "yes" or "no". */
const ALLOWLIST_FIELD = "Host on the allowlist now";

/**
 * The lines shown above a pending, exactly bound web fetch review: the fixed caution, and a
 * statement when the host is not on the workspace allowlist. None for any other request.
 */
export function networkNotes(detail: ApprovalDetail): string[] {
  if (
    detail.approval.operation !== "network" ||
    detail.approval.state !== "pending" ||
    detail.binding !== "exact"
  ) {
    return [];
  }
  const listed = detail.fields.find((field) => field.label === ALLOWLIST_FIELD)?.value;
  return listed === "no" ? [NETWORK_CAUTION, NOT_ON_ALLOWLIST_TEXT] : [NETWORK_CAUTION];
}

export function operationLabel(operation: ApprovalOperation): string {
  return OPERATION_LABELS[operation];
}

/** Pending approvals first (soonest expiry first), then decided ones (newest first). */
export function orderApprovals(approvals: readonly ApprovalView[]): {
  pending: ApprovalView[];
  decided: ApprovalView[];
} {
  const pending = approvals
    .filter((a) => a.state === "pending")
    .sort((a, b) => a.expires_unix - b.expires_unix);
  const decided = approvals
    .filter((a) => a.state !== "pending")
    .sort((a, b) => b.created_unix - a.created_unix);
  return { pending, decided };
}

/**
 * Seconds left before `expiresUnix`. The base is the core's clock (`listNowUnix`), advanced
 * by `elapsedMs` of monotonic time since the list arrived. The local wall clock is not used.
 */
export function remainingSeconds(
  expiresUnix: number,
  listNowUnix: number,
  elapsedMs: number,
): number {
  return Math.max(0, expiresUnix - (listNowUnix + Math.floor(elapsedMs / 1000)));
}

export interface ReviewInput {
  /** The "writers stopped" checkbox. */
  writersStopped: boolean;
  /** True once the review delay has passed. */
  armed: boolean;
  /** True while a decision is being sent. */
  busy: boolean;
}

export interface Gate {
  enabled: boolean;
  /** Why Approve is disabled, shown as text. */
  reason: string | null;
}

/** What approving the shown detail does, or null when it cannot be approved. */
export function approvalEffect(detail: ApprovalDetail): string | null {
  if (detail.approval.state !== "pending" || detail.binding !== "exact") {
    return null;
  }
  return detail.waiting
    ? "Its turn is waiting. Approving runs this request now, after its checks are repeated."
    : "No turn is waiting for this request. Approving runs nothing now: it lets the model's next identical request run once.";
}

/** Decides whether Approve may be used for the shown detail. */
export function approveGate(detail: ApprovalDetail, input: ReviewInput): Gate {
  if (detail.approval.state !== "pending") {
    return { enabled: false, reason: "This approval is no longer pending." };
  }
  if (detail.binding !== "exact") {
    return { enabled: false, reason: "The recorded request cannot be approved." };
  }
  if (detail.review_token === null) {
    return { enabled: false, reason: "No review token was issued. Reload the review." };
  }
  if (detail.approval.operation === "file_write" && !input.writersStopped) {
    return { enabled: false, reason: "Confirm that other writers are stopped." };
  }
  if (!input.armed) {
    return { enabled: false, reason: "Approve unlocks after a short review delay." };
  }
  if (input.busy) {
    return { enabled: false, reason: null };
  }
  return { enabled: true, reason: null };
}

export interface DecisionArgs {
  decision: "approve" | "deny";
  writersStopped: boolean;
  reviewToken: string | null;
}

/** Arguments for `approval_decide`. Deny never carries a token or an acknowledgement. */
export function decisionArgs(
  decision: "approve" | "deny",
  detail: ApprovalDetail,
  input: ReviewInput,
): DecisionArgs {
  if (decision === "deny") {
    return { decision, writersStopped: false, reviewToken: null };
  }
  return {
    decision,
    writersStopped: detail.approval.operation === "file_write" && input.writersStopped,
    reviewToken: detail.review_token,
  };
}

export const REVIEW_AGAIN_CODES: ReadonlySet<string> = new Set([
  "review_expired",
  "review_mismatch",
]);
