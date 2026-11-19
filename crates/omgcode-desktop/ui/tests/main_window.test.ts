import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { SearchResult, SessionSummary } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import { PLACEHOLDER_CTRL_ENTER, PLACEHOLDER_ENTER } from "../src/main/composer";
import { createMockBackend } from "../src/mock/backend";
import { HOSTILE } from "../src/mock/fixtures";
import {
  buttonByText,
  type FakeIpc,
  type Handler,
  hasButton,
  installFakeIpc,
  mainHandlers,
  mountRoot,
  page,
  rejection,
  SETTINGS,
  session,
  settle,
  tree,
  userMessage,
} from "./helpers";

let app: MainApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;

async function start(handlers: Record<string, Handler> = {}): Promise<void> {
  ipc = installFakeIpc(mainHandlers(handlers));
  root = mountRoot();
  app = startMain(root);
  await settle();
}

function treeOf(...sessions: SessionSummary[]): Handler {
  return () => tree([{ id: "g1", folder: "C:\\work\\one", sessions }]);
}

function itemFor(id: string): HTMLElement {
  const found = Array.from(root.querySelectorAll<HTMLElement>("[data-session-id]")).find(
    (node) => node.dataset.sessionId === id,
  );
  if (found === undefined) {
    throw new Error(`No tree item for ${id}`);
  }
  return found;
}

function type(input: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  app?.dispose();
  app = null;
  setInvoker(null);
  vi.useRealTimers();
});

describe("session tree", () => {
  it("groups sessions and shows status chips", async () => {
    const idle = session({ session_id: "s-idle", title: "Idle one" });
    const running = session({ session_id: "s-run", title: "Running one", status: "running" });
    const open = session({ session_id: "s-open", title: "Open turn", status: "turn_open" });
    const busy = session({ session_id: "s-busy", title: "Busy one", status: "busy" });
    const bad = session({
      session_id: "s-bad",
      title: "Bad one",
      status: "invalid",
      invalid_code: "journal_hash_mismatch",
      updated_at: null,
    });
    await start({
      session_tree: () =>
        tree(
          [
            { id: "g1", folder: "C:\\work\\one", sessions: [idle, running] },
            { id: "g2", folder: "/home/dev/two", sessions: [open, busy, bad] },
          ],
          2,
        ),
    });
    expect(root.querySelector('[role="tree"]')).not.toBeNull();
    const groups = root.querySelectorAll('[data-kind="group"]');
    expect(groups).toHaveLength(2);
    expect(groups[0]?.textContent).toContain("C:\\work\\one");
    expect(itemFor("s-idle").querySelector(".chip")).toBeNull();
    expect(itemFor("s-run").textContent).toContain("Running");
    expect(itemFor("s-open").textContent).toContain("Turn open");
    expect(itemFor("s-busy").textContent).toContain("In use");
    expect(itemFor("s-bad").textContent).toContain("Unreadable");
    expect(itemFor("s-bad").textContent).toContain("journal_hash_mismatch");
    expect(itemFor("s-bad").textContent).toContain("unknown");
    expect(root.textContent).toContain("2 entries in session storage were skipped.");
    expect(root.querySelectorAll("[title]")).toHaveLength(0);
  });

  it("explains an empty store and an empty archive", async () => {
    await start();
    expect(root.textContent).toContain("No sessions yet");
    expect(root.textContent).toContain("OMGCode CLI");
    buttonByText(root, "Archived").click();
    await settle();
    expect(root.textContent).toContain("No archived sessions.");
  });

  it("passes view, sort and direction to session_tree", async () => {
    await start();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toEqual({
      view: "active",
      sort: "updated",
      descending: true,
    });
    buttonByText(root, "Archived").click();
    await settle();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toMatchObject({ view: "archived" });
    const select = root.querySelector("select") as HTMLSelectElement;
    select.value = "title";
    select.dispatchEvent(new Event("change"));
    await settle();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toMatchObject({ sort: "title" });
    buttonByText(root, "Descending").click();
    await settle();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toEqual({
      view: "archived",
      sort: "title",
      descending: false,
    });
  });

  it("supports arrow-key navigation and collapsing groups", async () => {
    const a = session({ session_id: "s-a", title: "A" });
    const b = session({ session_id: "s-b", title: "B" });
    await start({ session_tree: treeOf(a, b) });
    const group = root.querySelector<HTMLElement>('[data-kind="group"]') as HTMLElement;
    group.focus();
    group.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    expect(document.activeElement).toBe(itemFor("s-a"));
    itemFor("s-a").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    expect(document.activeElement).toBe(itemFor("s-b"));
    itemFor("s-b").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
    expect(document.activeElement).toBe(root.querySelector('[data-kind="group"]'));
    document.activeElement?.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }),
    );
    expect(root.querySelector('[data-kind="group"]')?.getAttribute("aria-expanded")).toBe("false");
    expect(root.querySelector(".tree-children")?.hasAttribute("hidden")).toBe(true);
  });
});

describe("search", () => {
  const hit: SearchResult = {
    truncated: true,
    hits: [
      {
        session: session({ session_id: "s-hit", title: "Hit session" }),
        folder_display: "C:\\work",
        seq: 2,
        role: "assistant",
        snippet: [
          { text: "before ", matched: false },
          { text: "needle", matched: true },
          { text: " after", matched: false },
        ],
      },
    ],
  };

  it("debounces by 250 ms and marks matched parts", async () => {
    await start({ session_search: () => hit });
    const input = root.querySelector('input[type="search"]') as HTMLInputElement;
    type(input, "need");
    await settle(249);
    expect(ipc.callsTo("session_search")).toHaveLength(0);
    await settle(1);
    expect(ipc.callsTo("session_search")).toHaveLength(1);
    expect(ipc.callsTo("session_search")[0]?.args).toEqual({
      query: "need",
      view: "active",
      limit: 50,
    });
    const marks = root.querySelectorAll("mark");
    expect(marks).toHaveLength(1);
    expect(marks[0]?.textContent).toBe("needle");
    expect(root.textContent).toContain("More results exist; refine the search.");
  });

  it("clears the query with Escape and restores the tree", async () => {
    await start({ session_search: () => hit });
    const input = root.querySelector('input[type="search"]') as HTMLInputElement;
    type(input, "need");
    await settle(250);
    const treeCalls = ipc.callsTo("session_tree").length;
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();
    expect(input.value).toBe("");
    expect(ipc.callsTo("session_tree").length).toBe(treeCalls + 1);
    expect(root.querySelector("mark")).toBeNull();
  });

  it("opens the hit and scrolls to the matching entry", async () => {
    const s = hit.hits[0]?.session as SessionSummary;
    await start({
      session_search: () => hit,
      session_transcript: () => page(s, [userMessage(1, "one"), userMessage(2, "needle")]),
    });
    type(root.querySelector('input[type="search"]') as HTMLInputElement, "need");
    await settle(250);
    (root.querySelector(".hit") as HTMLButtonElement).click();
    await settle();
    expect(root.querySelector('.entry.highlight[data-seq="2"]')).not.toBeNull();
    expect(root.querySelector('.entry.highlight[data-seq="1"]')).toBeNull();
  });
});

describe("long labels", () => {
  const folder = `${"f".repeat(5000)}\u202e`;
  const title = `${"t".repeat(300)}\u202e`;
  const s = session({ session_id: "s-long", title });

  it("clamps tree labels and search hits", async () => {
    await start({
      session_tree: () => tree([{ id: "g1", folder, sessions: [s] }]),
      session_search: () => ({
        truncated: false,
        hits: [{ session: s, folder_display: folder, seq: 1, role: "user", snippet: [] }],
      }),
    });
    expect(root.querySelector(".group-row .tree-title")?.classList.contains("clamp-2")).toBe(true);
    expect(root.querySelector(".tree-session .tree-title")?.classList.contains("clamp-2")).toBe(
      true,
    );
    type(root.querySelector('input[type="search"]') as HTMLInputElement, "x");
    await settle(250);
    expect(root.querySelector(".hit-title")?.classList.contains("clamp-1")).toBe(true);
    expect(root.querySelector(".hit-meta")?.classList.contains("clamp-1")).toBe(true);
    expect(root.querySelector(".hit-snippet")?.classList.contains("clamp-3")).toBe(true);
  });

  it("clamps the conversation header and offers the full text under Details", async () => {
    await start({
      session_tree: () => tree([{ id: "g1", folder, sessions: [s] }]),
      session_transcript: () => page(s, [userMessage(1, "hi")], { folder_display: folder }),
    });
    itemFor("s-long").click();
    await settle();
    const header = root.querySelector(".pane-header") as HTMLElement;
    expect(header.querySelector("h2")?.classList.contains("clamp-2")).toBe(true);
    expect(header.querySelector(".pane-folder")?.classList.contains("clamp-1")).toBe(true);
    const details = header.querySelector("details") as HTMLDetailsElement;
    expect(details.querySelector("summary")?.textContent).toBe("Details");
    expect(details.querySelector(".pane-full-title")?.textContent).toBe(neutralise(title));
    expect(details.querySelector(".pane-full-folder")?.textContent).toBe(neutralise(folder));
    expect(details.querySelector("[title]")).toBeNull();
  });
});

describe("hostile content", () => {
  it("renders fixture hostile strings as inert text", async () => {
    setInvoker(createMockBackend().invoke);
    root = mountRoot();
    app = startMain(root);
    await settle();
    expect(root.textContent).toContain(HOSTILE.img);
    itemFor("00000000-0000-4000-8000-000000000001").click();
    await settle();
    expect(root.textContent).toContain(HOSTILE.long);
    expect(root.textContent).toContain(HOSTILE.link);
    expect(root.textContent).toContain("[U+202E]");
    itemFor("00000000-0000-4000-8000-000000000005").click();
    await settle();
    expect(root.textContent).toContain(`Remove ${HOSTILE.img}`);
    for (const tag of ["img", "script", "a"]) {
      expect(document.querySelectorAll(tag)).toHaveLength(0);
    }
    expect(document.querySelectorAll("[onerror], [style], [href]")).toHaveLength(0);
  });
});

describe("conversation", () => {
  const s = session({ session_id: "s-one", title: "Conversation one" });

  it("loads the first page of 200 entries and pages on demand", async () => {
    const pages = [
      page(s, [userMessage(1, "first")], { next_after_seq: 1 }),
      page(s, [userMessage(2, "second")]),
    ];
    await start({
      session_tree: treeOf(s),
      session_transcript: (args) => (args.after_seq === null ? pages[0] : pages[1]),
    });
    itemFor("s-one").click();
    await settle();
    expect(ipc.callsTo("session_transcript")[0]?.args).toEqual({
      session_id: "s-one",
      after_seq: null,
      limit: 200,
    });
    expect(root.querySelector(".pane-header")?.textContent).toContain("Conversation one");
    expect(root.textContent).toContain("first");
    expect(root.textContent).not.toContain("second");
    buttonByText(root, "Load more").click();
    await settle();
    expect(ipc.callsTo("session_transcript")[1]?.args).toMatchObject({ after_seq: 1 });
    expect(root.textContent).toContain("second");
    expect(hasButton(root, "Load more")).toBe(false);
  });

  it("shows a banner for an incomplete tail", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "x")], { incomplete_tail: true }),
    });
    itemFor("s-one").click();
    await settle();
    expect(root.textContent).toContain("The end of this journal is still being written");
  });

  it("explains busy and invalid sessions instead of a transcript", async () => {
    const busy = session({ session_id: "s-busy", title: "Busy", status: "busy" });
    const bad = session({
      session_id: "s-bad",
      title: "Bad",
      status: "invalid",
      invalid_code: "journal_hash_mismatch",
    });
    await start({
      session_tree: treeOf(busy, bad),
      session_transcript: (args) =>
        args.session_id === "s-busy"
          ? rejection("busy", "Held by another process.")
          : rejection("invalid_journal", "Hash mismatch."),
    });
    itemFor("s-busy").click();
    await settle();
    expect(root.querySelector(".panel")?.textContent).toContain("Another process is writing");
    expect(root.querySelector(".panel")?.textContent).toContain("Held by another process.");
    expect((root.querySelector(".composer") as HTMLElement).hidden).toBe(true);
    itemFor("s-bad").click();
    await settle();
    expect(root.querySelector(".panel")?.textContent).toContain("failed validation");
    expect(root.querySelector(".panel")?.textContent).toContain("journal_hash_mismatch");
    expect((root.querySelector(".composer") as HTMLElement).hidden).toBe(true);
  });

  it("applies the composer and action visibility rules per status", async () => {
    const cases: Array<
      [SessionSummary, { composer: boolean; enabled: boolean; labels: string[] }]
    > = [
      [
        session({ session_id: "c1", title: "t1" }),
        { composer: true, enabled: true, labels: ["Archive"] },
      ],
      [
        session({ session_id: "c2", title: "t2", archived: true }),
        { composer: false, enabled: false, labels: ["Restore"] },
      ],
      [
        session({ session_id: "c3", title: "t3", status: "turn_open" }),
        { composer: true, enabled: false, labels: ["Interrupt"] },
      ],
      [
        session({ session_id: "c4", title: "t4", status: "running" }),
        { composer: true, enabled: false, labels: ["Cancel"] },
      ],
    ];
    await start({
      session_tree: treeOf(...cases.map(([c]) => c)),
      session_transcript: (args) =>
        page(cases.find(([c]) => c.session_id === args.session_id)?.[0] as SessionSummary, []),
      chat_status: (args) => ({
        session_id: args.session_id,
        state: "running",
        outcome: null,
        error: null,
      }),
    });
    for (const [summary, expected] of cases) {
      itemFor(summary.session_id).click();
      await settle();
      const header = root.querySelector(".pane-header") as HTMLElement;
      const composer = root.querySelector(".composer") as HTMLElement;
      expect(composer.hidden).toBe(!expected.composer);
      expect((root.querySelector(".message-box") as HTMLTextAreaElement).disabled).toBe(
        !expected.enabled,
      );
      for (const label of ["Archive", "Restore", "Interrupt", "Cancel"]) {
        expect(hasButton(header, label)).toBe(expected.labels.includes(label));
      }
    }
  });

  it("confirms Interrupt with a second explicit button", async () => {
    const open = session({ session_id: "s-open", title: "Open", status: "turn_open" });
    await start({
      session_tree: treeOf(open),
      session_transcript: () => page(open, []),
      session_interrupt: () => ({ ...open, status: "idle" }),
    });
    itemFor("s-open").click();
    await settle();
    buttonByText(root, "Interrupt").click();
    await settle();
    expect(ipc.callsTo("session_interrupt")).toHaveLength(0);
    expect(hasButton(root, "Confirm interrupt")).toBe(true);
    buttonByText(root, "Confirm interrupt").click();
    await settle();
    expect(ipc.callsTo("session_interrupt")[0]?.args).toEqual({ session_id: "s-open" });
  });

  it("archives an idle session", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      session_archive: () => ({ ...s, archived: true }),
    });
    itemFor("s-one").click();
    await settle();
    buttonByText(root.querySelector(".pane-header") as HTMLElement, "Archive").click();
    await settle();
    expect(ipc.callsTo("session_archive")[0]?.args).toEqual({ session_id: "s-one" });
  });
});

describe("chat lifecycle", () => {
  const s = session({ session_id: "s-chat", title: "Chat" });
  const running = { session_id: "s-chat", state: "running", outcome: null, error: null };

  async function startChat(statusReplies: unknown[]): Promise<void> {
    let n = 0;
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      chat_send: () => running,
      chat_cancel: () => ({ ...running, state: "cancelling" }),
      chat_status: () => statusReplies[Math.min(n++, statusReplies.length - 1)],
    });
    itemFor("s-chat").click();
    await settle();
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    buttonByText(root, "Send").click();
    await settle();
  }

  it("sends the message with the settings, polls, and reloads once completed", async () => {
    const outcome = {
      finish_reason: "stop",
      tools_mode: "disabled",
      tools_disabled_reason: "off",
      rounds: 1,
      tool_calls: 0,
      withheld_tool_results: 0,
      context: { estimated_tokens: 10, trigger_tokens: 60, context_tokens: 100, compacted: false },
    };
    await startChat([running, { ...running, state: "completed", outcome }]);
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({
      session_id: "s-chat",
      message: "hello",
      settings: { model: "test-model", context_tokens: 8192, auto_compact: true },
      skills: [],
    });
    expect((root.querySelector(".composer .running") as HTMLElement).hidden).toBe(false);
    const transcriptCalls = ipc.callsTo("session_transcript").length;
    const treeCalls = ipc.callsTo("session_tree").length;
    await settle(750);
    expect(ipc.callsTo("chat_status")).toHaveLength(1);
    await settle(750);
    expect(ipc.callsTo("chat_status")).toHaveLength(2);
    expect(root.querySelector(".notice")?.textContent).toContain("Finished: stop");
    expect(root.querySelector(".notice")?.textContent).toContain("context 10 of 100 tokens");
    expect(ipc.callsTo("session_transcript").length).toBeGreaterThan(transcriptCalls);
    expect(ipc.callsTo("session_tree").length).toBeGreaterThan(treeCalls);
    await settle(3000);
    expect(ipc.callsTo("chat_status")).toHaveLength(2);
  });

  it("shows the endpoint as fixed text and sends back exactly what chat_defaults returned", async () => {
    const endpoint = "http://127.0.0.1:4321/v1";
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      chat_defaults: () => ({
        settings: { ...SETTINGS, endpoint },
        preferences: "defaults",
        enter_to_send: true,
      }),
      chat_send: () => running,
    });
    itemFor("s-chat").click();
    await settle();
    const settings = root.querySelector(".settings") as HTMLElement;
    expect(settings.textContent).toContain(endpoint);
    expect(settings.textContent).toContain("Set when OMGCode starts.");
    const editable = Array.from(settings.querySelectorAll<HTMLInputElement>("input, textarea"));
    expect(editable.some((field) => field.value === endpoint)).toBe(false);
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ settings: { endpoint } });
  });

  it("cancels a running turn", async () => {
    await startChat([{ ...running, state: "cancelling" }]);
    const composer = root.querySelector(".composer") as HTMLElement;
    buttonByText(composer, "Cancel").click();
    await settle();
    expect(ipc.callsTo("chat_cancel")[0]?.args).toEqual({ session_id: "s-chat" });
    await settle(750);
    expect(ipc.callsTo("chat_status").length).toBeGreaterThan(0);
  });

  it("shows the core error message and code when the turn fails", async () => {
    await startChat([
      {
        ...running,
        state: "failed",
        error: { code: "provider_unreachable", message: "The endpoint could not be reached." },
      },
    ]);
    await settle(750);
    const notice = root.querySelector(".notice") as HTMLElement;
    expect(notice.textContent).toContain("The endpoint could not be reached.");
    expect(notice.textContent).toContain("provider_unreachable");
    expect(notice.classList.contains("error")).toBe(true);
  });

  it("shows a plain notice for a cancelled turn, decided by the code", async () => {
    await startChat([
      {
        ...running,
        state: "failed",
        error: { code: "cancelled", message: "turn was cancelled" },
      },
    ]);
    await settle(750);
    const notice = root.querySelector(".notice") as HTMLElement;
    expect(notice.textContent).toBe("Turn cancelled.");
    expect(notice.classList.contains("error")).toBe(false);
    expect((root.querySelector(".composer .running") as HTMLElement).hidden).toBe(true);
    expect(buttonByText(root, "Cancel").hidden).toBe(true);
  });

  it("keeps the error styling when only the message says cancelled", async () => {
    await startChat([
      {
        ...running,
        state: "failed",
        error: { code: "provider", message: "The request was cancelled by the server." },
      },
    ]);
    await settle(750);
    const notice = root.querySelector(".notice") as HTMLElement;
    expect(notice.textContent).toContain("The request was cancelled by the server.");
    expect(notice.textContent).toContain("provider");
    expect(notice.textContent).not.toBe("Turn cancelled.");
    expect(notice.classList.contains("error")).toBe(true);
  });

  it("keeps Send disabled until a model and context size are set", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      chat_defaults: () => ({
        settings: {
          endpoint: "",
          model: "",
          context_tokens: 0,
          max_output_tokens: 0,
          instructions: "",
          compact_threshold: 0,
          auto_compact: false,
        },
        preferences: "defaults",
        enter_to_send: true,
      }),
    });
    itemFor("s-chat").click();
    await settle();
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    expect(buttonByText(root, "Send").disabled).toBe(true);
    const inputs = root.querySelectorAll<HTMLInputElement>(".settings input[type=text]");
    type(inputs[0] as HTMLInputElement, "some-model");
    const numbers = root.querySelectorAll<HTMLInputElement>(".settings input[type=number]");
    type(numbers[0] as HTMLInputElement, "4096");
    expect(buttonByText(root, "Send").disabled).toBe(false);
  });
});

describe("approvals nudge and new session", () => {
  it("nudges when the pending count rises and opens the approvals window", async () => {
    let pending = 0;
    await start({ approvals_summary: () => ({ pending }) });
    expect((root.querySelector(".statusbar") as HTMLElement).hidden).toBe(true);
    pending = 1;
    await settle(3000);
    const bar = root.querySelector(".statusbar") as HTMLElement;
    expect(bar.hidden).toBe(false);
    expect(bar.textContent).toContain("An approval is waiting");
    expect(root.querySelector(".badge")?.textContent).toBe("1");
    buttonByText(bar, "Open approvals").click();
    await settle();
    expect(ipc.callsTo("approvals_show_window")).toHaveLength(1);
  });

  describe("notice lifecycle", () => {
    function bar(): HTMLElement {
      return root.querySelector(".statusbar") as HTMLElement;
    }

    it("shows nothing while the count stays at zero", async () => {
      await start({ approvals_summary: () => ({ pending: 0 }) });
      await settle(3000);
      await settle(3000);
      expect(bar().hidden).toBe(true);
      expect(bar().textContent).toBe("");
    });

    it("shows the notice for an approval already waiting at the first poll", async () => {
      await start({ approvals_summary: () => ({ pending: 1 }) });
      expect(bar().hidden).toBe(false);
      expect(bar().textContent).toContain("An approval is waiting");
      expect(hasButton(bar(), "Open approvals")).toBe(true);
      expect(root.querySelector(".badge")?.textContent).toBe("1");
    });

    it("keeps the notice while the count stays above zero and removes it at zero", async () => {
      let pending = 0;
      await start({ approvals_summary: () => ({ pending }) });
      pending = 1;
      await settle(3000);
      expect(bar().hidden).toBe(false);
      expect(bar().textContent).toContain("An approval is waiting");
      await settle(3000);
      expect(bar().hidden).toBe(false);
      expect(bar().textContent).toContain("An approval is waiting");
      pending = 0;
      await settle(3000);
      expect(bar().hidden).toBe(true);
      expect(bar().textContent).toBe("");
      expect(hasButton(root, "Open approvals")).toBe(false);
      expect((root.querySelector(".badge") as HTMLElement).hidden).toBe(true);
      await settle(3000);
      expect(bar().hidden).toBe(true);
    });

    it("keeps the notice when the count rises from one to two", async () => {
      let pending = 1;
      await start({ approvals_summary: () => ({ pending }) });
      pending = 2;
      await settle(3000);
      expect(bar().hidden).toBe(false);
      expect(bar().textContent).toContain("An approval is waiting");
      expect(hasButton(bar(), "Open approvals")).toBe(true);
      expect(root.querySelector(".badge")?.textContent).toBe("2");
    });

    it("shows the notice again when a new approval arrives after zero", async () => {
      let pending = 1;
      await start({ approvals_summary: () => ({ pending }) });
      pending = 0;
      await settle(3000);
      expect(bar().hidden).toBe(true);
      pending = 1;
      await settle(3000);
      expect(bar().hidden).toBe(false);
    });
  });

  it("sends the operator to Settings to register workspaces when none exist", async () => {
    await start({
      workspace_list: () => ({ development_root_set: false, workspaces: [] }),
      settings_show_window: () => null,
    });
    buttonByText(root, "New session").click();
    await settle();
    const dialog = root.querySelector("dialog") as HTMLDialogElement;
    expect(dialog.textContent).toContain("register a workspace in Settings");
    expect(hasButton(dialog, "Create")).toBe(false);
    expect(hasButton(dialog, "Close")).toBe(true);

    buttonByText(dialog, "Open settings").click();
    await settle();
    expect(ipc.callsTo("settings_show_window")).toHaveLength(1);
    // The main window only shows the settings window; it changes no setting itself.
    expect(ipc.calls.map((call) => call.command).filter((c) => c.startsWith("settings_"))).toEqual([
      "settings_show_window",
    ]);
  });

  it("creates a session in the chosen workspace and opens it", async () => {
    const created = session({ session_id: "s-new", title: "Fresh" });
    await start({
      workspace_list: () => ({
        development_root_set: true,
        workspaces: [
          {
            workspace_id: "w1",
            folder_display: "C:\\work",
            model_tools: true,
            read: true,
            write: false,
            execute: false,
          },
        ],
      }),
      session_create: () => created,
      session_transcript: () => page(created, []),
    });
    buttonByText(root, "New session").click();
    await settle();
    const dialog = root.querySelector("dialog") as HTMLDialogElement;
    expect(dialog.textContent).toContain("Enabled: model tools, read");
    type(dialog.querySelector('input[type="text"]') as HTMLInputElement, "Fresh");
    buttonByText(dialog, "Create").click();
    await settle();
    expect(ipc.callsTo("session_create")[0]?.args).toEqual({ workspace_id: "w1", title: "Fresh" });
    expect(root.querySelector(".pane-header")?.textContent).toContain("Fresh");
  });
});

describe("settings window, saved defaults, and per-turn skills", () => {
  const s = session({ session_id: "s-skills", title: "Skills" });
  const running = { session_id: "s-skills", state: "running", outcome: null, error: null };

  async function open(handlers: Record<string, Handler> = {}): Promise<void> {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      chat_send: () => running,
      chat_status: () => running,
      ...handlers,
    });
    itemFor("s-skills").click();
    await settle();
  }

  function skillBoxes(): HTMLInputElement[] {
    return Array.from(root.querySelectorAll<HTMLInputElement>(".skills input[type=checkbox]"));
  }

  it("opens the settings window and nothing else", async () => {
    await start();
    buttonByText(root, "Settings").click();
    await settle();
    expect(ipc.callsTo("settings_show_window")).toEqual([
      { command: "settings_show_window", args: {} },
    ]);
    expect(ipc.callsTo("settings_overview")).toHaveLength(0);
  });

  it("says so when the settings window cannot be shown", async () => {
    await start({ settings_show_window: () => rejection("window_unavailable", "gone") });
    buttonByText(root, "Settings").click();
    await settle();
    const bar = root.querySelector(".statusbar") as HTMLElement;
    expect(bar.hidden).toBe(false);
    expect(bar.textContent).toContain("The settings window could not be opened.");
  });

  it("keeps a window error through the approvals poll until it is dismissed", async () => {
    await start({
      approvals_summary: () => ({ pending: 0 }),
      settings_show_window: () => rejection("window_unavailable", "gone"),
    });
    buttonByText(root, "Settings").click();
    await settle();
    await settle(3000);
    await settle(3000);
    const bar = root.querySelector(".statusbar") as HTMLElement;
    expect(bar.hidden).toBe(false);
    expect(bar.textContent).toContain("The settings window could not be opened.");
    buttonByText(bar, "Dismiss").click();
    expect(bar.hidden).toBe(true);
    expect(bar.textContent).toBe("");
  });

  it("hides the skill choice when the workspace has no enabled skill", async () => {
    await open();
    expect(ipc.callsTo("session_skills")[0]?.args).toEqual({ session_id: "s-skills" });
    expect((root.querySelector(".skills") as HTMLElement).hidden).toBe(true);
  });

  it("sends the selected skills with one turn and clears them afterwards", async () => {
    await open({
      session_skills: () => ({ workspace_registered: true, skills: ["review", HOSTILE.img] }),
    });
    expect((root.querySelector(".skills") as HTMLElement).hidden).toBe(false);
    // Skill names are text, never markup.
    expect(root.querySelector(".skills img")).toBeNull();
    expect(root.querySelector(".skills")?.textContent).toContain(HOSTILE.img);
    const [review] = skillBoxes();
    (review as HTMLInputElement).click();
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ skills: ["review"] });
    expect(skillBoxes().every((box) => !box.checked)).toBe(true);
  });

  it("allows at most four skills for one turn", async () => {
    await open({
      session_skills: () => ({
        workspace_registered: true,
        skills: ["a1", "a2", "a3", "a4", "a5"],
      }),
    });
    const boxes = skillBoxes();
    for (const box of boxes.slice(0, 4)) {
      box.click();
    }
    expect(boxes[4]?.disabled).toBe(true);
    expect(boxes.slice(0, 4).every((box) => !box.disabled)).toBe(true);
    boxes[0]?.click();
    expect(boxes[4]?.disabled).toBe(false);
  });

  it("offers no skills when the list cannot be read", async () => {
    await open({ session_skills: () => rejection("busy", "busy") });
    expect((root.querySelector(".skills") as HTMLElement).hidden).toBe(true);
    expect(buttonByText(root, "Send")).toBeDefined();
  });

  it("saves the model settings as defaults without the endpoint", async () => {
    const saved = { ...SETTINGS, model: "saved-model" };
    await open({
      chat_preferences_save: () => ({ settings: saved, preferences: "saved", enter_to_send: true }),
    });
    const model = root.querySelector<HTMLInputElement>(".settings input[type=text]");
    type(model as HTMLInputElement, "saved-model");
    buttonByText(root, "Save as defaults").click();
    await settle();
    const args = ipc.callsTo("chat_preferences_save")[0]?.args as {
      preferences: Record<string, unknown>;
    };
    expect(args.preferences).toEqual({
      model: "saved-model",
      context_tokens: SETTINGS.context_tokens,
      max_output_tokens: SETTINGS.max_output_tokens,
      instructions: SETTINGS.instructions,
      compact_threshold: SETTINGS.compact_threshold,
      auto_compact: SETTINGS.auto_compact,
      enter_to_send: true,
    });
    expect("endpoint" in args.preferences).toBe(false);
    expect(root.querySelector(".preferences-row")?.textContent).toContain("Saved as defaults.");
  });

  describe("Enter to send", () => {
    function messageBox(): HTMLTextAreaElement {
      return root.querySelector(".message-box") as HTMLTextAreaElement;
    }

    function enterToSendBox(): HTMLInputElement {
      const label = Array.from(root.querySelectorAll(".settings label")).find(
        (candidate) => candidate.textContent === "Enter to send",
      );
      return label?.querySelector("input") as HTMLInputElement;
    }

    /** Dispatches a keydown and reports whether the composer prevented the default. */
    function press(init: KeyboardEventInit): boolean {
      const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
      messageBox().dispatchEvent(event);
      return event.defaultPrevented;
    }

    it("is on by default: Enter sends and Shift+Enter keeps the newline", async () => {
      await open();
      expect(enterToSendBox().checked).toBe(true);
      expect(messageBox().placeholder).toBe(PLACEHOLDER_ENTER);
      type(messageBox(), "hello");

      expect(press({ key: "Enter", shiftKey: true })).toBe(false);
      expect(press({ key: "Enter", isComposing: true })).toBe(false);
      await settle();
      expect(ipc.callsTo("chat_send")).toHaveLength(0);

      expect(press({ key: "Enter" })).toBe(true);
      await settle();
      expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ message: "hello" });
    });

    it("does not send an empty message on Enter, and says why", async () => {
      await open();
      expect(press({ key: "Enter" })).toBe(true);
      await settle();
      expect(ipc.callsTo("chat_send")).toHaveLength(0);
      expect(root.querySelector(".composer-error")?.textContent).toBe("Write a message first.");
    });

    it("when saved off, keeps Enter as a newline and sends on Ctrl+Enter", async () => {
      await open({
        chat_defaults: () => ({
          settings: { ...SETTINGS },
          preferences: "saved",
          enter_to_send: false,
        }),
      });
      expect(enterToSendBox().checked).toBe(false);
      expect(messageBox().placeholder).toBe(PLACEHOLDER_CTRL_ENTER);
      type(messageBox(), "hello");

      expect(press({ key: "Enter" })).toBe(false);
      await settle();
      expect(ipc.callsTo("chat_send")).toHaveLength(0);

      expect(press({ key: "Enter", ctrlKey: true })).toBe(true);
      await settle();
      expect(ipc.callsTo("chat_send")).toHaveLength(1);
    });

    it("applies a change at once and saves it with the defaults", async () => {
      await open({
        chat_preferences_save: () => ({
          settings: { ...SETTINGS },
          preferences: "saved",
          enter_to_send: false,
        }),
      });
      enterToSendBox().click();
      expect(messageBox().placeholder).toBe(PLACEHOLDER_CTRL_ENTER);
      type(messageBox(), "hello");
      expect(press({ key: "Enter" })).toBe(false);

      buttonByText(root, "Save as defaults").click();
      await settle();
      const args = ipc.callsTo("chat_preferences_save")[0]?.args as {
        preferences: Record<string, unknown>;
      };
      expect(args.preferences.enter_to_send).toBe(false);
      expect(ipc.callsTo("chat_send")).toHaveLength(0);
    });

    it("stays on when the saved defaults cannot be loaded", async () => {
      await open({ chat_defaults: () => rejection("internal", "failed") });
      expect(enterToSendBox().checked).toBe(true);
    });

    it("keeps a change made before the saved defaults arrive", async () => {
      let finish: (value: unknown) => void = () => undefined;
      await open({
        chat_defaults: () =>
          new Promise((resolve) => {
            finish = resolve;
          }),
      });
      enterToSendBox().click();
      expect(enterToSendBox().checked).toBe(false);

      finish({ settings: { ...SETTINGS }, preferences: "saved", enter_to_send: true });
      await settle();
      expect(enterToSendBox().checked).toBe(false);
      expect(messageBox().placeholder).toBe(PLACEHOLDER_CTRL_ENTER);
      type(messageBox(), "hello");
      expect(press({ key: "Enter" })).toBe(false);
      await settle();
      expect(ipc.callsTo("chat_send")).toHaveLength(0);
    });
  });

  it("shows the core error when saving defaults fails", async () => {
    await open({
      chat_preferences_save: () => rejection("invalid_request", "The model name is too long."),
    });
    buttonByText(root, "Save as defaults").click();
    await settle();
    expect(root.querySelector(".preferences-row")?.textContent).toContain(
      "The model name is too long. (invalid_request)",
    );
  });

  it("says when saved defaults could not be read", async () => {
    await open({
      chat_defaults: () => ({
        settings: { ...SETTINGS },
        preferences: "unreadable",
        enter_to_send: true,
      }),
    });
    expect(root.querySelector(".preferences-row")?.textContent).toContain(
      "Saved defaults could not be read",
    );
  });

  it("keeps edits made while defaults are being saved", async () => {
    let finish: (value: unknown) => void = () => undefined;
    await open({
      chat_preferences_save: () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    });
    const model = root.querySelector<HTMLInputElement>(
      ".settings input[type=text]",
    ) as HTMLInputElement;
    type(model, "first");
    buttonByText(root, "Save as defaults").click();
    await settle();
    expect(ipc.callsTo("chat_preferences_save")).toHaveLength(1);
    type(model, "second");
    finish({
      settings: { ...SETTINGS, model: "first" },
      preferences: "saved",
      enter_to_send: true,
    });
    await settle();
    expect(model.value).toBe("second");
    expect(root.querySelector(".preferences-row")?.textContent).toContain("Saved as defaults.");
  });

  describe("across session switches", () => {
    const a = session({ session_id: "s-a", title: "A" });
    const b = session({ session_id: "s-b", title: "B" });
    const transcript: Handler = (args) => page(args.session_id === "s-b" ? b : a, []);

    function skillNames(): string {
      return root.querySelector(".skills")?.textContent ?? "";
    }

    it("leaves another session's choice alone when an earlier send completes", async () => {
      let finish: (value: unknown) => void = () => undefined;
      await start({
        session_tree: treeOf(a, b),
        session_transcript: transcript,
        session_skills: () => ({ workspace_registered: true, skills: ["review"] }),
        chat_send: () =>
          new Promise((resolve) => {
            finish = resolve;
          }),
        chat_status: (args) => ({ ...running, session_id: args.session_id }),
      });
      itemFor("s-a").click();
      await settle();
      skillBoxes()[0]?.click();
      type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
      buttonByText(root, "Send").click();
      await settle();

      itemFor("s-b").click();
      await settle();
      skillBoxes()[0]?.click();
      type(root.querySelector(".message-box") as HTMLTextAreaElement, "draft for B");
      finish({ ...running, session_id: "s-a" });
      await settle();

      expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({
        session_id: "s-a",
        skills: ["review"],
      });
      expect(skillBoxes()[0]?.checked).toBe(true);
      expect((root.querySelector(".message-box") as HTMLTextAreaElement).value).toBe("draft for B");
    });

    it("ignores a skill list from an earlier opening of the same session", async () => {
      let finishFirst: (value: unknown) => void = () => undefined;
      let askedForA = 0;
      await start({
        session_tree: treeOf(a, b),
        session_transcript: transcript,
        session_skills: (args) => {
          if (args.session_id === "s-b") {
            return { workspace_registered: true, skills: [] };
          }
          askedForA += 1;
          if (askedForA === 1) {
            return new Promise((resolve) => {
              finishFirst = resolve;
            });
          }
          return { workspace_registered: true, skills: ["fresh"] };
        },
      });
      itemFor("s-a").click();
      await settle();
      itemFor("s-b").click();
      await settle();
      itemFor("s-a").click();
      await settle();
      skillBoxes()[0]?.click();

      finishFirst({ workspace_registered: true, skills: ["stale"] });
      await settle();
      expect(skillNames()).toContain("fresh");
      expect(skillNames()).not.toContain("stale");
      expect(skillBoxes()[0]?.checked).toBe(true);
    });
  });
});
