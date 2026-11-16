import { afterEach, describe, expect, it } from "vitest";
import { chatSubscribe } from "../src/ipc/chat_events";
import { setInvoker } from "../src/ipc/client";
import { type ChatEvent, isChatEvent } from "../src/ipc/types";
import { installFakeIpc } from "./helpers";

afterEach(() => {
  setInvoker(null);
});

describe("chatSubscribe", () => {
  it("passes an on_event object to the invoker and delivers only valid events", async () => {
    const sink: { target: { onmessage: (value: unknown) => void } | null } = { target: null };
    const ipc = installFakeIpc({
      chat_subscribe: (args) => {
        sink.target = args.on_event as { onmessage: (value: unknown) => void };
        return null;
      },
    });
    const received: ChatEvent[] = [];
    await chatSubscribe((event) => received.push(event));
    expect(ipc.callsTo("chat_subscribe")).toHaveLength(1);
    expect(Object.keys(ipc.callsTo("chat_subscribe")[0]?.args ?? {})).toEqual(["on_event"]);
    expect(typeof sink.target?.onmessage).toBe("function");
    const valid: ChatEvent[] = [
      { kind: "request_started", session_id: "s", request: 1 },
      { kind: "delta", session_id: "s", request: 1, text: "hi" },
      { kind: "preview_truncated", session_id: "s", request: 1 },
      { kind: "finished", session_id: "s" },
    ];
    for (const event of valid) {
      sink.target?.onmessage(event);
    }
    sink.target?.onmessage({ kind: "delta", session_id: "s", request: 1 });
    sink.target?.onmessage({ kind: "unknown", session_id: "s" });
    sink.target?.onmessage("finished");
    sink.target?.onmessage(null);
    expect(received).toEqual(valid);
  });

  it("rejects when the command fails", async () => {
    installFakeIpc({});
    await expect(chatSubscribe(() => undefined)).rejects.toMatchObject({
      code: "unknown_command",
    });
  });

  it("keeps delivering after a handler throws", async () => {
    const sink: { target: { onmessage: (value: unknown) => void } | null } = { target: null };
    installFakeIpc({
      chat_subscribe: (args) => {
        sink.target = args.on_event as { onmessage: (value: unknown) => void };
        return null;
      },
    });
    const seen: string[] = [];
    await chatSubscribe((event) => {
      seen.push(event.kind);
      throw new Error("handler failure");
    });
    expect(() => sink.target?.onmessage({ kind: "finished", session_id: "s" })).not.toThrow();
    sink.target?.onmessage({ kind: "finished", session_id: "t" });
    expect(seen).toEqual(["finished", "finished"]);
  });
});

describe("isChatEvent", () => {
  it("accepts the four event shapes and refuses everything else", () => {
    expect(isChatEvent({ kind: "finished", session_id: "s" })).toBe(true);
    expect(isChatEvent({ kind: "delta", session_id: "s", request: 0, text: "" })).toBe(true);
    expect(isChatEvent({ kind: "delta", session_id: "s", request: 1.2, text: "" })).toBe(false);
    expect(isChatEvent({ kind: "request_started", session_id: "s", request: -1 })).toBe(false);
    expect(isChatEvent({ kind: "request_started", session_id: "s", request: Number.NaN })).toBe(
      false,
    );
    expect(isChatEvent({ kind: "finished", session_id: 1 })).toBe(false);
    expect(isChatEvent(undefined)).toBe(false);
  });
});
