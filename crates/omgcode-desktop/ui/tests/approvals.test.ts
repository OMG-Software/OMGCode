import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type ApprovalsApp, LIST_POLL_MS, startApprovals } from "../src/approvals/app";
import {
  ALLOW_HOST_CANCELLED_TEXT,
  ALLOW_HOST_FULL_TEXT,
  ALLOW_HOST_LABEL,
  ALLOW_HOST_NOT_ALLOW_TEXT,
  allowHostOffer,
  allowHostText,
  approvalEffect,
  approveGate,
  decisionArgs,
  NETWORK_CAUTION,
  NOT_ON_ALLOWLIST_TEXT,
  operationLabel,
  orderApprovals,
  REVIEW_AGAIN_TEXT,
  remainingSeconds,
  reviewAgainNotice,
} from "../src/approvals/model";
import { setInvoker } from "../src/ipc/client";
import type { AllowHostView, ApprovalDetail, ApprovalView } from "../src/ipc/types";
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
    waiting: true,
    allow_host: null,
    ...overrides,
  };
}

describe("approval model", () => {
  it("says whether approving runs the request now or only allows a later one", () => {
    expect(approvalEffect(detail())).toContain("Approving runs this request now");
    expect(approvalEffect(detail({ waiting: false }))).toContain("Approving runs nothing now");
    expect(approvalEffect(detail({ binding: "stale", review_token: null }))).toBeNull();
    expect(approvalEffect(detail({}, { state: "approved" }))).toBeNull();
  });

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
    // Allow host carries the token and never the acknowledgement or a host.
    expect(decisionArgs("allow_host", detail({}, { operation: "network" }), input)).toEqual({
      decision: "allow_host",
      writersStopped: false,
      reviewToken: "tok-1",
    });
  });

  it("offers Allow host only for a pending, exact web fetch review that Rust offered", () => {
    const offered = detail(
      { allow_host: { state: "offered", host: "docs.example.com" } },
      { operation: "network" },
    );
    expect(allowHostOffer(offered)).toBe("docs.example.com");
    expect(allowHostOffer({ ...offered, allow_host: { state: "not_allow" } })).toBeNull();
    expect(allowHostOffer({ ...offered, allow_host: null })).toBeNull();
    expect(allowHostOffer({ ...offered, binding: "stale" })).toBeNull();
    expect(
      allowHostOffer({ ...offered, approval: view({ operation: "network", state: "approved" }) }),
    ).toBeNull();
    expect(allowHostOffer({ ...offered, approval: view({ operation: "command" }) })).toBeNull();
  });

  it("reviews again after every failed Allow host and after an expired review", () => {
    const failure = (code: string) => ({ code, message: "Gone." });
    for (const decision of ["approve", "deny", "allow_host"] as const) {
      expect(reviewAgainNotice(decision, failure("review_expired"))).toBe(REVIEW_AGAIN_TEXT);
      expect(reviewAgainNotice(decision, failure("review_mismatch"))).toBe(REVIEW_AGAIN_TEXT);
    }
    expect(reviewAgainNotice("allow_host", failure("cancelled"))).toBe(ALLOW_HOST_CANCELLED_TEXT);
    expect(reviewAgainNotice("allow_host", failure("web_host_too_many"))).toBe(
      "Gone. (web_host_too_many)",
    );
    expect(reviewAgainNotice("approve", failure("approval_not_pending"))).toBeNull();
  });
});

describe("approvals window", () => {
  let app: ApprovalsApp | null = null;
  let root: HTMLElement;
  let ipc: FakeIpc;
  let approvals: ApprovalView[];
  let current: ApprovalDetail;

  async function start(): Promise<void> {
    // A second start in one test would otherwise leave the first window's listeners live.
    app?.dispose();
    ipc = installFakeIpc({
      approvals_list: () => ({ now_unix: 1000, approvals }),
      approval_detail: () => current,
      approval_decide: (args) => ({
        ...view(),
        state: args.decision === "deny" ? "denied" : "approved",
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
    expect(root.textContent).toContain("If its turn is waiting, it runs as soon as you approve");
  });

  it("counts down from the core clock", async () => {
    await start();
    expect(root.querySelector(".countdown")?.textContent).toBe("expires in 01:05");
    await settle(2000);
    expect(root.querySelector(".countdown")?.textContent).toBe("expires in 01:03");
  });

  it("shows the exact request and the full hash after selecting", async () => {
    await start();
    // The soonest-expiring pending request is selected when the window opens.
    expect(ipc.callsTo("approval_detail")[0]?.args).toEqual({ id: "a1" });
    expect(root.querySelector(".review")).toHaveProperty("hidden", false);
    await select();
    expect(ipc.callsTo("approval_detail")).toHaveLength(2);
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
    expect(review.textContent).toContain("Edit 1 of 2");
    expect(review.textContent).toContain("Edit 2 of 2");
    expect(review.querySelectorAll(".diff")).toHaveLength(2);
    expect(review.querySelector("img")).toBeNull();
    const removed = Array.from(review.querySelectorAll(".diff-remove .diff-text")).map(
      (n) => n.textContent,
    );
    expect(removed).toContain(HOSTILE_IMG);
    expect(removed).toContain("c");
    const added = Array.from(review.querySelectorAll(".diff-add .diff-text")).map(
      (n) => n.textContent,
    );
    expect(added).toEqual(["b", "d"]);

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
    expect(review.querySelector("img")).toBeNull();
    expect(review.querySelector(".diff-remove")).toBeNull();
    const created = Array.from(review.querySelectorAll(".diff-add .diff-text")).map(
      (n) => n.textContent,
    );
    expect(created).toEqual(["hello", HOSTILE_IMG]);
  });

  it("renders a directory creation as one plain field without a diff", async () => {
    approvals = [view({ operation: "file_write" })];
    current = detail(
      {
        tool: "workspace_create_directory",
        fields: [{ label: "Directory", value: `src/${HOSTILE_IMG}`, multiline: false }],
      },
      { operation: "file_write" },
    );
    await start();
    await select();
    const review = root.querySelector(".review") as HTMLElement;
    expect(review.textContent).toContain("workspace_create_directory");
    expect(review.textContent).toContain("Directory");
    expect(review.textContent).toContain(`src/${HOSTILE_IMG}`);
    expect(review.querySelector("img")).toBeNull();
    expect(review.querySelector(".diff")).toBeNull();
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

  it("tells the operator what approving does for the shown request", async () => {
    await start();
    await select();
    expect(root.querySelector(".effect")?.textContent).toContain("Approving runs this request now");
    current = detail({ waiting: false });
    await select();
    expect(root.querySelector(".effect")?.textContent).toContain("Approving runs nothing now");
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

  describe("a web fetch approval", () => {
    function fetchDetail(
      listed: "yes" | "no",
      approval: Partial<ApprovalView> = {},
      // `unavailable` adds no note and no button, so the tests above see only the caution.
      allowHost: AllowHostView | null = { state: "unavailable" },
    ) {
      return detail(
        {
          allow_host: allowHost,
          tool: "web_fetch",
          fields: [
            { label: "URL", value: "https://docs.example.com/guide?page=2", multiline: false },
            { label: "Host", value: "docs.example.com", multiline: false },
            { label: "Host on the allowlist now", value: listed, multiline: false },
            { label: "Offset", value: "0", multiline: false },
            { label: "Maximum bytes", value: "12288", multiline: false },
          ],
        },
        {
          operation: "network",
          summary: "Fetch https://docs.example.com/guide?page=2",
          ...approval,
        },
      );
    }

    it("is labelled Web fetch, not Network", async () => {
      approvals = [view({ operation: "network" })];
      current = fetchDetail("yes");
      await start();
      await select();

      expect(operationLabel("network")).toBe("Web fetch");
      expect(root.querySelector(".review-title")?.textContent).toBe("Web fetch");
      expect(root.querySelector(".approval")?.textContent).toContain("Web fetch");
      expect(root.textContent).not.toContain("Network");
    });

    it("shows the review fields as text", async () => {
      approvals = [view({ operation: "network" })];
      current = fetchDetail("yes");
      await start();
      await select();

      const review = root.querySelector(".review") as HTMLElement;
      for (const text of [
        "URL",
        "https://docs.example.com/guide?page=2",
        "Host",
        "docs.example.com",
        "Host on the allowlist now",
        "Offset",
        "Maximum bytes",
        "12288",
      ]) {
        expect(review.textContent).toContain(text);
      }
    });

    it("shows the fixed caution and no allowlist statement when the host is listed", async () => {
      approvals = [view({ operation: "network" })];
      current = fetchDetail("yes");
      await start();
      await select();

      expect(NETWORK_CAUTION).toBe(
        "Approving sends this exact URL, including its path and query, to that host.",
      );
      expect(Array.from(root.querySelectorAll(".network-note")).map((n) => n.textContent)).toEqual([
        NETWORK_CAUTION,
      ]);
      expect(root.textContent).not.toContain(NOT_ON_ALLOWLIST_TEXT);
    });

    it("says the host is not on the workspace allowlist when the review says no", async () => {
      approvals = [view({ operation: "network" })];
      current = fetchDetail("no");
      await start();
      await select();

      expect(NOT_ON_ALLOWLIST_TEXT).toBe("This host is not on the workspace allowlist.");
      expect(Array.from(root.querySelectorAll(".network-note")).map((n) => n.textContent)).toEqual([
        NETWORK_CAUTION,
        NOT_ON_ALLOWLIST_TEXT,
      ]);
    });

    it("shows no caution for a command approval or a decided fetch", async () => {
      await start();
      await select();
      expect(root.querySelector(".network-note")).toBeNull();

      approvals = [view({ operation: "network", state: "approved" })];
      current = fetchDetail("no", { state: "approved" });
      await start();
      expect(root.querySelector(".network-note")).toBeNull();
    });

    it("never shows the writers-stopped checkbox and approves with writers_stopped false", async () => {
      approvals = [view({ operation: "network" })];
      current = fetchDetail("no");
      await start();
      await select();
      await settle(1000);

      expect(root.querySelector('input[type="checkbox"]')).toBeNull();
      expect(root.querySelector(".review")?.textContent).not.toContain(
        "stopped every other program",
      );
      const approve = buttonByText(root, "Approve");
      expect(approve.disabled).toBe(false);
      approve.click();
      await settle();

      expect(ipc.callsTo("approval_decide")[0]?.args).toEqual({
        id: "a1",
        decision: "approve",
        writers_stopped: false,
        review_token: "tok-1",
      });
    });

    describe("Allow host (ADR-0110)", () => {
      const offered: AllowHostView = { state: "offered", host: "docs.example.com" };

      function decisionButtons(): string[] {
        return Array.from(root.querySelectorAll(".decision button")).map(
          (button) => button.textContent ?? "",
        );
      }

      it("offers a third button between Deny and Approve, gated like Approve", async () => {
        approvals = [view({ operation: "network" })];
        current = fetchDetail("no", {}, offered);
        await start();
        await select();

        expect(ALLOW_HOST_LABEL).toBe("Allow host");
        expect(decisionButtons()).toEqual(["Deny", "Allow host", "Approve"]);
        const allowHost = buttonByText(root, ALLOW_HOST_LABEL);
        expect(allowHost.disabled).toBe(true);
        expect(allowHost.title).toBe("Approve, and add docs.example.com to the allowed web hosts");
        expect(
          Array.from(root.querySelectorAll(".network-note")).map((n) => n.textContent),
        ).toEqual([NETWORK_CAUTION, NOT_ON_ALLOWLIST_TEXT, allowHostText("docs.example.com")]);
        await settle(1000);
        expect(allowHost.disabled).toBe(false);
      });

      it("sends the token and no host, and says the host is now allowed", async () => {
        approvals = [view({ operation: "network" })];
        current = fetchDetail("no", {}, offered);
        await start();
        await select();
        await settle(1000);
        buttonByText(root, ALLOW_HOST_LABEL).click();
        await settle();

        expect(ipc.callsTo("approval_decide")[0]?.args).toEqual({
          id: "a1",
          decision: "allow_host",
          writers_stopped: false,
          review_token: "tok-1",
        });
        expect(root.querySelector(".result")?.textContent).toBe(
          "Decision recorded: approved. docs.example.com is now an allowed web host.",
        );
        for (const label of ["Deny", "Allow host", "Approve"]) {
          expect(buttonByText(root, label).disabled).toBe(true);
        }
      });

      // Rust spends the token before the dialog and before core's checks, so every failed
      // Allow host loads a fresh review instead of keeping the spent token.
      it.each([
        ["cancelled", "No change was made.", ALLOW_HOST_CANCELLED_TEXT],
        [
          "web_host_duplicate",
          "That host is already listed.",
          "That host is already listed. (web_host_duplicate)",
        ],
        ["web_host_too_many", "The list is full.", "The list is full. (web_host_too_many)"],
        [
          "web_host_fetch_not_allow",
          "Web fetch is not allow.",
          "Web fetch is not allow. (web_host_fetch_not_allow)",
        ],
        [
          "allow_host_unavailable",
          "No host was offered.",
          "No host was offered. (allow_host_unavailable)",
        ],
      ])(
        "reviews again with a fresh token after Allow host fails with %s",
        async (code, message, notice) => {
          approvals = [view({ operation: "network" })];
          current = fetchDetail("no", {}, offered);
          await start();
          ipc.on("approval_decide", () => rejection(code, message));
          await select();
          await settle(1000);
          buttonByText(root, ALLOW_HOST_LABEL).click();
          await settle();

          expect(ipc.callsTo("approval_detail")).toHaveLength(3);
          expect(root.querySelector(".result")?.textContent).toBe(notice);
          expect(buttonByText(root, "Approve").disabled).toBe(true);
          await settle(1000);
          expect(buttonByText(root, "Approve").disabled).toBe(false);
        },
      );

      it.each([
        [{ state: "not_allow" } as AllowHostView, ALLOW_HOST_NOT_ALLOW_TEXT],
        [{ state: "full" } as AllowHostView, ALLOW_HOST_FULL_TEXT],
      ])("shows no Allow host for %o and says why", async (allowHost, note) => {
        approvals = [view({ operation: "network" })];
        current = fetchDetail("no", {}, allowHost);
        await start();
        await select();
        await settle(1000);

        expect(decisionButtons()).toEqual(["Deny", "Approve"]);
        expect(root.querySelector(".review")?.textContent).toContain(note);
      });

      it("renders a hostile host as text only", async () => {
        approvals = [view({ operation: "network" })];
        current = fetchDetail("no", {}, { state: "offered", host: HOSTILE_IMG });
        await start();
        await select();

        expect(root.querySelector(".review img")).toBeNull();
        expect(root.querySelector(".review")?.textContent).toContain(HOSTILE_IMG);
      });
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
    // Opened with the window, selected again, then reloaded after the rejection.
    expect(ipc.callsTo("approval_detail")).toHaveLength(3);
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
      expect(ipc.callsTo("approval_detail")).toHaveLength(2);
    });

    it("clamps long labels in the list", async () => {
      approvals = [view({ folder_display: "x".repeat(5000), summary: "y".repeat(5000) })];
      await start();
      expect(root.querySelector(".who")?.classList.contains("clamp-1")).toBe(true);
      expect(root.querySelector(".summary")?.classList.contains("clamp-1")).toBe(true);
    });

    it("lists a new request when the window gains focus and keeps the review", async () => {
      approvals = [view({ id: "a1", expires_unix: 2000 })];
      await start();
      rowFor("a1").click();
      await settle();
      const review = root.querySelector(".review") as HTMLElement;
      const listCalls = ipc.callsTo("approvals_list").length;
      approvals = [view({ id: "a1", expires_unix: 2000 }), view({ id: "a2", expires_unix: 1500 })];
      window.dispatchEvent(new Event("focus"));
      await settle();
      expect(ipc.callsTo("approvals_list")).toHaveLength(listCalls + 1);
      expect(rowIds()).toEqual(["a2", "a1"]);
      expect(rowFor("a1").getAttribute("aria-pressed")).toBe("true");
      expect(rowFor("a2").getAttribute("aria-pressed")).toBe("false");
      expect(root.querySelector(".review")).toBe(review);
    });

    it("stops refreshing on window focus once disposed", async () => {
      await start();
      app?.dispose();
      app = null;
      const listCalls = ipc.callsTo("approvals_list").length;
      window.dispatchEvent(new Event("focus"));
      await settle(LIST_POLL_MS);
      expect(ipc.callsTo("approvals_list")).toHaveLength(listCalls);
    });
  });
});
