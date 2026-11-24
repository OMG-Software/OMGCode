import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { ProcessEntry, SessionProcesses } from "../src/ipc/types";
import {
  commandLine,
  createProcessesTab,
  formatDuration,
  outcomeText,
  PROCESSES_EMPTY,
  PROCESSES_NO_SESSION,
  PROCESSES_POLL_MS,
} from "../src/main/processes_tab";
import { createMockBackend } from "../src/mock/backend";
import { type FakeIpc, installFakeIpc, rejection } from "./helpers";

const SESSION = "11111111-1111-4111-8111-111111111111";
const OTHER = "22222222-2222-4222-8222-222222222222";

function entry(overrides: Partial<ProcessEntry> = {}): ProcessEntry {
  return {
    call_id: "call_1",
    state: "finished",
    started_at: "2026-01-01T10:00:00Z",
    finished_at: "2026-01-01T10:00:02Z",
    duration_ms: 1500,
    program: "cargo",
    arguments: ["test", "--lib"],
    text_truncated: false,
    argument_count: 2,
    profile: "rust",
    result: { kind: "exited", exit_code: 0, signal: null },
    limit: null,
    reason: null,
    ...overrides,
  };
}

function processes(entries: ProcessEntry[], overrides: Partial<SessionProcesses> = {}) {
  return { entries, truncated: false, run_details: "available" as const, ...overrides };
}

function mount(handler: (args: Record<string, unknown>) => unknown): {
  tab: ReturnType<typeof createProcessesTab>;
  ipc: FakeIpc;
} {
  const ipc = installFakeIpc({ session_processes: handler });
  const tab = createProcessesTab();
  document.body.replaceChildren(tab.element);
  return { tab, ipc };
}

async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
}

function texts(tab: { element: HTMLElement }, selector: string): string[] {
  return Array.from(tab.element.querySelectorAll(selector)).map((node) => node.textContent ?? "");
}

describe("the processes tab", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    setInvoker(null);
    document.body.replaceChildren();
  });

  it("asks for a session before showing anything and calls nothing", () => {
    const { tab, ipc } = mount(() => processes([]));

    expect(tab.element.textContent).toContain(PROCESSES_NO_SESSION);
    expect(ipc.calls).toHaveLength(0);
    expect(tab.element.querySelector<HTMLButtonElement>(".processes-refresh")?.disabled).toBe(true);
  });

  it("shows the empty state for a session with no calls", async () => {
    const { tab, ipc } = mount(() => processes([]));

    tab.showSession(SESSION);
    await settle();

    expect(ipc.callsTo("session_processes")[0]?.args).toEqual({ session_id: SESSION });
    expect(tab.element.textContent).toContain(PROCESSES_EMPTY);
    expect(PROCESSES_EMPTY).toBe("No subprocess has run in this session.");
    expect(tab.element.querySelectorAll(".process-entry")).toHaveLength(0);
  });

  it("lists each call with its command, state, time, duration, and outcome", async () => {
    const { tab } = mount(() =>
      processes([
        entry({ call_id: "a" }),
        entry({
          call_id: "b",
          result: { kind: "exited", exit_code: 1, signal: null },
          duration_ms: 40,
        }),
        entry({
          call_id: "c",
          result: { kind: "limit_exceeded", exit_code: null, signal: null },
          limit: "memory",
        }),
        entry({
          call_id: "d",
          state: "refused",
          result: null,
          duration_ms: null,
          reason: "approval_denied",
          finished_at: null,
        }),
        entry({ call_id: "e", state: "unfinished", result: null, finished_at: null }),
      ]),
    );

    tab.showSession(SESSION);
    await settle();

    expect(tab.element.querySelectorAll(".process-entry")).toHaveLength(5);
    expect(texts(tab, ".process-state")).toEqual([
      "finished",
      "finished",
      "finished",
      "refused",
      "unfinished",
    ]);
    expect(texts(tab, ".process-outcome")).toEqual([
      "exit 0",
      "exit 1",
      "stopped: memory limit",
      "refused: approval denied",
      "outcome unknown",
    ]);
    expect(texts(tab, ".process-duration")).toEqual(["1.5 s", "40 ms", "1.5 s", "1.5 s"]);
    const first = tab.element.querySelector<HTMLElement>(".process-command");
    expect(first?.textContent).toBe("cargo test --lib");
    expect(first?.title).toBe("cargo test --lib");
    expect(tab.element.querySelector(".process-time")?.textContent).not.toBe("");
  });

  it("renders program and arguments as text, never as markup", async () => {
    const hostile = '<img src=x onerror="alert(1)"> <b>bold</b>';
    const { tab } = mount(() =>
      processes([entry({ program: "<script>x</script>", arguments: [hostile] })]),
    );

    tab.showSession(SESSION);
    await settle();

    expect(tab.element.querySelector("img, script, b")).toBeNull();
    expect(tab.element.querySelector(".process-command")?.textContent).toContain(hostile);
  });

  it("notes unknown commands, hidden arguments, and shortened text", () => {
    expect(commandLine(entry({ program: null, arguments: null }))).toBe("(command unavailable)");
    expect(commandLine(entry({ program: "make", arguments: null, argument_count: 3 }))).toContain(
      "3 arguments not shown",
    );
    expect(commandLine(entry({ text_truncated: true }))).toContain("shortened");
  });

  it("describes every kind of result", () => {
    const kinds: Array<[ProcessEntry["result"], string]> = [
      [{ kind: "signalled", exit_code: null, signal: 9 }, "signal 9"],
      [{ kind: "deadline_exceeded", exit_code: null, signal: null }, "deadline exceeded"],
      [{ kind: "cancelled", exit_code: null, signal: null }, "cancelled"],
      [{ kind: "not_started", exit_code: null, signal: null }, "did not start"],
    ];
    for (const [result, label] of kinds) {
      expect(outcomeText(entry({ result }))).toBe(label);
    }
    expect(outcomeText(entry({ state: "running", result: null }))).toBe("running");
    expect(outcomeText(entry({ result: null, reason: "command_failed" }))).toBe(
      "finished: command failed",
    );
    expect(formatDuration(125_000)).toBe("2 min 5 s");
  });

  it("shows the truncation and missing run details notes", async () => {
    const { tab } = mount(() =>
      processes([entry()], { truncated: true, run_details: "unavailable" }),
    );

    tab.showSession(SESSION);
    await settle();

    expect(tab.element.textContent).toContain("Older calls are not shown.");
    expect(tab.element.textContent).toContain("Run details are unavailable");
  });

  it("notes a busy run journal", async () => {
    const { tab } = mount(() => processes([entry()], { run_details: "busy" }));

    tab.showSession(SESSION);
    await settle();

    expect(tab.element.textContent).toContain("Run details are busy");
  });

  it("never overlaps polling reads while one is still in flight", async () => {
    let release: (value: SessionProcesses) => void = () => undefined;
    let calls = 0;
    const running = processes([entry({ state: "running", result: null })]);
    const first = running;
    const { tab } = mount(() => {
      calls += 1;
      if (calls === 1) {
        return first;
      }
      return new Promise<SessionProcesses>((resolve) => {
        release = resolve;
      });
    });
    tab.showSession(SESSION);
    await settle();

    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS * 5);
    expect(calls).toBe(2);

    release(running);
    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS);
    expect(calls).toBe(3);
    tab.dispose();
  });

  it("shows an error as text and keeps the last list", async () => {
    let fail = false;
    const { tab } = mount(() =>
      fail ? rejection("busy", "The data is in use <b>now</b>.") : processes([entry()]),
    );
    tab.showSession(SESSION);
    await settle();

    fail = true;
    tab.element.querySelector<HTMLButtonElement>(".processes-refresh")?.click();
    await settle();

    expect(tab.element.querySelector(".error")?.textContent).toBe(
      "The data is in use <b>now</b>. (busy)",
    );
    expect(tab.element.querySelector(".error b")).toBeNull();
    expect(tab.element.querySelectorAll(".process-entry")).toHaveLength(1);
  });

  it("reads again on Refresh", async () => {
    const { tab, ipc } = mount(() => processes([entry()]));
    tab.showSession(SESSION);
    await settle();

    tab.element.querySelector<HTMLButtonElement>(".processes-refresh")?.click();
    await settle();

    expect(ipc.callsTo("session_processes")).toHaveLength(2);
  });

  it("polls while a call runs and stops when nothing runs", async () => {
    let running = true;
    const { tab, ipc } = mount(() =>
      processes([
        running
          ? entry({ state: "running", result: null, finished_at: null, duration_ms: null })
          : entry(),
      ]),
    );
    tab.showSession(SESSION);
    await settle();
    expect(ipc.callsTo("session_processes")).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS);
    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS);
    expect(ipc.callsTo("session_processes")).toHaveLength(3);
    expect(tab.element.querySelector(".process-state-running")).not.toBeNull();

    running = false;
    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS);
    expect(ipc.callsTo("session_processes")).toHaveLength(4);
    expect(tab.element.querySelector(".process-state-running")).toBeNull();

    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS * 5);
    expect(ipc.callsTo("session_processes")).toHaveLength(4);
  });

  it("does not poll while nothing runs", async () => {
    const { tab, ipc } = mount(() => processes([entry(), entry({ state: "unfinished" })]));
    tab.showSession(SESSION);
    await settle();

    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS * 5);

    expect(ipc.callsTo("session_processes")).toHaveLength(1);
  });

  it("clears and stops polling when the session is cleared", async () => {
    const { tab, ipc } = mount(() => processes([entry({ state: "running", result: null })]));
    tab.showSession(SESSION);
    await settle();

    tab.showSession(null);
    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS * 3);

    expect(ipc.callsTo("session_processes")).toHaveLength(1);
    expect(tab.element.querySelectorAll(".process-entry")).toHaveLength(0);
    expect(tab.element.textContent).toContain(PROCESSES_NO_SESSION);
  });

  it("stops polling and ignores a late answer after dispose", async () => {
    let release: (value: SessionProcesses) => void = () => undefined;
    const { tab, ipc } = mount(
      () =>
        new Promise<SessionProcesses>((resolve) => {
          release = resolve;
        }),
    );
    tab.showSession(SESSION);
    await settle();

    tab.dispose();
    release(processes([entry({ state: "running", result: null })]));
    await vi.advanceTimersByTimeAsync(PROCESSES_POLL_MS * 3);

    expect(ipc.callsTo("session_processes")).toHaveLength(1);
    expect(tab.element.querySelectorAll(".process-entry")).toHaveLength(0);
  });

  it("drops an answer that arrives after another session was shown", async () => {
    const pending: Array<(value: SessionProcesses) => void> = [];
    const { tab } = mount(
      () =>
        new Promise<SessionProcesses>((resolve) => {
          pending.push(resolve);
        }),
    );
    tab.showSession(SESSION);
    await settle();
    tab.showSession(OTHER);
    await settle();

    pending[0]?.(processes([entry({ call_id: "stale", program: "stale-program" })]));
    pending[1]?.(processes([entry({ call_id: "fresh", program: "fresh-program" })]));
    await settle();

    expect(tab.element.textContent).toContain("fresh-program");
    expect(tab.element.textContent).not.toContain("stale-program");
  });

  it("works against the mock backend with its fake entries", async () => {
    const backend = createMockBackend();
    setInvoker((command, args) => backend.invoke(command, args));
    const sessions = (await backend.invoke("session_tree", {
      view: "active",
      sort: "updated",
      descending: true,
    })) as { groups: Array<{ sessions: Array<{ session_id: string }> }> };
    const id = sessions.groups[0]?.sessions[0]?.session_id ?? SESSION;
    const tab = createProcessesTab();
    document.body.replaceChildren(tab.element);

    tab.showSession(id);
    await settle();

    expect(tab.element.querySelectorAll(".process-entry")).toHaveLength(5);
    const outcomes = texts(tab, ".process-outcome");
    expect(outcomes).toContain("exit 0");
    expect(outcomes).toContain("exit 1");
    expect(outcomes).toContain("stopped: memory limit");
    expect(outcomes.some((text) => text.startsWith("refused"))).toBe(true);
    tab.dispose();
  });
});
