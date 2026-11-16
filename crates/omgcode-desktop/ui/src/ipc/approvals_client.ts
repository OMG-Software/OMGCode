import { invokeCommand } from "./client";
import type { ApprovalDetail, ApprovalList, ApprovalView } from "./types";

/** Commands that only the approvals window may call. Imported by that entry alone. */
export const approvalsApi = {
  list: () => invokeCommand<ApprovalList>("approvals_list", {}),
  detail: (id: string) => invokeCommand<ApprovalDetail>("approval_detail", { id }),
  decide: (
    id: string,
    decision: "approve" | "deny",
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
