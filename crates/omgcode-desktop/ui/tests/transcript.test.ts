import { describe, expect, it } from "vitest";
import type { TranscriptEntry } from "../src/ipc/types";
import { renderEntry } from "../src/main/transcript";
import { HOSTILE_IMG } from "./helpers";

const base = { seq: 7, timestamp: "2026-01-02T10:00:00Z" };

const ENTRIES: Array<[TranscriptEntry, string, string]> = [
  [
    { ...base, kind: "message", role: "user", source: "chat", text: "hi\nthere", call_id: null },
    "bubble-user",
    "hi\nthere",
  ],
  [
    { ...base, kind: "message", role: "assistant", source: "chat", text: "ok", call_id: null },
    "bubble-assistant",
    "Assistant",
  ],
  [
    { ...base, kind: "message", role: "tool", source: "tool", text: "out", call_id: "c1" },
    "bubble-tool",
    "Tool",
  ],
  [
    {
      ...base,
      kind: "tool_requested",
      request_id: "r",
      call_id: "c1",
      tool: "workspace_read_file",
      arguments: '{"path":"a"}',
    },
    "tool-block",
    "workspace_read_file",
  ],
  [
    {
      ...base,
      kind: "tool_completed",
      request_id: "r",
      call_id: "c1",
      outcome: "denied",
      code: "approval_required",
      output_bytes: 12,
    },
    "tool-denied",
    "denied (approval_required), 12 bytes",
  ],
  [{ ...base, kind: "turn_started", request_id: "r" }, "turn", "Turn started"],
  [{ ...base, kind: "turn_completed", request_id: "r" }, "turn", "Turn completed"],
  [
    { ...base, kind: "turn_failed", request_id: "r", code: "boom" },
    "turn-failed",
    "Turn failed: boom",
  ],
  [
    {
      ...base,
      kind: "request_manifest",
      request_id: "r",
      model: "m1",
      prompt_version: "v3",
      tools: ["a", "b"],
    },
    "manifest",
    "m1, prompt v3, tools: a, b",
  ],
  [
    { ...base, kind: "context_compacted", summary: "gist", compacted_messages: 4, model: "m1" },
    "summary-block",
    "Summary of earlier conversation (generated, unverified)",
  ],
  [{ ...base, kind: "session_archived" }, "divider", "Session archived"],
  [{ ...base, kind: "session_restored" }, "divider", "Session restored"],
];

describe("renderEntry", () => {
  it.each(ENTRIES)("renders %j distinctly", (entry, className, expectedText) => {
    const node = renderEntry(entry);
    expect(node.classList.contains(className)).toBe(true);
    expect(node.dataset.seq).toBe("7");
    expect(node.textContent).toContain(expectedText);
  });

  it("keeps tool arguments in a pre element as text", () => {
    const node = renderEntry({
      ...base,
      kind: "tool_requested",
      request_id: "r",
      call_id: "c",
      tool: "t",
      arguments: HOSTILE_IMG,
    });
    expect(node.querySelector("pre")?.textContent).toBe(HOSTILE_IMG);
    expect(node.querySelector("img")).toBeNull();
  });

  it("renders message text without creating elements", () => {
    const node = renderEntry({
      ...base,
      kind: "message",
      role: "assistant",
      source: "chat",
      text: `${HOSTILE_IMG}</script>`,
      call_id: null,
    });
    expect(node.querySelector(".bubble-text")?.textContent).toBe(`${HOSTILE_IMG}</script>`);
    expect(node.querySelector("img, script")).toBeNull();
  });
});
