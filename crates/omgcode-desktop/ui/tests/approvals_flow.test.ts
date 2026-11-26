import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  type ApprovalsApp,
  QUEUE_HEIGHT,
  QUEUE_WIDTH,
  STACKED_QUERY,
  startApprovals,
} from "../src/approvals/app";
import { setInvoker } from "../src/ipc/client";
import type { ApprovalDetail, ApprovalView } from "../src/ipc/types";
import {
  buttonByText,
  type FakeIpc,
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
    summary: `summary of ${overrides.id ?? "a1"}`,
    state: "pending",
    created_unix: 900,
    expires_unix: 1065,
    request_sha256: "f".repeat(64),
    writers_stopped: false,
    ...overrides,
  };
}

function detailFor(id: string): ApprovalDetail {
  const approval = view({ id });
  return {
    approval,
    binding: "exact",
    tool: "workspace_run_command",
    fields: [{ label: "Program", value: `program-${id}`, multiline: false }],
    review_token: `tok-${id}`,
    waiting: true,
  };
}

describe("approvals window flow", () => {
  let app: ApprovalsApp | null = null;
  let root: HTMLElement;
  let ipc: FakeIpc;
  let approvals: ApprovalView[];

  async function start(): Promise<void> {
    ipc = installFakeIpc({
      approvals_list: () => ({ now_unix: 1000, approvals }),
      approval_detail: (args) => {
        const id = String(args.id);
        const listed = approvals.find((a) => a.id === id);
        return { ...detailFor(id), approval: listed ?? view({ id }) };
      },
      approval_decide: (args) => {
        const id = String(args.id);
        const state = args.decision === "approve" ? "approved" : "denied";
        approvals = approvals.map((a) => (a.id === id ? { ...a, state } : a));
        return { ...view({ id }), state };
      },
    });
    root = mountRoot();
    app = startApprovals(root);
    await settle();
  }

  function rowFor(id: string): HTMLButtonElement {
    const found = root.querySelector<HTMLButtonElement>(`[data-approval-id="${id}"]`);
    if (found === null) {
      throw new Error(`No row for ${id}`);
    }
    return found;
  }

  function pressedIds(): string[] {
    return Array.from(root.querySelectorAll<HTMLElement>('[aria-pressed="true"]')).map(
      (n) => n.dataset.approvalId ?? "",
    );
  }

  function detailIds(): string[] {
    return ipc.callsTo("approval_detail").map((c) => String(c.args.id));
  }

  function press(target: Element, key: string): void {
    target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  }

  beforeEach(() => {
    vi.useFakeTimers();
    approvals = [
      view({ id: "late", expires_unix: 3000 }),
      view({ id: "soon", expires_unix: 1100 }),
      view({ id: "mid", expires_unix: 2000 }),
    ];
  });

  afterEach(() => {
    app?.dispose();
    app = null;
    setInvoker(null);
    vi.useRealTimers();
  });

  describe("layout", () => {
    it("puts the queue, a splitter, and the review in order and shows counts", async () => {
      approvals = [...approvals, view({ id: "d1", state: "denied" })];
      await start();
      const body = root.querySelector(".approvals-body") as HTMLElement;
      expect(body.children[0]?.classList.contains("queue")).toBe(true);
      expect(body.children[1]?.getAttribute("role")).toBe("separator");
      expect(body.children[2]?.classList.contains("review-pane")).toBe(true);
      expect(root.querySelector('[data-tab="pending"]')?.textContent).toBe("Pending (3)");
      expect(root.querySelector('[data-tab="history"]')?.textContent).toBe("History (1)");
      const badge = root.querySelector(".badge") as HTMLElement;
      expect(badge.textContent).toBe("3");
      expect(badge.hidden).toBe(false);
      expect(root.querySelector("h1")?.textContent).toBe("Approvals");
    });

    it("resizes the queue beside the review with its splitter (ADR-0101)", async () => {
      await start();
      const queue = root.querySelector(".queue") as HTMLElement;
      const splitter = root.querySelector('[role="separator"]') as HTMLElement;
      expect(splitter.getAttribute("aria-label")).toBe("Resize the approval list");
      expect(splitter.getAttribute("aria-controls")).toBe(queue.id);
      expect(splitter.getAttribute("aria-orientation")).toBe("vertical");
      expect(queue.style.getPropertyValue("--queue-width")).toBe(`${QUEUE_WIDTH.initial}px`);
      splitter.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
      expect(queue.style.getPropertyValue("--queue-width")).toBe(
        `${(QUEUE_WIDTH.initial ?? 0) + 16}px`,
      );
      splitter.dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true }));
      expect(queue.style.getPropertyValue("--queue-width")).toBe(`${QUEUE_WIDTH.max}px`);
      splitter.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
      expect(queue.style.getPropertyValue("--queue-width")).toBe(`${QUEUE_WIDTH.initial}px`);
    });

    it("turns the splitter horizontal and sets the height when the panes stack", async () => {
      const query = vi.fn((text: string) => ({ matches: text === STACKED_QUERY }));
      vi.stubGlobal("matchMedia", query);
      try {
        await start();
        const queue = root.querySelector(".queue") as HTMLElement;
        const splitter = root.querySelector('[role="separator"]') as HTMLElement;
        expect(splitter.getAttribute("aria-orientation")).toBe("horizontal");
        splitter.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
        expect(queue.style.getPropertyValue("--queue-height")).toBe(
          `${(QUEUE_HEIGHT.initial ?? 0) + 16}px`,
        );
        // Left and right do nothing to a horizontal splitter.
        splitter.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
        expect(queue.style.getPropertyValue("--queue-height")).toBe(
          `${(QUEUE_HEIGHT.initial ?? 0) + 16}px`,
        );
      } finally {
        vi.unstubAllGlobals();
      }
    });

    it("stacks at the same width that approvals.css names", () => {
      const css = Object.values(
        import.meta.glob<string>("../src/styles/approvals.css", {
          query: "?raw",
          import: "default",
          eager: true,
        }),
      )[0];
      expect(css).toContain(`@media ${STACKED_QUERY}`);
    });

    it("shows empty states and hides the badge when nothing is pending", async () => {
      approvals = [];
      await start();
      expect(root.textContent).toContain("No approvals are waiting.");
      expect((root.querySelector(".badge") as HTMLElement).hidden).toBe(true);
      expect((root.querySelector(".review") as HTMLElement).hidden).toBe(true);
      expect((root.querySelector(".review-empty") as HTMLElement).hidden).toBe(false);
    });

    it("shows history rows with a state chip and no button", async () => {
      approvals = [
        view({ id: "d1", state: "approved" }),
        view({ id: "d2", state: "denied" }),
        view({ id: "d3", state: "expired" }),
      ];
      await start();
      const history = Array.from(root.querySelectorAll(".approval.decided"));
      expect(history).toHaveLength(3);
      for (const row of history) {
        expect(row.querySelector("button")).toBeNull();
      }
      expect(history[0]?.querySelector(".chip-approved, .chip-denied")).not.toBeNull();
      const chips = history.map((r) => r.querySelector(".chip")?.className);
      expect(chips.filter((c) => c?.includes("chip-approved"))).toHaveLength(1);
      expect(chips.filter((c) => c?.includes("chip-denied"))).toHaveLength(1);
      expect(chips.filter((c) => c === "chip")).toHaveLength(1);
    });

    it("keeps the decision row after many decided records", async () => {
      const decided = Array.from({ length: 50 }, (_, i) =>
        view({ id: `d${i}`, state: i % 2 === 0 ? "approved" : "denied", created_unix: 100 + i }),
      );
      approvals = [view({ id: "p1" }), ...decided];
      await start();
      const review = root.querySelector(".review") as HTMLElement;
      const actions = review.querySelector(".review-actions") as HTMLElement;
      expect(actions).not.toBeNull();
      expect(hasButton(actions, "Deny")).toBe(true);
      expect(hasButton(actions, "Approve")).toBe(true);
      // No decided record sits in the review pane or before the review in the document.
      expect(review.closest(".review-pane")?.querySelector(".approval")).toBeNull();
      expect(root.querySelectorAll(".approval.decided")).toHaveLength(50);
      const queue = root.querySelector(".queue") as HTMLElement;
      expect(queue.contains(review)).toBe(false);
    });

    it("moves the request digest, tool id and binding into collapsed technical details", async () => {
      await start();
      const details = root.querySelector("details.technical") as HTMLDetailsElement;
      expect(details.open).toBe(false);
      expect(details.querySelector("summary")?.textContent).toBe("Technical details");
      expect(details.textContent).toContain("f".repeat(64));
      expect(details.textContent).toContain("workspace_run_command");
      expect(details.textContent).toContain("Exact match");
    });

    it("says whether a turn is waiting in a chip", async () => {
      await start();
      expect(root.querySelector(".review-head .chip-waiting")?.textContent).toBe("Turn waiting");
      const other = detailFor("soon");
      other.waiting = false;
      ipc.on("approval_detail", () => other);
      rowFor("soon").click();
      await settle();
      expect(root.querySelector(".review-head .chip")?.textContent).toBe("No turn waiting");
      expect(root.querySelector(".effect")?.textContent).toContain("Approving runs nothing now");
    });
  });

  it("links the Pending and History tabs to their panels", async () => {
    await start();
    for (const name of ["pending", "history"]) {
      const tab = root.querySelector(`[data-tab="${name}"]`) as HTMLElement;
      const panel = document.getElementById(tab.getAttribute("aria-controls") ?? "");
      expect(panel?.getAttribute("role")).toBe("tabpanel");
      expect(panel?.getAttribute("aria-labelledby")).toBe(tab.id);
    }
  });

  describe("selection", () => {
    it("selects the soonest-expiring pending request when the window opens", async () => {
      await start();
      expect(detailIds()).toEqual(["soon"]);
      expect(pressedIds()).toEqual(["soon"]);
      expect(root.querySelector(".review")?.textContent).toContain("program-soon");
      expect(document.activeElement).toBe(rowFor("soon"));
    });

    it("keeps the selection when a poll adds a request that expires sooner", async () => {
      await start();
      rowFor("mid").click();
      await settle();
      const calls = detailIds().length;
      approvals = [view({ id: "new", expires_unix: 1001 }), ...approvals];
      await settle(3000);
      expect(rowFor("new")).toBeTruthy();
      expect(pressedIds()).toEqual(["mid"]);
      expect(detailIds()).toHaveLength(calls);
      expect(root.querySelector(".review")?.textContent).toContain("program-mid");
    });

    it("selects the next request when the selected one is decided elsewhere", async () => {
      await start();
      approvals = approvals.map((a) => (a.id === "soon" ? { ...a, state: "denied" } : a));
      await settle(3000);
      expect(pressedIds()).toEqual(["mid"]);
      expect(detailIds()).toEqual(["soon", "mid"]);
      expect(root.querySelector(".review")?.textContent).toContain("program-mid");
    });

    it("selects the next request when the selected one is gone, and clears at the end", async () => {
      await start();
      approvals = approvals.filter((a) => a.id !== "soon");
      await settle(3000);
      expect(pressedIds()).toEqual(["mid"]);
      approvals = [];
      await settle(3000);
      expect(pressedIds()).toEqual([]);
      expect((root.querySelector(".review") as HTMLElement).hidden).toBe(true);
    });

    it("does not move focus when a poll changes the selection", async () => {
      await start();
      const other = rowFor("late");
      other.focus();
      approvals = approvals.filter((a) => a.id !== "soon");
      await settle(3000);
      expect(pressedIds()).toEqual(["mid"]);
      expect(document.activeElement).toBe(other);
    });

    it("advances to the next request after a decision and focuses the review heading", async () => {
      await start();
      buttonByText(root, "Deny").click();
      await settle();
      expect(pressedIds()).toEqual(["mid"]);
      expect(root.querySelector(".review")?.textContent).toContain("program-mid");
      expect(root.querySelector(".notice")?.textContent).toBe("Decision recorded: denied.");
      const heading = root.querySelector(".review-title") as HTMLElement;
      expect(heading.getAttribute("tabindex")).toBe("-1");
      expect(document.activeElement).toBe(heading);
      expect(document.activeElement).not.toBe(buttonByText(root, "Approve"));
    });

    it("keeps showing the decided record when nothing else is pending", async () => {
      approvals = [view({ id: "only" })];
      await start();
      buttonByText(root, "Deny").click();
      await settle(3000);
      expect(root.querySelector(".result")?.textContent).toContain("denied");
      expect((root.querySelector(".review") as HTMLElement).hidden).toBe(false);
    });
  });

  describe("fresh reviews", () => {
    it("restarts the Approve delay for each newly shown review", async () => {
      await start();
      await settle(1000);
      expect(buttonByText(root, "Approve").disabled).toBe(false);
      rowFor("mid").click();
      await settle();
      expect(buttonByText(root, "Approve").disabled).toBe(true);
      await settle(999);
      expect(buttonByText(root, "Approve").disabled).toBe(true);
      await settle(1);
      expect(buttonByText(root, "Approve").disabled).toBe(false);
    });

    it("restarts the delay for the request chosen after a decision", async () => {
      await start();
      await settle(1000);
      buttonByText(root, "Deny").click();
      await settle();
      expect(buttonByText(root, "Approve").disabled).toBe(true);
      await settle(1000);
      expect(buttonByText(root, "Approve").disabled).toBe(false);
    });

    it("unticks writers-stopped for each newly shown review", async () => {
      approvals = approvals.map((a) => ({ ...a, operation: "file_write" }));
      await start();
      await settle(1000);
      const box = root.querySelector('input[type="checkbox"]') as HTMLInputElement;
      box.checked = true;
      box.dispatchEvent(new Event("change", { bubbles: true }));
      expect(buttonByText(root, "Approve").disabled).toBe(false);
      rowFor("mid").click();
      await settle(1000);
      const fresh = root.querySelector('input[type="checkbox"]') as HTMLInputElement;
      expect(fresh.checked).toBe(false);
      expect(buttonByText(root, "Approve").disabled).toBe(true);
    });

    it("shows only the newest review when two answers for one request arrive out of order", async () => {
      await start();
      const answers: Array<(detail: ApprovalDetail) => void> = [];
      ipc.on(
        "approval_detail",
        () =>
          new Promise<ApprovalDetail>((resolve) => {
            answers.push(resolve);
          }),
      );
      const issued = (n: number): ApprovalDetail => ({
        ...detailFor("mid"),
        approval: view({ id: "mid", expires_unix: 2000 }),
        fields: [{ label: "Program", value: `program-mid-${n}`, multiline: false }],
        review_token: `tok-mid-${n}`,
      });
      rowFor("mid").click();
      rowFor("mid").click();
      await settle();
      expect(answers).toHaveLength(2);
      // The second request's answer arrives first; the older one must not replace it.
      answers[1]?.(issued(2));
      await settle();
      answers[0]?.(issued(1));
      await settle(1000);
      const review = root.querySelector(".review")?.textContent ?? "";
      expect(review).toContain("program-mid-2");
      expect(review).not.toContain("program-mid-1");
      buttonByText(root, "Approve").click();
      await settle();
      expect(ipc.callsTo("approval_decide").at(-1)?.args.review_token).toBe("tok-mid-2");
    });

    it("keeps the newly selected review when an earlier decision's review expires", async () => {
      await start();
      await settle(1000);
      let failDecision: () => void = () => undefined;
      ipc.on(
        "approval_decide",
        () =>
          new Promise<never>((_resolve, reject) => {
            failDecision = () => reject({ code: "review_expired", message: "Expired." });
          }),
      );
      let answerMid: (detail: ApprovalDetail) => void = () => undefined;
      ipc.on(
        "approval_detail",
        () =>
          new Promise<ApprovalDetail>((resolve) => {
            answerMid = resolve;
          }),
      );
      buttonByText(root, "Approve").click();
      await settle();
      rowFor("mid").click();
      await settle();
      // The decision for "soon" fails while "mid" is selected and its review is still loading.
      failDecision();
      await settle();
      answerMid(detailFor("mid"));
      await settle();
      expect(pressedIds()).toEqual(["mid"]);
      expect(root.querySelector(".review")?.textContent).toContain("program-mid");
      expect(detailIds().filter((id) => id === "soon")).toHaveLength(1);
    });

    it("keeps the new review's Approve delay when an earlier decision finishes late", async () => {
      await start();
      await settle(1000);
      let finish: () => void = () => undefined;
      ipc.on(
        "approval_decide",
        (args) =>
          new Promise<ApprovalView>((resolve) => {
            finish = () => resolve({ ...view({ id: String(args.id) }), state: "denied" });
          }),
      );
      buttonByText(root, "Deny").click();
      await settle();
      rowFor("mid").click();
      await settle();
      expect(buttonByText(root, "Approve").disabled).toBe(true);
      finish();
      await settle(1000);
      expect(root.querySelector(".review")?.textContent).toContain("program-mid");
      expect(buttonByText(root, "Approve").disabled).toBe(false);
    });

    it("keeps a failed review load on screen across list polls without retrying", async () => {
      await start();
      ipc.on("approval_detail", () => rejection("approval_not_found", "Gone."));
      rowFor("mid").click();
      await settle();
      const error = root.querySelector(".approvals > .error") as HTMLElement;
      expect(error.hidden).toBe(false);
      expect(error.textContent).toContain("approval_not_found");
      const loads = detailIds().length;
      await settle(6000);
      expect(error.hidden).toBe(false);
      expect(error.textContent).toContain("approval_not_found");
      expect(detailIds()).toHaveLength(loads);
      ipc.on("approval_detail", (args) => detailFor(String(args.id)));
      rowFor("late").click();
      await settle();
      expect(error.hidden).toBe(true);
    });

    it("never focuses Approve, when it renders or when it unlocks", async () => {
      await start();
      expect(document.activeElement).not.toBe(buttonByText(root, "Approve"));
      rowFor("mid").click();
      await settle();
      const heading = root.querySelector(".review-title");
      expect(document.activeElement).toBe(heading);
      await settle(1000);
      expect(buttonByText(root, "Approve").disabled).toBe(false);
      expect(document.activeElement).toBe(heading);
      expect(document.activeElement).not.toBe(buttonByText(root, "Approve"));
    });
  });

  describe("keyboard", () => {
    it("moves the selection with ArrowDown and ArrowUp and loads each detail", async () => {
      await start();
      press(rowFor("soon"), "ArrowDown");
      await settle();
      expect(pressedIds()).toEqual(["mid"]);
      expect(document.activeElement).toBe(rowFor("mid"));
      expect(root.querySelector(".review")?.textContent).toContain("program-mid");
      press(rowFor("mid"), "ArrowDown");
      await settle();
      expect(pressedIds()).toEqual(["late"]);
      press(rowFor("late"), "ArrowDown");
      await settle();
      expect(pressedIds()).toEqual(["late"]);
      press(rowFor("late"), "ArrowUp");
      await settle();
      expect(pressedIds()).toEqual(["mid"]);
      expect(detailIds()).toEqual(["soon", "mid", "late", "mid"]);
      expect(document.activeElement).toBe(rowFor("mid"));
    });

    it("returns focus to the selected row on Escape inside the review", async () => {
      await start();
      rowFor("mid").click();
      await settle();
      const heading = root.querySelector(".review-title") as HTMLElement;
      expect(document.activeElement).toBe(heading);
      press(heading, "Escape");
      expect(document.activeElement).toBe(rowFor("mid"));
    });

    it("adds no shortcut that approves or denies", async () => {
      await start();
      await settle(1000);
      const keys = ["a", "y", "d", "n", "Enter", " ", "Delete"];
      for (const key of keys) {
        press(document.body, key);
        press(root.querySelector(".review-title") as Element, key);
        press(rowFor("soon"), key);
        document.dispatchEvent(new KeyboardEvent("keydown", { key, ctrlKey: true, bubbles: true }));
      }
      await settle();
      expect(ipc.callsTo("approval_decide")).toHaveLength(0);
    });
  });
});
