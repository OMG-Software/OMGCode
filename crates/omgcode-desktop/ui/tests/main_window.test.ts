import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { SearchResult, SessionSummary } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
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
  const folder = `${"f".repeat(5000)}‮`;
  const title = `${"t".repeat(300)}‮`;
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
      chat_defaults: () => ({ ...SETTINGS, endpoint }),
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

  it("keeps Send disabled until a model and context size are set", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      chat_defaults: () => ({
        endpoint: "",
        model: "",
        context_tokens: 0,
        max_output_tokens: 0,
        instructions: "",
        compact_threshold: 0,
        auto_compact: false,
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

  it("explains how to register workspaces when none exist", async () => {
    await start({ workspace_list: () => ({ development_root_set: false, workspaces: [] }) });
    buttonByText(root, "New session").click();
    await settle();
    const dialog = root.querySelector("dialog") as HTMLDialogElement;
    expect(dialog.textContent).toContain("omgcode workspace set-root");
    expect(dialog.textContent).toContain("omgcode workspace register");
    expect(hasButton(dialog, "Create")).toBe(false);
    expect(hasButton(dialog, "Close")).toBe(true);
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
