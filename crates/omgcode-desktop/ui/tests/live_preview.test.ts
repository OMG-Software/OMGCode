import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { neutralise } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import type { SessionSummary } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import {
  buttonByText,
  type FakeIpc,
  type Handler,
  HOSTILE_IMG,
  installFakeIpc,
  mainHandlers,
  mountRoot,
  page,
  session,
  settle,
  tree,
  userMessage,
} from "./helpers";

const FRAME_MS = 20;
const s = session({ session_id: "s-live", title: "Live" });
const other = session({ session_id: "s-other", title: "Other" });
const running = { session_id: "s-live", state: "running", outcome: null, error: null };
const completed = { ...running, state: "completed", outcome: null };

type Push = (value: unknown) => void;

let app: MainApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;
let push: Push;

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

async function start(handlers: Record<string, Handler> = {}): Promise<void> {
  ipc = installFakeIpc(
    mainHandlers({
      session_tree: treeOf(s, other),
      session_transcript: (args) =>
        args.session_id === "s-other"
          ? page(other, [userMessage(1, "other")])
          : page(s, [userMessage(1, "hi")]),
      chat_subscribe: (args) => {
        push = (args.on_event as { onmessage: Push }).onmessage;
        return null;
      },
      chat_send: () => running,
      chat_status: () => completed,
      ...handlers,
    }),
  );
  root = mountRoot();
  app = startMain(root);
  await settle();
  itemFor("s-live").click();
  await settle();
}

async function send(): Promise<void> {
  const box = root.querySelector(".message-box") as HTMLTextAreaElement;
  box.value = "hello";
  box.dispatchEvent(new Event("input", { bubbles: true }));
  buttonByText(root, "Send").click();
  await settle();
}

function liveEntries(): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(".bubble-live"));
}

function liveText(): HTMLElement {
  const node = root.querySelector<HTMLElement>(".bubble-live [aria-live]");
  if (node === null) {
    throw new Error("No live entry");
  }
  return node;
}

async function emit(...events: unknown[]): Promise<void> {
  for (const event of events) {
    push(event);
  }
  await settle(FRAME_MS);
}

function delta(text: string, request = 1, id = "s-live"): unknown {
  return { kind: "delta", session_id: id, request, text };
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

describe("live assistant preview", () => {
  it("shows a labelled live entry that follows the deltas", async () => {
    await start();
    await send();
    expect(liveEntries()).toHaveLength(0);
    await emit({ kind: "request_started", session_id: "s-live", request: 1 });
    expect(liveEntries()).toHaveLength(1);
    await emit(delta("Hel"), delta("lo"));
    expect(liveText().textContent).toBe("Hello");
    expect(liveText().getAttribute("aria-live")).toBe("polite");
    expect(liveEntries()[0]?.textContent).toContain("test-model (generating, not saved yet)");
    expect(root.querySelector(".transcript")?.lastElementChild).toBe(liveEntries()[0]);
  });

  it("updates the same text node in place and coalesces updates per frame", async () => {
    await start();
    await send();
    await emit(delta("a"));
    const node = liveText().firstChild;
    const entry = liveEntries()[0];
    push(delta("b"));
    push(delta("c"));
    expect(typeof requestAnimationFrame).toBe("function");
    expect(liveText().textContent).toBe("a");
    await settle(FRAME_MS);
    expect(liveText().textContent).toBe("abc");
    expect(liveText().firstChild).toBe(node);
    expect(liveEntries()[0]).toBe(entry);
    expect(liveText().childNodes).toHaveLength(1);
  });

  it("renders hostile preview text as literal text and creates no element", async () => {
    await start();
    await send();
    await emit(delta(`${HOSTILE_IMG}<script>alert(2)</script>`));
    const box = liveText();
    expect(box.textContent).toBe(`${HOSTILE_IMG}<script>alert(2)</script>`);
    expect(box.children).toHaveLength(0);
    expect(root.querySelector(".transcript img")).toBeNull();
    expect(root.querySelector(".transcript script")).toBeNull();
  });

  it("shows bidirectional override characters through neutralise", async () => {
    await start();
    await send();
    await emit(delta("abc\u202Edef\u2066"));
    expect(liveText().textContent).toBe(neutralise("abc\u202Edef\u2066"));
    expect(liveText().textContent).toContain("[U+202E]");
    expect(liveText().textContent).not.toContain("\u202E");
  });

  it("shows a note only when the preview was truncated", async () => {
    await start();
    await send();
    await emit(delta("abc"));
    const note = root.querySelector<HTMLElement>(".live-note");
    expect(note?.hidden).toBe(true);
    await emit({ kind: "preview_truncated", session_id: "s-live", request: 1 });
    expect(note?.hidden).toBe(false);
    expect(note?.textContent).toContain("cut off");
    expect(note?.textContent).toContain("saved reply");
  });

  it("removes the live entry and shows a plain notice when the turn is cancelled", async () => {
    await start();
    await send();
    await emit(delta("provisional"));
    expect(liveEntries()).toHaveLength(1);
    ipc.on("chat_status", () => ({
      ...running,
      state: "failed",
      error: { code: "cancelled", message: "turn was cancelled" },
    }));
    await emit({ kind: "finished", session_id: "s-live" });
    await settle(FRAME_MS);
    expect(liveEntries()).toHaveLength(0);
    const notice = root.querySelector(".notice") as HTMLElement;
    expect(notice.textContent).toBe("Turn cancelled.");
    expect(notice.classList.contains("error")).toBe(false);
    expect((root.querySelector(".composer .running") as HTMLElement).hidden).toBe(true);
  });

  it("removes the live entry when the turn settles and shows the recorded messages", async () => {
    await start();
    await send();
    await emit(delta("provisional"));
    expect(liveEntries()).toHaveLength(1);
    ipc.on("session_transcript", () =>
      page(s, [
        userMessage(1, "hi"),
        {
          seq: 2,
          timestamp: "2026-01-02T10:00:01Z",
          kind: "message",
          role: "assistant",
          source: "chat",
          text: "recorded",
          call_id: null,
        },
      ]),
    );
    await emit({ kind: "finished", session_id: "s-live" });
    await settle(FRAME_MS);
    expect(liveEntries()).toHaveLength(0);
    expect(root.querySelector(".transcript")?.textContent).toContain("recorded");
    expect(root.querySelector(".transcript")?.textContent).not.toContain("provisional");
  });

  it("does not show another session's preview, and shows it again on switching back", async () => {
    await start();
    await send();
    await emit(delta("mine"));
    itemFor("s-other").click();
    await settle(FRAME_MS);
    expect(liveEntries()).toHaveLength(0);
    await emit(delta("still mine"));
    expect(liveEntries()).toHaveLength(0);
    itemFor("s-live").click();
    await settle(FRAME_MS);
    expect(liveEntries()).toHaveLength(1);
    expect(liveText().textContent).toBe("minestill mine");
  });
});

describe("following the end of the transcript", () => {
  const VIEW = 200;

  interface Scroll {
    transcript: HTMLElement;
    top: number;
    height: number;
  }

  /** Gives the transcript a 200-pixel view whose offset is clamped like a browser's. */
  function fakeScroll(height: number, top = 0): Scroll {
    const transcript = root.querySelector<HTMLElement>(".transcript") as HTMLElement;
    const state: Scroll = { transcript, top, height };
    Object.defineProperty(transcript, "scrollHeight", {
      configurable: true,
      get: () => state.height,
    });
    Object.defineProperty(transcript, "clientHeight", { configurable: true, value: VIEW });
    Object.defineProperty(transcript, "scrollTop", {
      configurable: true,
      get: () => state.top,
      set: (value: number) => {
        state.top = Math.max(0, Math.min(value, state.height - VIEW));
      },
    });
    return state;
  }

  /** The operator moves the scrollbar; the browser then fires `scroll`. */
  function scrollTo(state: Scroll, top: number): void {
    state.transcript.scrollTop = top;
    state.transcript.dispatchEvent(new Event("scroll"));
  }

  function scrollToBottom(state: Scroll): void {
    scrollTo(state, state.height);
  }

  it("scrolls to the end when a message is sent, even from higher up", async () => {
    await start();
    const state = fakeScroll(1000);
    scrollToBottom(state);
    scrollTo(state, 100);
    await send();
    expect(state.top).toBe(800);
  });

  it("follows new content until scrolled up, and again after scrolling back down", async () => {
    await start();
    const state = fakeScroll(1000);
    await send();
    state.height = 1200;
    await emit(delta("first"));
    expect(state.top).toBe(1000);

    scrollTo(state, 500);
    state.height = 1400;
    await emit(delta(" second"));
    expect(state.top).toBe(500);

    scrollToBottom(state);
    state.height = 1600;
    await emit(delta(" third"));
    expect(state.top).toBe(1400);
  });

  it("does not stop following when content grows without a scroll up", async () => {
    await start();
    const state = fakeScroll(1000);
    await send();
    // Growth alone fires no scroll event; a later event at the end keeps the transcript pinned.
    state.height = 1300;
    state.transcript.dispatchEvent(new Event("scroll"));
    await emit(delta("more"));
    expect(state.top).toBe(1100);
  });

  it("keeps the end in view when the turn ends, and keeps the place if scrolled up", async () => {
    await start();
    const state = fakeScroll(1000);
    await send();
    state.height = 1500;
    await emit({ kind: "finished", session_id: "s-live" });
    expect(state.top).toBe(1300);

    await send();
    scrollTo(state, 300);
    state.height = 1800;
    await emit({ kind: "finished", session_id: "s-live" });
    expect(state.top).toBe(300);
  });
});
