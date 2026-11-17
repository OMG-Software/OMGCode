import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { ChatStatus } from "../src/ipc/types";
import {
  CHAT_POLL_MS,
  CHAT_RECONCILE_MS,
  type ChatPreview,
  type ChatRunner,
  createChatRunner,
  PREVIEW_LIMIT,
  UNTRACKED_PREVIEW_SESSIONS,
} from "../src/main/chat_runner";
import { type FakeIpc, installFakeIpc, SETTINGS, settle } from "./helpers";

const ID = "s-1";
const OTHER = "s-2";
const running: ChatStatus = { session_id: ID, state: "running", outcome: null, error: null };
const completed: ChatStatus = { ...running, state: "completed" };

type Push = (value: unknown) => void;

let ipc: FakeIpc;
let push: Push;
let runner: ChatRunner;
let statuses: ChatStatus[];
let previews: Array<[string, ChatPreview | null]>;

function setup(options: { subscribe?: boolean; statuses?: ChatStatus[] } = {}): void {
  const replies = options.statuses ?? [completed];
  let n = 0;
  ipc = installFakeIpc({
    chat_subscribe: (args) => {
      if (options.subscribe === false) {
        throw { code: "unknown_command", message: "chat_subscribe" };
      }
      push = (args.on_event as { onmessage: Push }).onmessage;
      return null;
    },
    chat_send: (args) => ({ ...running, session_id: args.session_id as string }),
    chat_status: () => replies[Math.min(n++, replies.length - 1)],
    chat_cancel: () => ({ ...running, state: "cancelling" }),
  });
  statuses = [];
  previews = [];
  runner = createChatRunner({
    onStatus: (status) => statuses.push(status),
    onPreview: (id, preview) => previews.push([id, preview]),
  });
}

async function start(): Promise<void> {
  await runner.send(ID, "hello", SETTINGS);
  await settle();
}

function lastPreview(): ChatPreview | null {
  return previews[previews.length - 1]?.[1] ?? null;
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  runner.dispose();
  setInvoker(null);
  vi.useRealTimers();
});

describe("subscription and settling", () => {
  it("subscribes once and does not poll chat_status on a 750 ms timer", async () => {
    setup();
    await start();
    await runner.send(OTHER, "again", SETTINGS);
    expect(ipc.callsTo("chat_subscribe")).toHaveLength(1);
    await settle(CHAT_POLL_MS * 4);
    expect(ipc.callsTo("chat_status")).toHaveLength(0);
    expect(runner.isActive(ID)).toBe(true);
  });

  it("reads chat_status exactly once when finished arrives", async () => {
    setup();
    await start();
    push({ kind: "finished", session_id: ID });
    await settle();
    expect(ipc.callsTo("chat_status")).toHaveLength(1);
    expect(statuses[statuses.length - 1]).toEqual(completed);
    expect(runner.isActive(ID)).toBe(false);
    await settle(CHAT_RECONCILE_MS * 2);
    expect(ipc.callsTo("chat_status")).toHaveLength(1);
  });

  it("stays active when the status after finished is still running", async () => {
    setup({ statuses: [running, completed] });
    await start();
    push({ kind: "finished", session_id: ID });
    await settle();
    expect(runner.isActive(ID)).toBe(true);
    await settle(CHAT_RECONCILE_MS);
    expect(ipc.callsTo("chat_status")).toHaveLength(2);
    expect(runner.isActive(ID)).toBe(false);
  });

  it("settles a turn whose finished event never arrives at the reconcile check", async () => {
    setup();
    await start();
    await settle(CHAT_RECONCILE_MS - 1);
    expect(ipc.callsTo("chat_status")).toHaveLength(0);
    await settle(1);
    expect(ipc.callsTo("chat_status")).toHaveLength(1);
    expect(statuses[statuses.length - 1]).toEqual(completed);
    expect(runner.isActive(ID)).toBe(false);
  });

  it("reads the status once when finished arrives before chat_send returns", async () => {
    setup();
    ipc.on("chat_send", () => {
      push({ kind: "request_started", session_id: ID, request: 1 });
      push({ kind: "delta", session_id: ID, request: 1, text: "early" });
      push({ kind: "finished", session_id: ID });
      return running;
    });
    await start();
    expect(ipc.callsTo("chat_status")).toHaveLength(1);
    expect(runner.isActive(ID)).toBe(false);
    expect(lastPreview()).toBeNull();
  });

  it("falls back to the 750 ms poll when chat_subscribe rejects", async () => {
    setup({ subscribe: false });
    await start();
    await settle(CHAT_POLL_MS);
    expect(ipc.callsTo("chat_status")).toHaveLength(1);
    expect(runner.isActive(ID)).toBe(false);
  });

  it("polls a watched session fast once the subscription is known to have failed", async () => {
    setup({ subscribe: false });
    runner.watch(ID);
    await settle(CHAT_POLL_MS);
    expect(ipc.callsTo("chat_status")).toHaveLength(1);
  });

  it("reports a failed status when a status read rejects", async () => {
    setup();
    await start();
    ipc.on("chat_status", () => {
      throw { code: "unknown", message: "boom" };
    });
    push({ kind: "finished", session_id: ID });
    await settle();
    expect(statuses[statuses.length - 1]).toMatchObject({ state: "failed" });
    expect(runner.isActive(ID)).toBe(false);
  });

  it("ignores events after dispose", async () => {
    setup();
    await start();
    runner.dispose();
    push({ kind: "finished", session_id: ID });
    push({ kind: "delta", session_id: ID, request: 1, text: "x" });
    await settle();
    expect(ipc.callsTo("chat_status")).toHaveLength(0);
    expect(runner.preview(ID)).toBeNull();
  });
});

describe("preview state", () => {
  it("resets on request_started and appends deltas in order", async () => {
    setup();
    await start();
    push({ kind: "request_started", session_id: ID, request: 1 });
    expect(lastPreview()).toEqual({ request: 1, text: "", truncated: false });
    push({ kind: "delta", session_id: ID, request: 1, text: "Hel" });
    push({ kind: "delta", session_id: ID, request: 1, text: "lo" });
    expect(runner.preview(ID)).toEqual({ request: 1, text: "Hello", truncated: false });
    push({ kind: "request_started", session_id: ID, request: 2 });
    expect(runner.preview(ID)).toEqual({ request: 2, text: "", truncated: false });
  });

  it("ignores a delta for an older request and starts a newer one", async () => {
    setup();
    await start();
    push({ kind: "request_started", session_id: ID, request: 2 });
    push({ kind: "delta", session_id: ID, request: 2, text: "new" });
    push({ kind: "delta", session_id: ID, request: 1, text: "old" });
    expect(runner.preview(ID)?.text).toBe("new");
    push({ kind: "delta", session_id: ID, request: 3, text: "next" });
    expect(runner.preview(ID)).toEqual({ request: 3, text: "next", truncated: false });
  });

  it("sets the truncated flag", async () => {
    setup();
    await start();
    push({ kind: "request_started", session_id: ID, request: 1 });
    push({ kind: "delta", session_id: ID, request: 1, text: "abc" });
    push({ kind: "preview_truncated", session_id: ID, request: 1 });
    expect(runner.preview(ID)).toEqual({ request: 1, text: "abc", truncated: true });
  });

  it("clears the preview when the turn settles", async () => {
    setup();
    await start();
    push({ kind: "delta", session_id: ID, request: 1, text: "abc" });
    push({ kind: "finished", session_id: ID });
    await settle();
    expect(runner.preview(ID)).toBeNull();
    expect(lastPreview()).toBeNull();
  });

  it("bounds the client-side preview at 64 KiB", async () => {
    setup();
    await start();
    push({ kind: "request_started", session_id: ID, request: 1 });
    const piece = "a".repeat(1024);
    for (let i = 0; i < 80; i++) {
      push({ kind: "delta", session_id: ID, request: 1, text: piece });
    }
    const preview = runner.preview(ID);
    expect(preview?.text.length).toBe(PREVIEW_LIMIT);
    expect(preview?.truncated).toBe(true);
  });

  it("does not split a surrogate pair at the bound", async () => {
    setup();
    await start();
    push({ kind: "delta", session_id: ID, request: 1, text: "a".repeat(PREVIEW_LIMIT - 1) });
    push({ kind: "delta", session_id: ID, request: 1, text: "\u{1F600}" });
    expect(runner.preview(ID)?.text).toBe("a".repeat(PREVIEW_LIMIT - 1));
  });

  it("does not track a session it did not start or watch, and does not throw", async () => {
    setup();
    await start();
    push({ kind: "request_started", session_id: OTHER, request: 1 });
    push({ kind: "delta", session_id: OTHER, request: 1, text: "stray" });
    push({ kind: "finished", session_id: OTHER });
    await settle();
    expect(runner.isActive(OTHER)).toBe(false);
    expect(runner.preview(OTHER)).toBeNull();
    expect(previews).toEqual([]);
    expect(ipc.callsTo("chat_status")).toHaveLength(0);
  });

  it("keeps previews apart per session", async () => {
    setup();
    await start();
    await runner.send(OTHER, "again", SETTINGS);
    push({ kind: "delta", session_id: ID, request: 1, text: "one" });
    push({ kind: "delta", session_id: OTHER, request: 1, text: "two" });
    expect(runner.preview(ID)?.text).toBe("one");
    expect(runner.preview(OTHER)?.text).toBe("two");
  });

  it("tracks request_started and delta for a watched session", async () => {
    setup();
    runner.watch(ID);
    await settle();
    push({ kind: "request_started", session_id: ID, request: 1 });
    push({ kind: "delta", session_id: ID, request: 1, text: "replayed" });
    expect(runner.preview(ID)?.text).toBe("replayed");
  });

  it("ignores malformed events without throwing", async () => {
    setup();
    await start();
    const bad: unknown[] = [
      null,
      "delta",
      42,
      [],
      {},
      { kind: "delta" },
      { kind: "delta", session_id: ID, request: 1 },
      { kind: "delta", session_id: ID, request: "1", text: "x" },
      { kind: "delta", session_id: ID, request: -1, text: "x" },
      { kind: "delta", session_id: ID, request: 1.5, text: "x" },
      { kind: "delta", session_id: ID, request: 1, text: 5 },
      { kind: "delta", session_id: 7, request: 1, text: "x" },
      { kind: "request_started", session_id: ID },
      { kind: "preview_truncated", session_id: ID, request: Number.MAX_SAFE_INTEGER + 1 },
      { kind: "finished" },
      { kind: "explode", session_id: ID, request: 1 },
    ];
    for (const value of bad) {
      expect(() => push(value)).not.toThrow();
    }
    await settle();
    expect(runner.preview(ID)).toBeNull();
    expect(ipc.callsTo("chat_status")).toHaveLength(0);
    expect(runner.isActive(ID)).toBe(true);
  });

  it("keeps delivering events after a handler throws", async () => {
    ipc = installFakeIpc({
      chat_subscribe: (args) => {
        push = (args.on_event as { onmessage: Push }).onmessage;
        return null;
      },
      chat_send: () => running,
    });
    let calls = 0;
    runner = createChatRunner({
      onStatus: () => undefined,
      onPreview: () => {
        calls += 1;
        if (calls === 1) {
          throw new Error("handler failure");
        }
      },
    });
    await start();
    push({ kind: "delta", session_id: ID, request: 1, text: "a" });
    push({ kind: "delta", session_id: ID, request: 1, text: "b" });
    expect(runner.preview(ID)?.text).toBe("ab");
  });

  describe("sessions not tracked yet", () => {
    async function replay(id: string, text: string): Promise<void> {
      push({ kind: "request_started", session_id: id, request: 1 });
      push({ kind: "delta", session_id: id, request: 1, text });
    }

    it("keeps a replayed preview silently and delivers it on watch", async () => {
      setup();
      await start();
      const before = statuses.length;
      await replay(OTHER, "before");
      expect(previews).toEqual([]);
      expect(statuses).toHaveLength(before);
      expect(ipc.callsTo("chat_status")).toHaveLength(0);
      expect(runner.isActive(OTHER)).toBe(false);
      runner.watch(OTHER);
      expect(previews).toEqual([[OTHER, { request: 1, text: "before", truncated: false }]]);
      push({ kind: "delta", session_id: OTHER, request: 1, text: " after" });
      expect(runner.preview(OTHER)?.text).toBe("before after");
    });

    it("drops the stored preview on finished without reading the status", async () => {
      setup();
      await start();
      await replay(OTHER, "gone");
      push({ kind: "preview_truncated", session_id: OTHER, request: 1 });
      expect(runner.preview(OTHER)?.truncated).toBe(true);
      push({ kind: "finished", session_id: OTHER });
      await settle();
      expect(runner.preview(OTHER)).toBeNull();
      expect(previews).toEqual([]);
      expect(ipc.callsTo("chat_status")).toHaveLength(0);
    });

    it("keeps at most four untracked previews and evicts the oldest", async () => {
      setup();
      await start();
      push({ kind: "delta", session_id: ID, request: 1, text: "tracked" });
      const ids = Array.from({ length: UNTRACKED_PREVIEW_SESSIONS + 1 }, (_, i) => `u-${i}`);
      for (const id of ids) {
        await replay(id, id);
      }
      expect(runner.preview(ids[0] as string)).toBeNull();
      for (const id of ids.slice(1)) {
        expect(runner.preview(id)?.text).toBe(id);
      }
      expect(runner.preview(ID)?.text).toBe("tracked");
    });

    it("starts a send with no stale stored preview", async () => {
      setup();
      await start();
      await replay(OTHER, "stale");
      await runner.send(OTHER, "new", SETTINGS);
      expect(runner.preview(OTHER)).toBeNull();
      expect(previews).toEqual([]);
      push({ kind: "delta", session_id: OTHER, request: 1, text: "fresh" });
      expect(runner.preview(OTHER)?.text).toBe("fresh");
    });
  });
});
