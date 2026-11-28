import { invokeCommand } from "./client";
import type { ApprovalDecision, ApprovalDetail, ApprovalList, ApprovalView } from "./types";

/** Commands that only the approvals window may call. Imported by that entry alone. */
export const approvalsApi = {
  list: () => invokeCommand<ApprovalList>("approvals_list", {}),
  detail: (id: string) => invokeCommand<ApprovalDetail>("approval_detail", { id }),
  /** `allow_host` sends no host: Rust uses the one its review kept with the token. */
  decide: (
    id: string,
    decision: ApprovalDecision,
    writersStopped: boolean,
    reviewToken: string | null,
  ) =>
    invokeCommand<ApprovalView>("approval_decide", {
      id,
      decision,
      writers_stopped: writersStopped,
      review_token: reviewToken,
    }),
};
