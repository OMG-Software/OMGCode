import { describe, expect, it } from "vitest";
import { formatCountdown, formatUpdated, utf8Length } from "../src/format";
import type { ChatOutcome } from "../src/ipc/types";
import {
  composerState,
  isSendKey,
  messageProblem,
  outcomeLine,
  type SendKey,
  sessionActions,
  settingsValid,
  statusLabel,
  titleProblem,
} from "../src/main/model";
import { SETTINGS, session } from "./helpers";

describe("isSendKey", () => {
  const plain: SendKey = {
    key: "Enter",
    shiftKey: false,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    isComposing: false,
    keyCode: 13,
  };

  it("sends on a plain Enter only when Enter to send is on", () => {
    expect(isSendKey(plain, true)).toBe(true);
    expect(isSendKey(plain, false)).toBe(false);
  });

  it("keeps Shift+Enter and Alt+Enter as newlines", () => {
    for (const enterToSend of [true, false]) {
      expect(isSendKey({ ...plain, shiftKey: true }, enterToSend)).toBe(false);
      expect(isSendKey({ ...plain, altKey: true }, enterToSend)).toBe(false);
    }
  });

  it("always sends on Ctrl+Enter or Cmd+Enter", () => {
    for (const enterToSend of [true, false]) {
      expect(isSendKey({ ...plain, ctrlKey: true }, enterToSend)).toBe(true);
      expect(isSendKey({ ...plain, metaKey: true }, enterToSend)).toBe(true);
    }
  });

  it("never sends on an Enter that confirms an input-method composition", () => {
    for (const enterToSend of [true, false]) {
      expect(isSendKey({ ...plain, isComposing: true }, enterToSend)).toBe(false);
      expect(isSendKey({ ...plain, keyCode: 229 }, enterToSend)).toBe(false);
      expect(isSendKey({ ...plain, ctrlKey: true, isComposing: true }, enterToSend)).toBe(false);
    }
  });

  it("ignores other keys", () => {
    expect(isSendKey({ ...plain, key: "a", keyCode: 65 }, true)).toBe(false);
  });
});

describe("composerState", () => {
  it("is hidden without a session, when archived, busy, invalid, or after a load failure", () => {
    expect(composerState(null, false).visible).toBe(false);
    expect(composerState(session({ archived: true }), false).visible).toBe(false);
    expect(composerState(session({ status: "busy" }), false).visible).toBe(false);
    expect(composerState(session({ status: "invalid" }), false).visible).toBe(false);
    expect(composerState(session(), false, "busy").visible).toBe(false);
  });

  it("is visible but disabled while a turn runs or is open", () => {
    for (const status of ["running", "turn_open"] as const) {
      const state = composerState(session({ status }), false);
      expect(state).toMatchObject({ visible: true, enabled: false });
      expect(state.reason).not.toBeNull();
    }
    expect(composerState(session(), true)).toMatchObject({ visible: true, enabled: false });
  });

  it("is enabled for an idle session", () => {
    expect(composerState(session(), false)).toEqual({ visible: true, enabled: true, reason: null });
  });
});

describe("sessionActions", () => {
  it("offers Archive only for idle active sessions", () => {
    expect(sessionActions(session(), false).archive).toBe(true);
    expect(sessionActions(session({ status: "running" }), false).archive).toBe(false);
    expect(sessionActions(session({ archived: true }), false).archive).toBe(false);
    expect(sessionActions(session(), true).archive).toBe(false);
  });

  it("offers Restore for archived, Interrupt for turn_open, Cancel for running", () => {
    expect(sessionActions(session({ archived: true }), false).restore).toBe(true);
    expect(sessionActions(session({ status: "turn_open" }), false).interrupt).toBe(true);
    expect(sessionActions(session({ status: "running" }), false).cancel).toBe(true);
    expect(sessionActions(session(), false)).toEqual({
      archive: true,
      restore: false,
      interrupt: false,
      cancel: false,
    });
  });
});

describe("labels and limits", () => {
  it("maps statuses to chip labels", () => {
    expect(statusLabel("idle")).toBeNull();
    expect(statusLabel("running")).toBe("Running");
    expect(statusLabel("turn_open")).toBe("Turn open");
    expect(statusLabel("busy")).toBe("In use");
    expect(statusLabel("invalid")).toBe("Unreadable");
  });

  it("requires a model and positive context tokens", () => {
    expect(settingsValid(SETTINGS)).toBe(true);
    expect(settingsValid({ ...SETTINGS, model: "  " })).toBe(false);
    expect(settingsValid({ ...SETTINGS, context_tokens: 0 })).toBe(false);
    expect(settingsValid({ ...SETTINGS, context_tokens: Number.NaN })).toBe(false);
  });

  it("measures message and title limits in UTF-8 bytes", () => {
    expect(messageProblem("")).not.toBeNull();
    expect(messageProblem("hello")).toBeNull();
    expect(messageProblem("a".repeat(16 * 1024))).toBeNull();
    expect(messageProblem("a".repeat(16 * 1024 + 1))).not.toBeNull();
    expect(messageProblem("é".repeat(8 * 1024 + 1))).not.toBeNull();
    expect(titleProblem("x".repeat(256))).toBeNull();
    expect(titleProblem("€".repeat(86))).not.toBeNull();
    expect(titleProblem("   ")).not.toBeNull();
    expect(utf8Length("€")).toBe(3);
  });

  it("formats the outcome line", () => {
    const outcome: ChatOutcome = {
      finish_reason: "stop",
      tools_mode: "disabled",
      tools_disabled_reason: "no preference",
      rounds: 2,
      tool_calls: 1,
      withheld_tool_results: 3,
      context: {
        estimated_tokens: 100,
        trigger_tokens: 600,
        context_tokens: 1000,
        compacted: false,
      },
    };
    const line = outcomeLine(outcome);
    expect(line).toContain("Finished: stop");
    expect(line).toContain("tools disabled (no preference)");
    expect(line).toContain("2 rounds");
    expect(line).toContain("1 tool calls");
    expect(line).toContain("3 withheld tool results");
    expect(line).toContain("context 100 of 1000 tokens, compaction at 600");
    expect(outcomeLine({ ...outcome, tools_mode: "enabled", context: null })).toContain(
      "tools enabled",
    );
  });
});

describe("format", () => {
  it("formats countdowns and unknown times", () => {
    expect(formatCountdown(65)).toBe("01:05");
    expect(formatCountdown(-3)).toBe("00:00");
    expect(formatUpdated(null)).toBe("unknown");
    expect(formatUpdated("not a date")).toBe("unknown");
    const now = Date.parse("2026-01-02T10:00:00Z");
    expect(formatUpdated("2026-01-02T09:00:00Z", now)).not.toBe("unknown");
  });
});
