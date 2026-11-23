import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { ChatDefaults, SearchResult, SessionSummary } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import {
  clampThreshold,
  DEFAULTS_UNREADABLE,
  PLACEHOLDER_CTRL_ENTER,
  PLACEHOLDER_ENTER,
  SAVED_DEFAULTS_UNREADABLE,
  SET_CONTEXT_IN_SETTINGS,
  tokenLabel,
} from "../src/main/composer";
import { createMockBackend } from "../src/mock/backend";
import { HOSTILE } from "../src/mock/fixtures";
import {
  buttonByLabel,
  buttonByText,
  CHAT_BUDGET_BOUNDS,
  choose,
  connectionStatus,
  type FakeIpc,
  type Handler,
  hasButton,
  installFakeIpc,
  mainHandlers,
  modelSelect,
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
    // Session titles and folders never reach a tooltip. The window's icon buttons carry fixed
    // tooltips of their own (ADR-0072), so the check covers the session tree.
    expect(root.querySelector(".sidebar-body")?.querySelectorAll("[title]")).toHaveLength(0);
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
    expect(document.querySelectorAll("[onerror], [href]")).toHaveLength(0);
    // The only inline style is the shell's two panel widths (ADR-0072); no content has one.
    expect(document.querySelectorAll("[style]:not(.shell)")).toHaveLength(0);
    const shell = document.querySelector(".shell") as HTMLElement;
    expect(Array.from(shell.style).sort()).toEqual(["--left-width", "--right-width"]);
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
    const transcript = root.querySelector(".transcript");
    expect(transcript?.textContent).toContain("first");
    expect(transcript?.textContent).not.toContain("second");
    buttonByText(root, "Load more").click();
    await settle();
    expect(ipc.callsTo("session_transcript")[1]?.args).toMatchObject({ after_seq: 1 });
    expect(transcript?.textContent).toContain("second");
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

  it("shows the endpoint as fixed text and sends back exactly what the session's connection returned", async () => {
    const endpoint = "http://127.0.0.1:4321/v1";
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      connection_status: () =>
        connectionStatus({
          connection: { endpoint, source: "active", profile: null, kind: null, selectable: true },
        }),
      chat_send: () => running,
    });
    itemFor("s-chat").click();
    await settle();
    const settings = root.querySelector(".settings") as HTMLElement;
    expect(settings.textContent).toContain(endpoint);
    expect(settings.textContent).toContain(
      "Follows the session's provider, or is fixed when OMGCode starts.",
    );
    const editable = Array.from(settings.querySelectorAll<HTMLInputElement>("input, textarea"));
    expect(editable.some((field) => field.value === endpoint)).toBe(false);
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ settings: { endpoint } });
  });

  it("picks up a connection chosen in settings and keeps the draft and model", async () => {
    const before = "http://127.0.0.1:1234/v1";
    const after = "http://127.0.0.1:11434/v1";
    let current = before;
    // The core keeps the session's model, as a recorded choice does.
    let model: string = SETTINGS.model;
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      session_model_select: (args) => {
        model = args.model as string;
        return connectionStatus({ model });
      },
      connection_status: () =>
        connectionStatus({
          model,
          connection: {
            endpoint: current,
            source: "active",
            profile: null,
            kind: null,
            selectable: true,
          },
        }),
      chat_models: (args) => ({
        session_id: args.session_id as string,
        endpoint: current,
        kind: null,
        models: [
          { id: "lfm2.5:latest", cloud_tag: false },
          { id: SETTINGS.model, cloud_tag: false },
        ],
      }),
      chat_send: (args) =>
        (args.settings as { endpoint: string }).endpoint === current
          ? running
          : rejection("connection_changed", "The model connection changed in settings."),
    });
    itemFor("s-chat").click();
    await settle();
    choose(modelSelect(root), "lfm2.5:latest");
    // The settings window switches to Ollama after this window read its endpoint.
    current = after;
    const box = root.querySelector(".message-box") as HTMLTextAreaElement;
    type(box, "hello");
    buttonByText(root, "Send").click();
    await settle();
    const notice = root.querySelector(".notice") as HTMLElement;
    expect(notice.textContent).toContain("connection_changed");
    expect(notice.classList.contains("error")).toBe(true);
    const settings = root.querySelector(".settings") as HTMLElement;
    expect(settings.textContent).toContain(after);
    expect(settings.textContent).not.toContain(before);
    expect(box.value).toBe("hello");

    // The next send carries the new endpoint and the operator's own inputs.
    buttonByText(root, "Send").click();
    await settle();
    const sent = ipc.callsTo("chat_send");
    expect(sent).toHaveLength(2);
    expect(sent[1]?.args).toMatchObject({ settings: { endpoint: after } });
    expect(sent[1]?.args).toMatchObject({ settings: { model: "lfm2.5:latest" } });
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

  it("asks for a context size in Settings until one is saved, and reads it again on focus", async () => {
    const unset = {
      ...CHAT_BUDGET_BOUNDS,
      settings: { ...SETTINGS, model: "", context_tokens: 0 },
      preferences: "defaults" as const,
      enter_to_send: true,
    };
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      chat_defaults: () => unset,
    });
    itemFor("s-chat").click();
    await settle();
    const message = root.querySelector(".message-box") as HTMLTextAreaElement;
    type(message, "hello");
    expect(buttonByText(root, "Send").disabled).toBe(true);
    choose(modelSelect(root), SETTINGS.model);
    expect(buttonByText(root, "Send").disabled).toBe(true);
    message.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(root.querySelector(".composer-error")?.textContent).toBe(SET_CONTEXT_IN_SETTINGS);
    expect(ipc.callsTo("chat_send")).toHaveLength(0);

    // The settings window saves a context size; this window reads it when it gains focus.
    ipc.on("chat_defaults", () => ({ ...unset, settings: { ...SETTINGS, model: "" } }));
    window.dispatchEvent(new Event("focus"));
    await settle();
    expect(buttonByText(root, "Send").disabled).toBe(false);
  });
});

describe("approvals segment and new session", () => {
  function segment(): HTMLElement {
    return root.querySelector(".status-approvals") as HTMLElement;
  }

  it("shows the waiting count in the status bar and opens the approvals window", async () => {
    let pending = 0;
    await start({ approvals_summary: () => ({ pending }) });
    expect(segment().hidden).toBe(true);
    pending = 1;
    await settle(3000);
    expect(segment().hidden).toBe(false);
    expect(segment().textContent).toContain("1 approval waiting");
    expect(root.querySelector(".badge")?.textContent).toBe("1");
    // The old banner above the status bar is gone; only errors use it.
    expect((root.querySelector(".statusbar") as HTMLElement).hidden).toBe(true);
    buttonByText(segment(), "Review").click();
    await settle();
    expect(ipc.callsTo("approvals_show_window")).toHaveLength(1);
  });

  describe("approvals segment lifecycle", () => {
    it("shows nothing while the count stays at zero", async () => {
      await start({ approvals_summary: () => ({ pending: 0 }) });
      await settle(3000);
      await settle(3000);
      expect(segment().hidden).toBe(true);
      expect((root.querySelector(".statusbar") as HTMLElement).textContent).toBe("");
    });

    it("shows an approval already waiting at the first poll", async () => {
      await start({ approvals_summary: () => ({ pending: 1 }) });
      expect(segment().hidden).toBe(false);
      expect(segment().textContent).toContain("1 approval waiting");
      expect(hasButton(segment(), "Review")).toBe(true);
      expect(root.querySelector(".badge")?.textContent).toBe("1");
    });

    it("keeps the segment while the count stays above zero and hides it at zero", async () => {
      let pending = 0;
      await start({ approvals_summary: () => ({ pending }) });
      pending = 1;
      await settle(3000);
      expect(segment().hidden).toBe(false);
      await settle(3000);
      expect(segment().hidden).toBe(false);
      pending = 0;
      await settle(3000);
      expect(segment().hidden).toBe(true);
      expect((root.querySelector(".badge") as HTMLElement).hidden).toBe(true);
      await settle(3000);
      expect(segment().hidden).toBe(true);
    });

    it("updates the count when it rises from one to two", async () => {
      let pending = 1;
      await start({ approvals_summary: () => ({ pending }) });
      pending = 2;
      await settle(3000);
      expect(segment().hidden).toBe(false);
      expect(segment().textContent).toContain("2 approvals waiting");
      expect(root.querySelector(".badge")?.textContent).toBe("2");
    });

    it("shows the segment again when a new approval arrives after zero", async () => {
      let pending = 1;
      await start({ approvals_summary: () => ({ pending }) });
      pending = 0;
      await settle(3000);
      expect(segment().hidden).toBe(true);
      pending = 1;
      await settle(3000);
      expect(segment().hidden).toBe(false);
    });

    it("opens the approvals window from the sidebar row too, and decides nothing", async () => {
      await start({ approvals_summary: () => ({ pending: 1 }) });
      buttonByText(root.querySelector(".sidebar-footer") as HTMLElement, "Approvals").click();
      await settle();
      expect(ipc.callsTo("approvals_show_window")).toHaveLength(1);
      expect(
        ipc.calls.map((call) => call.command).filter((c) => c.startsWith("approval_")),
      ).toEqual([]);
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
            read: "allow",
            write: "prompt",
            execute: "deny",
          },
        ],
      }),
      session_create: () => created,
      session_transcript: () => page(created, []),
    });
    buttonByText(root, "New session").click();
    await settle();
    const dialog = root.querySelector("dialog") as HTMLDialogElement;
    expect(dialog.textContent).toContain("Model tools on. Read allow, write prompt, execute deny.");
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

  it("disables Workspace settings until a session is open", async () => {
    await start({ session_tree: treeOf(s) });
    expect(buttonByLabel(root, "Workspace settings").disabled).toBe(true);

    itemFor("s-skills").click();
    await settle();
    expect(buttonByLabel(root, "Workspace settings").disabled).toBe(false);
    expect(ipc.callsTo("settings_show_workspace")).toHaveLength(0);
  });

  it("opens the settings window for the open session's workspace by session id only", async () => {
    await open();
    buttonByLabel(root, "Workspace settings").click();
    await settle();
    expect(ipc.callsTo("settings_show_workspace")).toEqual([
      { command: "settings_show_workspace", args: { session_id: "s-skills" } },
    ]);
    expect(ipc.callsTo("settings_show_window")).toHaveLength(0);
  });

  it("says so when the workspace settings cannot be opened", async () => {
    await open({
      settings_show_workspace: () =>
        rejection("unknown_workspace", "The workspace is not registered."),
    });
    buttonByLabel(root, "Workspace settings").click();
    await settle();
    expect(root.querySelector(".composer-error")?.textContent).toContain(
      "The workspace is not registered.",
    );
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

  it("sends the budgets and instructions saved last, read again just before the send", async () => {
    await open();
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    // The settings window saves new values while this window keeps focus.
    ipc.on("chat_defaults", () => ({
      ...CHAT_BUDGET_BOUNDS,
      settings: {
        ...SETTINGS,
        context_tokens: 65_536,
        max_tool_calls: 192,
        turn_timeout_seconds: 900,
        instructions: "From the settings window.",
      },
      preferences: "saved",
      enter_to_send: true,
    }));
    const before = ipc.callsTo("chat_defaults").length;
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("chat_defaults").length).toBe(before + 1);
    const commands = ipc.calls.map((call) => call.command);
    expect(commands.lastIndexOf("chat_defaults")).toBeLessThan(commands.indexOf("chat_send"));
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({
      message: "hello",
      settings: {
        context_tokens: 65_536,
        max_tool_calls: 192,
        turn_timeout_seconds: 900,
        instructions: "From the settings window.",
        compact_threshold: SETTINGS.compact_threshold,
      },
    });
  });

  it("sends nothing twice when Send is clicked again while the defaults are read", async () => {
    await open();
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    let finish: (value: ChatDefaults) => void = () => undefined;
    ipc.on(
      "chat_defaults",
      () =>
        new Promise<ChatDefaults>((resolve) => {
          finish = resolve;
        }),
    );
    const send = buttonByText(root, "Send");
    send.click();
    expect(send.disabled).toBe(true);
    send.click();
    await settle();
    expect(ipc.callsTo("chat_send")).toHaveLength(0);
    finish({
      ...CHAT_BUDGET_BOUNDS,
      settings: SETTINGS,
      preferences: "saved",
      enter_to_send: true,
    });
    await settle();
    expect(ipc.callsTo("chat_send")).toHaveLength(1);
  });

  it("says when the defaults cannot be read, retries on Send, and never sends without them", async () => {
    await open({ chat_defaults: () => rejection("internal", "store offline") });
    const problem = (): string | null | undefined =>
      root.querySelector(".composer-error")?.textContent;
    expect(problem()).toBe(`${DEFAULTS_UNREADABLE} store offline (internal)`);
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    // Enabled, so a click reads the defaults again; a second failure still sends nothing.
    const send = buttonByText(root, "Send");
    expect(send.disabled).toBe(false);
    const before = ipc.callsTo("chat_defaults").length;
    send.click();
    await settle();
    expect(ipc.callsTo("chat_defaults").length).toBe(before + 1);
    expect(ipc.callsTo("chat_send")).toHaveLength(0);
    expect(problem()).toBe(`${DEFAULTS_UNREADABLE} store offline (internal)`);

    // Once the read succeeds, the same click sends.
    ipc.on("chat_defaults", () => ({
      ...CHAT_BUDGET_BOUNDS,
      settings: SETTINGS,
      preferences: "saved",
      enter_to_send: true,
    }));
    send.click();
    await settle();
    expect(ipc.callsTo("chat_send")).toHaveLength(1);
  });

  it("does not send with older defaults when the read before the send fails", async () => {
    await open();
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    ipc.on("chat_defaults", () => rejection("internal", "store offline"));
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("chat_send")).toHaveLength(0);
    expect(root.querySelector(".composer-error")?.textContent).toBe(
      `${DEFAULTS_UNREADABLE} store offline (internal)`,
    );
  });

  describe("after a send, keeps what was typed or chosen during it", () => {
    /** Clicks Send with the defaults read held open; the returned function lets it finish. */
    async function sendHeld(): Promise<() => Promise<void>> {
      let finish: (value: ChatDefaults) => void = () => undefined;
      ipc.on(
        "chat_defaults",
        () =>
          new Promise<ChatDefaults>((resolve) => {
            finish = resolve;
          }),
      );
      buttonByText(root, "Send").click();
      await settle();
      return async () => {
        finish({
          ...CHAT_BUDGET_BOUNDS,
          settings: SETTINGS,
          preferences: "saved",
          enter_to_send: true,
        });
        await settle();
      };
    }

    it("removes only the sent text and the sent skills", async () => {
      await open({
        session_skills: () => ({ workspace_registered: true, skills: ["review", "lint"] }),
      });
      const [review, lint] = skillBoxes();
      (review as HTMLInputElement).click();
      const box = root.querySelector(".message-box") as HTMLTextAreaElement;
      type(box, "hello");
      const release = await sendHeld();
      type(box, "hello, and then the tests");
      (lint as HTMLInputElement).click();
      await release();
      expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({
        message: "hello",
        skills: ["review"],
      });
      expect(box.value).toBe(", and then the tests");
      expect(review?.checked).toBe(false);
      expect(lint?.checked).toBe(true);
    });

    it("keeps a sent skill that was unticked and ticked again during the send", async () => {
      await open({
        session_skills: () => ({ workspace_registered: true, skills: ["review", "lint"] }),
      });
      const [review, lint] = skillBoxes();
      (review as HTMLInputElement).click();
      (lint as HTMLInputElement).click();
      type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
      const release = await sendHeld();
      (review as HTMLInputElement).click();
      (review as HTMLInputElement).click();
      await release();
      expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ skills: ["review", "lint"] });
      // Chosen again after the click, so kept; lint was untouched, so cleared.
      expect(review?.checked).toBe(true);
      expect(lint?.checked).toBe(false);
    });

    it("keeps the whole draft when it no longer starts with the sent text", async () => {
      await open();
      const box = root.querySelector(".message-box") as HTMLTextAreaElement;
      type(box, "hello");
      const release = await sendHeld();
      type(box, "a different question");
      await release();
      expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ message: "hello" });
      expect(box.value).toBe("a different question");
    });
  });

  it("saves only the composer's own defaults: the threshold, auto-compact, and the send key", async () => {
    await open({
      chat_preferences_save: () => ({
        ...CHAT_BUDGET_BOUNDS,
        settings: SETTINGS,
        preferences: "saved",
        enter_to_send: false,
      }),
    });
    const settings = root.querySelector(".settings") as HTMLElement;
    const slider = settings.querySelector<HTMLInputElement>('input[type="range"]');
    if (slider === null) throw new Error("no slider");
    type(slider, "256000");
    expect(settings.querySelector(".slider-value")?.textContent).toBe("256K tokens");
    const checks = Array.from(settings.querySelectorAll("label.field-check"));
    const box = (label: string) =>
      checks
        .find((candidate) => candidate.textContent === label)
        ?.querySelector("input") as HTMLInputElement;
    box("Auto-compact").click();
    box("Enter to send").click();
    buttonByText(root, "Save as defaults").click();
    await settle();
    const args = ipc.callsTo("chat_preferences_save")[0]?.args as {
      preferences: Record<string, unknown>;
    };
    expect(args.preferences).toEqual({
      compact_threshold: 256_000,
      auto_compact: !SETTINGS.auto_compact,
      enter_to_send: false,
    });
    expect(root.querySelector(".preferences-row")?.textContent).toContain("Saved as defaults.");
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    box("Enter to send").click();
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({
      settings: { compact_threshold: 256_000, auto_compact: !SETTINGS.auto_compact },
    });
  });

  it("keeps early composer edits and shows a saved threshold outside the range at its end", async () => {
    let finish: (value: ChatDefaults) => void = () => undefined;
    await open({
      chat_defaults: () =>
        new Promise<ChatDefaults>((resolve) => {
          finish = resolve;
        }),
    });
    const settings = root.querySelector(".settings") as HTMLElement;
    const slider = settings.querySelector<HTMLInputElement>('input[type="range"]');
    if (slider === null) throw new Error("no slider");
    const autoCompact = Array.from(settings.querySelectorAll("label.field-check"))
      .find((candidate) => candidate.textContent === "Auto-compact")
      ?.querySelector("input") as HTMLInputElement;
    type(slider, "512000");
    finish({
      ...CHAT_BUDGET_BOUNDS,
      settings: { ...SETTINGS, compact_threshold: 6000, auto_compact: false },
      preferences: "saved",
      enter_to_send: true,
    });
    await settle();
    // The edited slider keeps its value; the untouched check box shows the saved one.
    expect(slider.value).toBe("512000");
    expect(autoCompact.checked).toBe(false);

    // A fresh composer shows a saved value below the range at the range's start.
    expect(clampThreshold(6000, 128_000, 1_000_000)).toBe(128_000);
    expect(clampThreshold(2_000_000, 128_000, 1_000_000)).toBe(1_000_000);
    expect(clampThreshold(203_000, 128_000, 1_000_000)).toBe(200_000);
    expect(tokenLabel(1_000_000)).toBe("1M");
    expect(tokenLabel(128_000)).toBe("128K");
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
          ...CHAT_BUDGET_BOUNDS,
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

      finish({
        ...CHAT_BUDGET_BOUNDS,
        settings: { ...SETTINGS },
        preferences: "saved",
        enter_to_send: true,
      });
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
        ...CHAT_BUDGET_BOUNDS,
        settings: { ...SETTINGS },
        preferences: "unreadable",
        enter_to_send: true,
      }),
    });
    expect(root.querySelector(".preferences-row")?.textContent).toContain(
      "Saved defaults could not be read",
    );
  });

  it("names the unreadable saved defaults, not a missing context size, as the reason", async () => {
    // Rust gives the built-in values, which set no context size, for a file it cannot read.
    await open({
      chat_defaults: () => ({
        ...CHAT_BUDGET_BOUNDS,
        settings: { ...SETTINGS, context_tokens: 0 },
        preferences: "unreadable",
        enter_to_send: true,
      }),
    });
    const message = root.querySelector(".message-box") as HTMLTextAreaElement;
    type(message, "hello");
    expect(buttonByText(root, "Send").disabled).toBe(true);
    message.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(root.querySelector(".composer-error")?.textContent).toBe(SAVED_DEFAULTS_UNREADABLE);
    expect(ipc.callsTo("chat_send")).toHaveLength(0);
  });

  it("keeps edits made while defaults are being saved", async () => {
    let finish: (value: unknown) => void = () => undefined;
    await open({
      chat_preferences_save: () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    });
    ipc.on("chat_models", (args) => ({
      session_id: args.session_id as string,
      endpoint: SETTINGS.endpoint,
      kind: null,
      models: [
        { id: "first", cloud_tag: false },
        { id: "second", cloud_tag: false },
      ],
    }));
    buttonByText(root, "Refresh").click();
    await settle();
    const model = modelSelect(root);
    choose(model, "first");
    buttonByText(root, "Save as defaults").click();
    await settle();
    expect(ipc.callsTo("chat_preferences_save")).toHaveLength(1);
    choose(model, "second");
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

describe("status bar", () => {
  const s = session({ session_id: "s-foot", title: "Footer" });
  const LMS = "http://127.0.0.1:1234/v1";
  const OLLAMA = "http://127.0.0.1:11434/v1";

  function footer(): HTMLElement {
    return root.querySelector(".status-footer") as HTMLElement;
  }

  function provider(): HTMLSelectElement {
    return footer().querySelector('select[aria-label="Model provider"]') as HTMLSelectElement;
  }

  function optionTexts(): string[] {
    return Array.from(provider().options).map((option) => option.textContent ?? "");
  }

  function choose(value: string): void {
    provider().value = value;
    provider().dispatchEvent(new Event("change", { bubbles: true }));
  }

  /** A status with the local profiles `lms` and `ollama`, `active` being the active one. */
  function status(active: string | null, selectable = true): unknown {
    const endpoint = active === "ollama" ? OLLAMA : LMS;
    let kind: string | null = null;
    if (active !== null) {
      kind = active === "ollama" ? "ollama_local" : "lm_studio";
    }
    return {
      connection: {
        endpoint,
        source: selectable ? "active" : "profile",
        profile: active,
        kind,
        selectable,
      },
      origin: selectable ? "default" : "launch",
      model: SETTINGS.model,
      stale: false,
      session_choices_unreadable: false,
      choices: selectable
        ? [
            { name: "lms", kind: "lm_studio", endpoint: LMS, active: active === "lms" },
            { name: "ollama", kind: "ollama_local", endpoint: OLLAMA, active: active === "ollama" },
          ]
        : [],
    };
  }

  it("sits below the conversation and starts with no session open", async () => {
    await start();
    expect(footer()).not.toBeNull();
    expect(footer().previousElementSibling?.classList.contains("statusbar")).toBe(true);
    expect(footer().textContent).toContain("No session open");
    expect(ipc.callsTo("session_context")).toHaveLength(0);
  });

  it("lists the default and each local connection and marks the active one", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      connection_status: () => status("ollama"),
    });
    itemFor("s-foot").click();
    await settle();
    expect(optionTexts()).toEqual([
      `LM Studio: default (${LMS})`,
      `LM Studio: lms (${LMS})`,
      `Ollama: ollama (${OLLAMA})`,
    ]);
    expect(provider().value).toBe("ollama");
    expect(provider().disabled).toBe(false);
    // Like the rest of the main window, the footer uses no tooltips.
    expect(footer().querySelectorAll("[title]")).toHaveLength(0);
  });

  it("asks Rust to switch, then shows the new endpoint in the composer", async () => {
    let active: string | null = null;
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      connection_status: () => status(active),
      connection_select: (args) => {
        active = args.profile as string | null;
        return status(active);
      },
    });
    itemFor("s-foot").click();
    await settle();
    const settings = root.querySelector(".settings") as HTMLElement;
    expect(settings.textContent).toContain(LMS);

    choose("ollama");
    await settle();
    expect(ipc.callsTo("connection_select").map((call) => call.args)).toEqual([
      { session_id: "s-foot", profile: "ollama" },
    ]);
    expect(provider().value).toBe("ollama");
    expect(settings.textContent).toContain(OLLAMA);

    choose("");
    await settle();
    expect(ipc.callsTo("connection_select")[1]?.args).toEqual({
      session_id: "s-foot",
      profile: null,
    });
    expect(provider().value).toBe("");
  });

  it("changes nothing when the operator answers No", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      connection_status: () => status("lms"),
      connection_select: () => rejection("cancelled", "The change was not confirmed."),
    });
    itemFor("s-foot").click();
    await settle();
    const reads = ipc.callsTo("connection_status").length;
    choose("ollama");
    await settle();
    expect(footer().textContent).toContain("The model provider was not changed.");
    // The recorded choice is read again, so the dropdown shows what is really active.
    expect(ipc.callsTo("connection_status").length).toBeGreaterThan(reads);
    expect(provider().value).toBe("lms");
    expect(provider().disabled).toBe(false);
  });

  it("shows the core error and code when a switch fails", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      connection_status: () => status(null),
      connection_select: () => rejection("busy", "Another settings change is in progress."),
    });
    itemFor("s-foot").click();
    await settle();
    choose("lms");
    await settle();
    expect(footer().textContent).toContain("Another settings change is in progress. (busy)");
    expect(provider().value).toBe("");
  });

  it("disables the choice when a launch argument fixed the connection", async () => {
    await start({ connection_status: () => status("lms", false) });
    expect(provider().disabled).toBe(true);
    expect(optionTexts()).toEqual([`Set at launch: LM Studio (${LMS})`]);
  });

  it("says when the connection cannot be read", async () => {
    await start({
      connection_status: () => rejection("control", "The settings store is unhealthy."),
    });
    expect(footer().textContent).toContain("The settings store is unhealthy. (control)");
    expect(provider().disabled).toBe(true);
  });

  it("shows the open session's workspace and branch", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      session_context: () => ({
        workspace_registered: true,
        branch: { state: "branch", name: "codex/ollama-provider", note: null },
      }),
    });
    itemFor("s-foot").click();
    await settle();
    expect(ipc.callsTo("session_context")[0]?.args).toEqual({ session_id: "s-foot" });
    expect(footer().textContent).toContain("Workspace: C:\\work\\one");
    expect(footer().textContent).toContain("Branch: codex/ollama-provider");
  });

  it.each([
    [{ state: "detached", name: "5ba2e8a", note: null }, "Detached at 5ba2e8a"],
    [{ state: "none", name: null, note: null }, "Not a Git repository"],
    [{ state: "unavailable", name: null, note: null }, "Branch unavailable"],
    [
      { state: "unavailable", name: null, note: "HEAD has an unknown form" },
      "Branch unavailable: HEAD has an unknown form",
    ],
  ])("shows the head %o as %s", async (branch, text) => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      session_context: () => ({ workspace_registered: true, branch }),
    });
    itemFor("s-foot").click();
    await settle();
    const segment = Array.from(footer().querySelectorAll<HTMLElement>(".status-segment")).find(
      (node) => node.textContent === text,
    );
    expect(segment).toBeDefined();
  });

  it("notes an unregistered workspace and survives a failed branch read", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      session_context: () => ({
        workspace_registered: false,
        branch: { state: "none", name: null, note: null },
      }),
    });
    itemFor("s-foot").click();
    await settle();
    expect(footer().textContent).toContain("(workspace not registered here)");

    ipc.on("session_context", () => rejection("busy", "busy"));
    window.dispatchEvent(new Event("focus"));
    await settle();
    expect(footer().textContent).toContain("Branch unavailable");
  });

  it("renders a hostile branch name as inert text", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      session_context: () => ({
        workspace_registered: true,
        branch: { state: "branch", name: HOSTILE, note: null },
      }),
    });
    itemFor("s-foot").click();
    await settle();
    expect(footer().querySelector("img")).toBeNull();
    expect(footer().textContent).toContain(neutralise(`Branch: ${HOSTILE}`));
  });

  it("reads the connection and branch again on focus, and stops once disposed", async () => {
    await start({ session_tree: treeOf(s), session_transcript: () => page(s, []) });
    itemFor("s-foot").click();
    await settle();
    const status0 = ipc.callsTo("connection_status").length;
    const context0 = ipc.callsTo("session_context").length;
    window.dispatchEvent(new Event("focus"));
    await settle();
    expect(ipc.callsTo("connection_status").length).toBe(status0 + 1);
    expect(ipc.callsTo("session_context").length).toBe(context0 + 1);

    app?.dispose();
    app = null;
    window.dispatchEvent(new Event("focus"));
    await settle();
    expect(ipc.callsTo("connection_status").length).toBe(status0 + 1);
  });

  it("reads the connection again when a send finds it changed elsewhere", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      chat_send: () => rejection("connection_changed", "The model connection changed."),
    });
    itemFor("s-foot").click();
    await settle();
    const before = ipc.callsTo("connection_status").length;
    type(root.querySelector(".message-box") as HTMLTextAreaElement, "hello");
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("connection_status").length).toBeGreaterThan(before);
  });

  it("drops a branch read that a newer session open overtook", async () => {
    const other = session({ session_id: "s-other", title: "Other" });
    let finishFirst: (value: unknown) => void = () => undefined;
    await start({
      session_tree: () =>
        tree([
          { id: "g1", folder: "C:\\work\\one", sessions: [s] },
          { id: "g2", folder: "C:\\work\\two", sessions: [other] },
        ]),
      session_transcript: (args) => page(args.session_id === "s-foot" ? s : other, []),
      session_context: (args) => {
        if (args.session_id !== "s-foot") {
          return {
            workspace_registered: true,
            branch: { state: "branch", name: "two", note: null },
          };
        }
        return new Promise((resolve) => {
          finishFirst = resolve;
        });
      },
    });
    itemFor("s-foot").click();
    await settle();
    itemFor("s-other").click();
    await settle();
    finishFirst({
      workspace_registered: true,
      branch: { state: "branch", name: "one", note: null },
    });
    await settle();
    expect(footer().textContent).toContain("Workspace: C:\\work\\two");
    expect(footer().textContent).toContain("Branch: two");
    expect(footer().textContent).not.toContain("Branch: one");
  });
});
