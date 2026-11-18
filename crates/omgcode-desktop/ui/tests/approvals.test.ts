import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type ApprovalsApp, startApprovals } from "../src/approvals/app";
import {
  approveGate,
  decisionArgs,
  orderApprovals,
  remainingSeconds,
} from "../src/approvals/model";
import { setInvoker } from "../src/ipc/client";
import type { ApprovalDetail, ApprovalView } from "../src/ipc/types";
import {
  buttonByText,
  type FakeIpc,
  HOSTILE_IMG,
  hasButton,
  installFakeIpc,
  mountRoot,
  rejection,
  settle,
} from "./helpers";

function view(overrides: Partial<ApprovalView> = {}): ApprovalView {
  return {
    id: "a1",
    session_id: "s1",
    session_title: "Session title",
    folder_display: "C:\\work",
    operation: "command",
    summary: "cargo test",
    state: "pending",
    created_unix: 900,
    expires_unix: 1065,
    request_sha256: "f".repeat(64),
    writers_stopped: false,
    ...overrides,
  };
}

function detail(
  overrides: Partial<ApprovalDetail> = {},
  approval: Partial<ApprovalView> = {},
): ApprovalDetail {
  return {
    approval: view(approval),
    binding: "exact",
    tool: "workspace_run_command",
    fields: [
      { label: "Program", value: "cargo", multiline: false },
      { label: "Arguments", value: `test\n${HOSTILE_IMG}`, multiline: true },
    ],
    review_token: "tok-1",
    ...overrides,
  };
}

describe("approval model", () => {
  it("orders pending by soonest expiry, then decided newest first", () => {
    const { pending, decided } = orderApprovals([
      view({ id: "late", expires_unix: 2000 }),
      view({ id: "old", state: "denied", created_unix: 100 }),
      view({ id: "soon", expires_unix: 1100 }),
      view({ id: "new", state: "approved", created_unix: 800 }),
    ]);
    expect(pending.map((a) => a.id)).toEqual(["soon", "late"]);
    expect(decided.map((a) => a.id)).toEqual(["new", "old"]);
  });

  it("computes the countdown from the core clock plus elapsed monotonic time", () => {
    expect(remainingSeconds(1065, 1000, 0)).toBe(65);
    expect(remainingSeconds(1065, 1000, 5999)).toBe(60);
    expect(remainingSeconds(1065, 1000, 90_000)).toBe(0);
  });

  it("gates Approve on binding, token, state, acknowledgement and delay", () => {
    const ready = { writersStopped: false, armed: true, busy: false };
    expect(approveGate(detail(), ready).enabled).toBe(true);
    expect(approveGate(detail(), { ...ready, armed: false }).enabled).toBe(false);
    expect(approveGate(detail({ binding: "stale" }), ready).enabled).toBe(false);
    expect(approveGate(detail({ binding: "not_found" }), ready).enabled).toBe(false);
    expect(approveGate(detail({ review_token: null }), ready).enabled).toBe(false);
    expect(approveGate(detail({}, { state: "denied" }), ready).enabled).toBe(false);
    expect(approveGate(detail(), { ...ready, busy: true }).enabled).toBe(false);
    const file = detail({}, { operation: "file_write" });
    expect(approveGate(file, ready).enabled).toBe(false);
    expect(approveGate(file, { ...ready, writersStopped: true }).enabled).toBe(true);
  });

  it("builds decision arguments", () => {
    const input = { writersStopped: true, armed: true, busy: false };
    expect(decisionArgs("deny", detail(), input)).toEqual({
      decision: "deny",
      writersStopped: false,
      reviewToken: null,
    });
    expect(decisionArgs("approve", detail(), input)).toEqual({
      decision: "approve",
      writersStopped: false,
      reviewToken: "tok-1",
    });
    expect(
      decisionArgs("approve", detail({}, { operation: "file_write" }), input).writersStopped,
    ).toBe(true);
  });
});

describe("approvals window", () => {
  let app: ApprovalsApp | null = null;
  let root: HTMLElement;
  let ipc: FakeIpc;
  let approvals: ApprovalView[];
  let current: ApprovalDetail;

  async function start(): Promise<void> {
    ipc = installFakeIpc({
      approvals_list: () => ({ now_unix: 1000, approvals }),
      approval_detail: () => current,
      approval_decide: (args) => ({
        ...view(),
        state: args.decision === "approve" ? "approved" : "denied",
      }),
    });
    root = mountRoot();
    app = startApprovals(root);
    await settle();
  }

  async function select(): Promise<void> {
    (root.querySelector(".approval-button") as HTMLButtonElement).click();
    await settle();
  }

  beforeEach(() => {
    vi.useFakeTimers();
    approvals = [view()];
    current = detail();
  });

  afterEach(() => {
    app?.dispose();
    app = null;
    setInvoker(null);
    vi.useRealTimers();
  });

  it("lists pending first, shows decided muted, and focuses the list", async () => {
    approvals = [
      view({ id: "d1", state: "denied", summary: "decided one" }),
      view({ id: "p2", summary: "second", expires_unix: 3000 }),
      view({ id: "p1", summary: "first", expires_unix: 1100, operation: "file_write" }),
    ];
    await start();
    const rows = Array.from(root.querySelectorAll(".approval"));
    expect(rows.map((r) => r.classList.contains("pending"))).toEqual([true, true, false]);
    expect(rows[0]?.textContent).toContain("File change");
    expect(rows[0]?.textContent).toContain("first");
    expect(rows[0]?.textContent).toContain("Session title");
    expect(rows[2]?.textContent).toContain("denied");
    expect(document.activeElement).toBe(root.querySelector(".approval-button"));
    expect(root.textContent).toContain("Nothing runs when you approve.");
  });

  it("counts down from the core clock", async () => {
    await start();
    expect(root.querySelector(".countdown")?.textContent).toBe("expires in 01:05");
    await settle(2000);
    expect(root.querySelector(".countdown")?.textContent).toBe("expires in 01:03");
  });

  it("shows the exact request and the full hash after selecting", async () => {
    await start();
    expect(root.querySelector(".review")).toHaveProperty("hidden", true);
    await select();
    expect(ipc.callsTo("approval_detail")[0]?.args).toEqual({ id: "a1" });
    const review = root.querySelector(".review") as HTMLElement;
    expect(review.textContent).toContain("workspace_run_command");
    expect(review.textContent).toContain("Program");
    expect(review.querySelector("pre")?.textContent).toBe(`test\n${HOSTILE_IMG}`);
    expect(review.textContent).toContain("f".repeat(64));
    expect(review.querySelector("img")).toBeNull();
    expect(document.activeElement).not.toBe(buttonByText(review, "Approve"));
  });

  it("keeps Approve disabled for one second after the review renders", async () => {
    await start();
    await select();
    const approve = buttonByText(root, "Approve");
    expect(approve.disabled).toBe(true);
    await settle(999);
    expect(approve.disabled).toBe(true);
    await settle(1);
    expect(approve.disabled).toBe(false);
  });

  it("renders the fields of the multi-edit and create tools as plain text", async () => {
    approvals = [view({ operation: "file_write" })];
    current = detail(
      {
        tool: "workspace_multi_edit_file",
        fields: [
          { label: "File", value: "src/lib.rs", multiline: false },
          { label: "Edits", value: "2", multiline: false },
          { label: "Old text 1", value: `a\n${HOSTILE_IMG}`, multiline: true },
          { label: "New text 1", value: "b", multiline: true },
          { label: "Old text 2", value: "c", multiline: true },
          { label: "New text 2", value: "d", multiline: true },
        ],
      },
      { operation: "file_write" },
    );
    await start();
    await select();
    let review = root.querySelector(".review") as HTMLElement;
    expect(review.textContent).toContain("workspace_multi_edit_file");
    expect(review.textContent).toContain("Old text 2");
    expect(review.textContent).toContain("New text 2");
    expect(review.querySelector("img")).toBeNull();
    expect(review.querySelector("pre")?.textContent).toBe(`a\n${HOSTILE_IMG}`);

    current = detail(
      {
        tool: "workspace_create_file",
        fields: [
          { label: "File", value: "new.txt", multiline: false },
          { label: "Size", value: "12 bytes", multiline: false },
          { label: "SHA-256", value: "e".repeat(64), multiline: false },
          { label: "Content", value: `hello\n${HOSTILE_IMG}`, multiline: true },
        ],
      },
      { operation: "file_write" },
    );
    await select();
    review = root.querySelector(".review") as HTMLElement;
    expect(review.textContent).toContain("workspace_create_file");
    expect(review.textContent).toContain("Content");
    expect(review.querySelector("img")).toBeNull();
    expect(review.querySelector("pre")?.textContent).toBe(`hello\n${HOSTILE_IMG}`);
  });

  it("renders the fields of the rename and delete tools as plain text", async () => {
    approvals = [view({ operation: "file_write" })];
    current = detail(
      {
        tool: "workspace_rename_file",
        fields: [
          { label: "From", value: "src/old.rs", multiline: false },
          { label: "To", value: `src/new${HOSTILE_IMG}.rs`, multiline: false },
          { label: "Size", value: "11 bytes", multiline: false },
          { label: "SHA-256", value: "e".repeat(64), multiline: false },
        ],
      },
      { operation: "file_write" },
    );
    await start();
    await select();
    let review = root.querySelector(".review") as HTMLElement;
    expect(review.textContent).toContain("workspace_rename_file");
    expect(review.textContent).toContain("From");
    expect(review.textContent).toContain("To");
    expect(review.textContent).toContain(`src/new${HOSTILE_IMG}.rs`);
    expect(review.querySelector("img")).toBeNull();

    current = detail(
      {
        tool: "workspace_delete_file",
        fields: [
          { label: "File", value: `old${HOSTILE_IMG}.txt`, multiline: false },
          { label: "Size", value: "11 bytes", multiline: false },
          { label: "SHA-256", value: "f".repeat(64), multiline: false },
        ],
      },
      { operation: "file_write" },
    );
    await select();
    review = root.querySelector(".review") as HTMLElement;
    expect(review.textContent).toContain("workspace_delete_file");
    expect(review.textContent).toContain(`old${HOSTILE_IMG}.txt`);
    expect(review.querySelector("img")).toBeNull();
  });

  it("requires the writers-stopped acknowledgement for a file change", async () => {
    approvals = [view({ operation: "file_write" })];
    current = detail({ tool: "workspace_edit_file" }, { operation: "file_write" });
    await start();
    await select();
    await settle(1000);
    const approve = buttonByText(root, "Approve");
    expect(approve.disabled).toBe(true);
    expect(root.querySelector(".review")?.textContent).toContain(
      "I have stopped every other program that could write to this file. OMGCode cannot detect a concurrent writer.",
    );
    const box = root.querySelector('input[type="checkbox"]') as HTMLInputElement;
    box.checked = true;
    box.dispatchEvent(new Event("change", { bubbles: true }));
    expect(approve.disabled).toBe(false);
    approve.click();
    await settle();
    expect(ipc.callsTo("approval_decide")[0]?.args).toEqual({
      id: "a1",
      decision: "approve",
      writers_stopped: true,
      review_token: "tok-1",
    });
    expect(root.querySelector(".result")?.textContent).toContain("approved");
    expect(ipc.callsTo("approvals_list").length).toBeGreaterThan(1);
  });

  it("sends writers_stopped false for a command approval", async () => {
    await start();
    await select();
    await settle(1000);
    buttonByText(root, "Approve").click();
    await settle();
    expect(ipc.callsTo("approval_decide")[0]?.args).toEqual({
      id: "a1",
      decision: "approve",
      writers_stopped: false,
      review_token: "tok-1",
    });
  });

  it("denies at once without a token or acknowledgement", async () => {
    await start();
    await select();
    buttonByText(root, "Deny").click();
    await settle();
    expect(ipc.callsTo("approval_decide")[0]?.args).toEqual({
      id: "a1",
      decision: "deny",
      writers_stopped: false,
      review_token: null,
    });
    expect(root.querySelector(".result")?.textContent).toContain("denied");
  });

  it("never enables Approve without a review token", async () => {
    current = detail({ review_token: null });
    await start();
    await select();
    await settle(2000);
    expect(buttonByText(root, "Approve").disabled).toBe(true);
  });

  it.each(["stale", "not_found"] as const)("offers only Deny for a %s binding", async (binding) => {
    current = detail({ binding, tool: null, fields: [], review_token: null });
    await start();
    await select();
    await settle(2000);
    const review = root.querySelector(".review") as HTMLElement;
    expect(hasButton(review, "Approve")).toBe(false);
    expect(hasButton(review, "Deny")).toBe(true);
    expect(review.querySelector(".banner")?.textContent).toContain(
      "Approving would have no effect",
    );
    expect(review.querySelector(".fields")).toBeNull();
  });

  it("explains a changed file for a stale file edit", async () => {
    approvals = [view({ operation: "file_write" })];
    current = detail(
      { binding: "stale", tool: null, fields: [], review_token: null },
      { operation: "file_write" },
    );
    await start();
    await select();
    expect(root.querySelector(".review .banner")?.textContent).toContain("the file changed");
  });

  it("reloads the review and disables Approve again after review_expired", async () => {
    await start();
    ipc.on("approval_decide", () => rejection("review_expired", "The review token expired."));
    await select();
    await settle(1000);
    buttonByText(root, "Approve").click();
    await settle();
    expect(ipc.callsTo("approval_detail")).toHaveLength(2);
    expect(root.querySelector(".result")?.textContent).toContain("Review it again");
    expect(buttonByText(root, "Approve").disabled).toBe(true);
    await settle(1000);
    expect(buttonByText(root, "Approve").disabled).toBe(false);
  });

  it("shows other decision errors as text", async () => {
    await start();
    ipc.on("approval_decide", () => rejection("approval_not_pending", `Gone ${HOSTILE_IMG}`));
    await select();
    buttonByText(root, "Deny").click();
    await settle();
    expect(root.querySelector(".result")?.textContent).toContain(`Gone ${HOSTILE_IMG}`);
    expect(root.querySelector("img")).toBeNull();
  });

  describe("stable rows", () => {
    function rowFor(id: string): HTMLElement {
      const found = root.querySelector<HTMLElement>(`[data-approval-id="${id}"]`);
      if (found === null) {
        throw new Error(`No row for ${id}`);
      }
      return found;
    }

    function rowIds(): string[] {
      return Array.from(root.querySelectorAll<HTMLElement>(".pending [data-approval-id]")).map(
        (node) => node.dataset.approvalId ?? "",
      );
    }

    it("keeps the same elements, focus and review across ticks and equal polls", async () => {
      approvals = [view({ id: "a1" }), view({ id: "a2", expires_unix: 3000 })];
      await start();
      const first = rowFor("a1");
      const second = rowFor("a2");
      const items = Array.from(root.querySelectorAll(".approvals-ul > li"));
      second.focus();
      const before = second.querySelector(".countdown")?.textContent;
      const listCalls = ipc.callsTo("approvals_list").length;
      await settle(7000);
      expect(ipc.callsTo("approvals_list").length).toBeGreaterThan(listCalls);
      expect(rowFor("a1")).toBe(first);
      expect(rowFor("a2")).toBe(second);
      expect(Array.from(root.querySelectorAll(".approvals-ul > li"))).toEqual(items);
      expect(document.activeElement).toBe(second);
      expect(second.querySelector(".countdown")?.textContent).not.toBe(before);
    });

    it("shows an expired presentation at zero without replacing the row", async () => {
      approvals = [view({ id: "a1", expires_unix: 1002 })];
      await start();
      const row = rowFor("a1");
      await settle(2000);
      expect(rowFor("a1")).toBe(row);
      expect(row.querySelector(".countdown")?.textContent).toBe("expired");
      expect(row.closest("li")?.classList.contains("expired")).toBe(true);
    });

    it("reconciles a changed list by id and keeps focus, selection and the review", async () => {
      approvals = [
        view({ id: "a1", expires_unix: 2000 }),
        view({ id: "a2", expires_unix: 3000 }),
        view({ id: "a3", expires_unix: 4000 }),
      ];
      await start();
      const keep = rowFor("a1");
      const gone = rowFor("a2");
      const kept3 = rowFor("a3");
      keep.click();
      await settle();
      const review = root.querySelector(".review") as HTMLElement;
      const approve = buttonByText(root, "Approve");
      kept3.focus();
      approvals = [
        view({ id: "a3", expires_unix: 4000 }),
        view({ id: "a1", expires_unix: 2000 }),
        view({ id: "a4", expires_unix: 1500 }),
      ];
      await settle(3000);
      expect(rowIds()).toEqual(["a4", "a1", "a3"]);
      expect(rowFor("a1")).toBe(keep);
      expect(rowFor("a3")).toBe(kept3);
      expect(gone.isConnected).toBe(false);
      expect(document.activeElement).toBe(kept3);
      expect(keep.getAttribute("aria-pressed")).toBe("true");
      expect(root.querySelector(".review")).toBe(review);
      expect(review.hidden).toBe(false);
      expect(buttonByText(root, "Approve")).toBe(approve);
    });

    it("rebuilds only the row whose data changed", async () => {
      approvals = [view({ id: "a1" }), view({ id: "a2", expires_unix: 3000 })];
      await start();
      const unchanged = rowFor("a2");
      const changed = rowFor("a1");
      approvals = [view({ id: "a1", summary: "other" }), view({ id: "a2", expires_unix: 3000 })];
      await settle(3000);
      expect(rowFor("a2")).toBe(unchanged);
      expect(rowFor("a1")).not.toBe(changed);
      expect(rowFor("a1").textContent).toContain("other");
    });

    it("does not restart the Approve delay on ticks or equal polls", async () => {
      await start();
      await select();
      const approve = buttonByText(root, "Approve");
      await settle(1000);
      expect(approve.disabled).toBe(false);
      await settle(6000);
      expect(buttonByText(root, "Approve")).toBe(approve);
      expect(approve.disabled).toBe(false);
      expect(ipc.callsTo("approval_detail")).toHaveLength(1);
    });

    it("clamps long labels in the list", async () => {
      approvals = [view({ folder_display: "x".repeat(5000), summary: "y".repeat(5000) })];
      await start();
      expect(root.querySelector(".who")?.classList.contains("clamp-1")).toBe(true);
      expect(root.querySelector(".summary")?.classList.contains("clamp-3")).toBe(true);
    });
  });
});
