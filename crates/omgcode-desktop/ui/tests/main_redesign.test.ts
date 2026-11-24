import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { SessionSummary, TranscriptEntry } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import { COUNTER_SHOW_RATIO, SKILLS_PER_TURN } from "../src/main/composer";
import { MESSAGE_MAX_BYTES } from "../src/main/model";
import { argumentPreview, renderEntry, TOOL_PREVIEW_CHARS } from "../src/main/transcript";
import {
  buttonByLabel,
  buttonByText,
  type FakeIpc,
  type Handler,
  HOSTILE_IMG,
  installFakeIpc,
  mainHandlers,
  mountRoot,
  page,
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

function type(input: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function menu(label: string): HTMLDetailsElement {
  const found = Array.from(root.querySelectorAll<HTMLDetailsElement>("details.popover")).find(
    (details) => {
      const summary = details.querySelector("summary");
      return (
        summary?.textContent?.startsWith(label) === true ||
        summary?.getAttribute("aria-label") === label
      );
    },
  );
  if (found === undefined) {
    throw new Error(`No menu starting with "${label}"`);
  }
  return found;
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

describe("sidebar View menu", () => {
  it("keeps the header to a title, New session, collapse, search, and one View menu", async () => {
    await start();
    const header = root.querySelector(".sidebar-header") as HTMLElement;
    // Outside the View menu there are two buttons: New session and the collapse icon (ADR-0072).
    const direct = Array.from(header.querySelectorAll("button")).filter(
      (b) => b.closest("details") === null,
    );
    expect(direct.map((b) => b.getAttribute("aria-label") ?? b.textContent)).toEqual([
      "New session",
      "Collapse sessions panel",
    ]);
    expect(buttonByText(header, "New session").classList.contains("primary")).toBe(true);
    expect(header.querySelectorAll("details.popover")).toHaveLength(1);
    expect(header.querySelector("details")?.contains(header.querySelector(".search"))).toBe(false);
    const footer = root.querySelector(".sidebar-footer") as HTMLElement;
    expect(buttonByText(footer, "Approvals")).toBeDefined();
    expect(buttonByText(footer, "Settings")).toBeDefined();
  });

  it("holds Active and Archived, sort, and direction, and each still works", async () => {
    await start();
    const view = menu("View");
    expect(view.open).toBe(false);
    const group = view.querySelector('[role="radiogroup"]') as HTMLElement;
    expect(group.classList.contains("segmented")).toBe(true);
    expect(group.querySelector('[aria-checked="true"]')?.textContent).toBe("Active");

    buttonByText(view, "Archived").click();
    await settle();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toMatchObject({ view: "archived" });
    expect(view.querySelector('[aria-checked="true"]')?.textContent).toBe("Archived");

    const select = view.querySelector("select") as HTMLSelectElement;
    select.value = "created";
    select.dispatchEvent(new Event("change"));
    await settle();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toMatchObject({ sort: "created" });

    buttonByText(view, "Descending").click();
    await settle();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toEqual({
      view: "archived",
      sort: "created",
      descending: false,
    });
    expect(buttonByText(view, "Ascending")).toBeDefined();

    buttonByText(view, "Active").click();
    await settle();
    expect(ipc.callsTo("session_tree").at(-1)?.args).toMatchObject({ view: "active" });
  });

  it("closes on Escape and when the pointer goes down elsewhere", async () => {
    await start();
    const view = menu("View");
    view.open = true;
    view.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(view.open).toBe(false);
    view.open = true;
    (root.querySelector(".sidebar-body") as HTMLElement).dispatchEvent(
      new Event("pointerdown", { bubbles: true }),
    );
    expect(view.open).toBe(false);
  });
});

describe("status bar approvals segment", () => {
  it("is hidden at zero and sits between the branch and the model", async () => {
    await start({ approvals_summary: () => ({ pending: 0 }) });
    const footer = root.querySelector(".status-footer") as HTMLElement;
    const segment = footer.querySelector(".status-approvals") as HTMLElement;
    expect(segment.hidden).toBe(true);
    const order = Array.from(footer.children);
    expect(order.indexOf(segment)).toBeLessThan(
      order.indexOf(footer.querySelector(".status-spacer") as Element),
    );
  });

  it("announces a new count in a live region that is always present and holds no button", async () => {
    let pending = 0;
    await start({ approvals_summary: () => ({ pending }) });
    const live = root.querySelector(".status-footer .visually-hidden") as HTMLElement;
    expect(live.getAttribute("role")).toBe("status");
    expect(live.getAttribute("aria-live")).toBe("polite");
    expect(live.hidden).toBe(false);
    expect(live.closest("[hidden]")).toBeNull();
    expect(live.textContent).toBe("");
    pending = 1;
    await settle(10_000);
    expect(live.textContent).toBe("1 approval waiting");
    expect(live.querySelector("button")).toBeNull();
    pending = 0;
    await settle(10_000);
    expect(live.textContent).toBe("");
  });

  it("names the count, offers Review, and opens only the approvals window", async () => {
    await start({ approvals_summary: () => ({ pending: 3 }) });
    const segment = root.querySelector(".status-approvals") as HTMLElement;
    expect(segment.hidden).toBe(false);
    expect(segment.textContent).toContain("3 approvals waiting");
    buttonByText(segment, "Review").click();
    await settle();
    expect(ipc.callsTo("approvals_show_window")).toHaveLength(1);
    const names = ipc.calls.map((call) => call.command);
    expect(names.filter((name) => name.startsWith("approval_"))).toEqual([]);
  });
});

describe("composer toolbar", () => {
  const s = session({ session_id: "s-bar", title: "Bar" });

  async function open(handlers: Record<string, Handler> = {}): Promise<void> {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, []),
      ...handlers,
    });
    (root.querySelector('[data-session-id="s-bar"]') as HTMLElement).click();
    await settle();
  }

  function counter(): HTMLElement {
    return root.querySelector(".counter") as HTMLElement;
  }

  function box(): HTMLTextAreaElement {
    return root.querySelector(".message-box") as HTMLTextAreaElement;
  }

  it("shows the byte counter only above 80 percent of the limit, and when over it", async () => {
    await open();
    const threshold = Math.floor(MESSAGE_MAX_BYTES * COUNTER_SHOW_RATIO);
    expect(counter().hidden).toBe(true);
    type(box(), "a".repeat(threshold));
    expect(counter().hidden).toBe(true);
    type(box(), "a".repeat(threshold + 1));
    expect(counter().hidden).toBe(false);
    expect(counter().textContent).toBe(`${threshold + 1} / ${MESSAGE_MAX_BYTES} bytes`);
    expect(counter().classList.contains("over")).toBe(false);
    type(box(), "a".repeat(MESSAGE_MAX_BYTES + 1));
    expect(counter().hidden).toBe(false);
    expect(counter().classList.contains("over")).toBe(true);
    expect((buttonByText(root, "Send") as HTMLButtonElement).disabled).toBe(true);
    type(box(), "short");
    expect(counter().hidden).toBe(true);
  });

  it("puts Skills and Chat settings in upward menus on the left, Send on the right", async () => {
    await open({
      session_skills: () => ({ workspace_registered: true, skills: ["review"] }),
    });
    const row = root.querySelector(".composer-row") as HTMLElement;
    const tools = row.querySelector(".composer-tools") as HTMLElement;
    const send = row.querySelector(".composer-send") as HTMLElement;
    expect(menu("Skills").classList.contains("open-up")).toBe(true);
    expect(menu("Chat settings").classList.contains("open-up")).toBe(true);
    expect(tools.contains(menu("Skills"))).toBe(true);
    expect(tools.contains(menu("Chat settings"))).toBe(true);
    expect(buttonByText(send, "Send").classList.contains("primary")).toBe(true);
    expect(row.querySelectorAll("button.primary")).toHaveLength(1);
    expect(root.querySelector(".composer-panel .settings-grid")).not.toBeNull();
  });

  it("opens Chat settings from a gear icon button with a tooltip and expanded state", async () => {
    await open();
    const settings = menu("Chat settings");
    const trigger = settings.querySelector("summary") as HTMLElement;
    expect(trigger.textContent).toBe("");
    expect(trigger.title).toBe("Chat settings");
    expect(trigger.getAttribute("aria-label")).toBe("Chat settings");
    expect(trigger.querySelector("svg.icon")).not.toBeNull();
    const panel = settings.querySelector(".popover-panel") as HTMLElement;
    expect(trigger.getAttribute("aria-controls")).toBe(panel.id);
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    settings.open = true;
    await settle();
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    settings.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(trigger);
  });

  it("counts the chosen skills in the summary and keeps the at-most-four rule", async () => {
    await open({
      session_skills: () => ({
        workspace_registered: true,
        skills: ["a1", "a2", "a3", "a4", "a5"],
      }),
    });
    const skills = menu("Skills");
    expect(skills.hidden).toBe(false);
    expect(skills.querySelector("summary")?.textContent).toBe("Skills (0)");
    expect(skills.textContent).toContain(`Skills for this turn (at most ${SKILLS_PER_TURN})`);
    const boxes = Array.from(skills.querySelectorAll<HTMLInputElement>("input[type=checkbox]"));
    for (const choice of boxes.slice(0, SKILLS_PER_TURN)) {
      choice.click();
    }
    expect(skills.querySelector("summary")?.textContent).toBe("Skills (4)");
    expect(boxes[4]?.disabled).toBe(true);
    boxes[0]?.click();
    expect(skills.querySelector("summary")?.textContent).toBe("Skills (3)");
    expect(boxes[4]?.disabled).toBe(false);
  });

  it("hides the Skills menu when no skill is enabled", async () => {
    await open();
    expect(menu("Skills").hidden).toBe(true);
    expect(menu("Chat settings").hidden).toBe(false);
  });

  it("keeps the settings grid, the read-only endpoint, and Save defaults in the menu", async () => {
    await open();
    const settings = menu("Chat settings");
    expect(settings.querySelector(".fixed-value")?.textContent).toBe(SETTINGS.endpoint);
    expect(settings.textContent).toContain("Follows the session's provider");
    expect(buttonByText(settings, "Save as defaults")).toBeDefined();
    // The model and the provider moved to the side panel (ADR-0089); nothing is typed (ADR-0066).
    expect(settings.querySelectorAll("input[type=text]")).toHaveLength(0);
    expect(settings.querySelectorAll("select")).toHaveLength(0);
    // They sit in the side panel's model box, not the composer (ADR-0089).
    const composer = root.querySelector(".composer") as HTMLElement;
    expect(composer.querySelectorAll("select")).toHaveLength(0);
    const modelBox = root.querySelector(".model-box") as HTMLElement;
    expect(modelBox.querySelectorAll('select[aria-label="Model"]')).toHaveLength(1);
    expect(modelBox.querySelectorAll('select[aria-label="Provider"]')).toHaveLength(1);
    // Budgets and instructions moved to the settings window (ADR-0080); a slider sets the
    // compaction threshold.
    expect(settings.querySelectorAll("input[type=number]")).toHaveLength(0);
    expect(settings.querySelectorAll("textarea")).toHaveLength(0);
    const slider = settings.querySelector<HTMLInputElement>('input[type="range"]');
    expect(slider?.getAttribute("aria-label")).toBe("Compaction threshold");
    expect([slider?.min, slider?.max, slider?.step]).toEqual(["128000", "1000000", "8000"]);
    expect(settings.querySelector(".slider-value")?.textContent).toBe("200K tokens");
    expect(settings.textContent).toContain("Auto-compact");
    expect(settings.textContent).toContain("Enter to send");
  });

  it("puts Workspace settings beside Send as an icon button with a tooltip", async () => {
    await open();
    const send = root.querySelector(".composer-send") as HTMLElement;
    const workspace = buttonByLabel(send, "Workspace settings");
    expect(workspace.title).toBe("Workspace settings");
    expect(workspace.textContent).toBe("");
    expect(workspace.querySelector("svg.icon")).not.toBeNull();
    expect(root.querySelector(".composer-tools")?.contains(workspace)).toBe(false);
    // It sits just before the Send button, on the right.
    const order = Array.from(send.querySelectorAll("button")).filter((b) => !b.hidden);
    expect(order.map((b) => b.getAttribute("aria-label") ?? b.textContent)).toEqual([
      "Workspace settings",
      "Send",
    ]);
  });

  it("keeps Send and the draft working with both menus closed", async () => {
    await open({
      chat_send: () => ({ session_id: "s-bar", state: "running", outcome: null, error: null }),
      chat_status: () => ({ session_id: "s-bar", state: "running", outcome: null, error: null }),
    });
    expect(menu("Chat settings").open).toBe(false);
    type(box(), "hello");
    buttonByText(root, "Send").click();
    await settle();
    expect(ipc.callsTo("chat_send")[0]?.args).toMatchObject({ message: "hello" });
    expect(
      buttonByText(root.querySelector(".composer-send") as HTMLElement, "Cancel"),
    ).toBeDefined();
  });
});

describe("compact transcript rows", () => {
  const base = { seq: 3, timestamp: "2026-01-02T10:00:00Z" };
  const RTL = `${String.fromCharCode(0x202e)}exe.txt`;

  function request(args: string, tool = "workspace_read_file"): TranscriptEntry {
    return {
      ...base,
      kind: "tool_requested",
      request_id: "r",
      call_id: "c",
      tool,
      arguments: args,
    };
  }

  it("shows a tool request as a collapsed row with the name and a one-line preview", () => {
    const node = renderEntry(request('{\n  "path": "src/lib.rs",\n  "note": "x"\n}'));
    expect(node.tagName).toBe("DETAILS");
    expect((node as HTMLDetailsElement).open).toBe(false);
    const summary = node.querySelector("summary") as HTMLElement;
    expect(summary.textContent).toContain("workspace_read_file");
    const preview = summary.querySelector(".tool-preview") as HTMLElement;
    expect(preview.classList.contains("clamp-1")).toBe(true);
    expect(preview.textContent).toBe('{ "path": "src/lib.rs", "note": "x" }');
    expect(node.querySelector("pre.tool-args")?.textContent).toBe(
      '{\n  "path": "src/lib.rs",\n  "note": "x"\n}',
    );
  });

  it("cuts a long preview but keeps every argument in the body", () => {
    const long = "A".repeat(TOOL_PREVIEW_CHARS * 3);
    const node = renderEntry(request(long));
    const preview = node.querySelector(".tool-preview")?.textContent ?? "";
    expect(preview.length).toBe(TOOL_PREVIEW_CHARS + 1);
    expect(preview.endsWith("…")).toBe(true);
    expect(node.querySelector("pre")?.textContent).toBe(long);
  });

  it("does not split a surrogate pair at the cut", () => {
    const text = `${"a".repeat(TOOL_PREVIEW_CHARS - 1)}\u{1f600}tail`;
    const cut = argumentPreview(text);
    expect(cut).toBe(`${"a".repeat(TOOL_PREVIEW_CHARS - 1)}…`);
  });

  it("renders hostile arguments, tool names, and bidirectional marks as text", () => {
    const node = renderEntry(request(`${HOSTILE_IMG} ${RTL}`, HOSTILE_IMG));
    expect(node.querySelector("img")).toBeNull();
    expect(node.querySelector(".tool-name")?.textContent).toBe(HOSTILE_IMG);
    const preview = node.querySelector(".tool-preview")?.textContent ?? "";
    expect(preview).toBe(neutralise(`${HOSTILE_IMG} ${RTL}`));
    expect(preview).toContain("[U+202E]");
    expect(preview).not.toContain(String.fromCharCode(0x202e));
    expect(node.querySelectorAll("[style], [onerror], [href]")).toHaveLength(0);
  });

  it("keeps a tool result as a compact muted row with its text", () => {
    const node = renderEntry({
      ...base,
      kind: "tool_completed",
      request_id: "r",
      call_id: "c",
      outcome: "succeeded",
      code: null,
      output_bytes: 9,
    });
    expect(node.classList.contains("tool-result")).toBe(true);
    expect(node.classList.contains("note-line")).toBe(false);
    expect(node.textContent).toBe("Tool result: succeeded, 9 bytes of output");
  });

  it("draws turn notes and request metadata as divider lines", () => {
    const notes: TranscriptEntry[] = [
      { ...base, kind: "turn_started", request_id: "r" },
      { ...base, kind: "turn_completed", request_id: "r" },
      { ...base, kind: "turn_failed", request_id: "r", code: null },
      {
        ...base,
        kind: "request_manifest",
        request_id: "r",
        model: "m",
        prompt_version: "v",
        tools: [],
      },
      { ...base, kind: "session_archived" },
      { ...base, kind: "session_restored" },
    ];
    for (const entry of notes) {
      const node = renderEntry(entry);
      expect(node.classList.contains("note-line")).toBe(true);
      expect(node.querySelectorAll(".note-text")).toHaveLength(1);
      expect(node.querySelector(".entry-head")).toBeNull();
    }
  });

  it("renders hostile text in divider lines as text", () => {
    const failed = renderEntry({
      ...base,
      kind: "turn_failed",
      request_id: "r",
      code: HOSTILE_IMG,
    });
    expect(failed.querySelector("img")).toBeNull();
    expect(failed.textContent).toBe(`Turn failed: ${HOSTILE_IMG}`);
    const manifest = renderEntry({
      ...base,
      kind: "request_manifest",
      request_id: "r",
      model: HOSTILE_IMG,
      prompt_version: RTL,
      tools: [HOSTILE_IMG],
    });
    expect(manifest.querySelector("img")).toBeNull();
    expect(manifest.textContent).toContain("[U+202E]");
    expect(manifest.textContent).not.toContain(String.fromCharCode(0x202e));
  });

  it("right-aligns your messages and puts a reply in a response block", () => {
    const mine = renderEntry(userMessage(1, "hi"));
    const reply = renderEntry({
      ...base,
      kind: "message",
      role: "assistant",
      source: "chat",
      text: "ok",
      call_id: null,
    });
    expect(mine.classList.contains("bubble-user")).toBe(true);
    expect(reply.classList.contains("bubble-assistant")).toBe(true);
    expect(reply.classList.contains("bubble-user")).toBe(false);
  });
});

describe("transcript in the window", () => {
  const s = session({ session_id: "s-rows", title: "Rows" });

  it("keeps a request and its result as adjacent rows, across a page boundary", async () => {
    const entries: TranscriptEntry[] = [
      userMessage(1, "go"),
      {
        seq: 2,
        timestamp: "2026-01-02T10:00:00Z",
        kind: "tool_requested",
        request_id: "r",
        call_id: "c",
        tool: "workspace_list_dir",
        arguments: "{}",
      },
    ];
    const rest: TranscriptEntry[] = [
      {
        seq: 3,
        timestamp: "2026-01-02T10:00:01Z",
        kind: "tool_completed",
        request_id: "r",
        call_id: "c",
        outcome: "succeeded",
        code: null,
        output_bytes: 2,
      },
    ];
    await start({
      session_tree: treeOf(s),
      session_transcript: (args) =>
        args.after_seq === null || args.after_seq === undefined
          ? page(s, entries, { next_after_seq: 2 })
          : page(s, rest),
    });
    (root.querySelector('[data-session-id="s-rows"]') as HTMLElement).click();
    await settle();
    expect(root.querySelectorAll(".tool-block")).toHaveLength(1);
    expect(root.querySelector(".tool-result")).toBeNull();
    buttonByText(root, "Load more").click();
    await settle();
    const rows = Array.from(root.querySelectorAll<HTMLElement>(".transcript > [data-seq]"));
    expect(rows.map((row) => row.dataset.seq)).toEqual(["1", "2", "3"]);
    expect(rows[1]?.classList.contains("tool-block")).toBe(true);
    expect(rows[2]?.classList.contains("tool-result")).toBe(true);
  });

  it("offers no inline waiting marker, since no per-session signal reaches this window", async () => {
    await start({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "go")]),
      approvals_summary: () => ({ pending: 2 }),
    });
    (root.querySelector('[data-session-id="s-rows"]') as HTMLElement).click();
    await settle();
    expect(root.querySelector(".transcript")?.textContent).not.toContain(
      "Waiting for your approval",
    );
  });
});
