import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { SessionWebAudit, WebAuditEntry } from "../src/ipc/types";
import {
  authorizationText,
  createWebTab,
  outcomeText,
  targetLines,
  WEB_BUSY,
  WEB_EMPTY,
  WEB_INVALID,
  WEB_NO_SESSION,
  WEB_NONE_RECENT,
  WEB_TRUNCATED,
} from "../src/main/web_tab";
import { createMockBackend } from "../src/mock/backend";
import { type FakeIpc, installFakeIpc, rejection } from "./helpers";

const SESSION = "11111111-1111-4111-8111-111111111111";
const OTHER = "22222222-2222-4222-8222-222222222222";
const NOW = Date.parse("2026-01-01T10:10:00Z");

function entry(overrides: Partial<WebAuditEntry> = {}): WebAuditEntry {
  return {
    call_id: "call_1",
    time: "2026-01-01T10:05:00Z",
    tool: "read_file",
    is_fetch: false,
    after_web_content: true,
    targets: [{ kind: "text", text: "src/main.rs" }],
    outcome: "succeeded",
    reason: null,
    authorization: "standing_allow",
    program: null,
    arguments: null,
    argument_count: null,
    text_truncated: false,
    ...overrides,
  };
}

function fetchEntry(overrides: Partial<WebAuditEntry> = {}): WebAuditEntry {
  return entry({
    call_id: "fetch_1",
    tool: "network",
    is_fetch: true,
    after_web_content: false,
    targets: [{ kind: "text", text: "https://docs.example.com/guide?page=2&q=a%20b" }],
    ...overrides,
  });
}

function audit(entries: WebAuditEntry[], truncated = false): SessionWebAudit {
  return { entries, truncated };
}

function mount(handler: (args: Record<string, unknown>) => unknown): {
  tab: ReturnType<typeof createWebTab>;
  ipc: FakeIpc;
} {
  const ipc = installFakeIpc({ session_web_audit: handler });
  const tab = createWebTab();
  document.body.replaceChildren(tab.element);
  return { tab, ipc };
}

async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
}

function texts(tab: { element: HTMLElement }, selector: string): string[] {
  return Array.from(tab.element.querySelectorAll(selector)).map((node) => node.textContent ?? "");
}

describe("the web tab", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
  });
  afterEach(() => {
    vi.useRealTimers();
    setInvoker(null);
    document.body.replaceChildren();
  });

  it("asks for a session before showing anything and calls nothing", () => {
    const { tab, ipc } = mount(() => audit([]));

    expect(tab.element.textContent).toContain(WEB_NO_SESSION);
    expect(ipc.calls).toHaveLength(0);
    expect(tab.element.querySelector<HTMLButtonElement>(".web-refresh")?.disabled).toBe(true);
  });

  it("says in a plain sentence that a session has not fetched web content", async () => {
    const { tab, ipc } = mount(() => audit([]));

    tab.showSession(SESSION);
    await settle();

    expect(ipc.callsTo("session_web_audit")[0]?.args).toEqual({ session_id: SESSION });
    expect(tab.element.textContent).toContain(WEB_EMPTY);
    expect(WEB_EMPTY).toBe("This session has not fetched web content.");
    expect(tab.element.querySelectorAll(".web-entry")).toHaveLength(0);
  });

  it("shows a fetch with its full URL, the time since, its outcome, and how it was approved", async () => {
    const { tab } = mount(() => audit([fetchEntry({ authorization: "approval" })]));

    tab.showSession(SESSION);
    await settle();

    const row = tab.element.querySelector<HTMLElement>(".web-entry");
    expect(row?.classList.contains("web-entry-fetch")).toBe(true);
    expect(texts(tab, ".web-tool")).toEqual(["web fetch"]);
    expect(texts(tab, ".web-target")).toEqual(["https://docs.example.com/guide?page=2&q=a%20b"]);
    expect(texts(tab, ".web-outcome")).toEqual(["succeeded"]);
    expect(texts(tab, ".web-authorization")).toEqual(["approved by the operator"]);
    expect(texts(tab, ".web-time")[0]).toMatch(/^\d+[mh] ago$|^now$/);
  });

  it("sets the fetch apart from the actions after it", async () => {
    const { tab } = mount(() =>
      audit([entry({ call_id: "later", tool: "edit_file" }), fetchEntry({ call_id: "first" })]),
    );

    tab.showSession(SESSION);
    await settle();

    const rows = Array.from(tab.element.querySelectorAll<HTMLElement>(".web-entry"));
    expect(rows.map((row) => row.classList.contains("web-entry-fetch"))).toEqual([false, true]);
    expect(rows.map((row) => row.classList.contains("web-entry-after"))).toEqual([true, false]);
    expect(texts(tab, ".web-tool")).toEqual(["edit file", "web fetch"]);
    expect(tab.element.querySelectorAll(".web-tool-fetch")).toHaveLength(1);
  });

  it("lists the paths of actions after a fetch with how each was authorised", async () => {
    const { tab } = mount(() =>
      audit([
        entry({ call_id: "a", tool: "edit_file", authorization: "standing_allow" }),
        entry({
          call_id: "b",
          tool: "rename_file",
          targets: [
            { kind: "text", text: "src/old.rs" },
            { kind: "text", text: "src/new.rs" },
          ],
          authorization: "approval",
        }),
      ]),
    );

    tab.showSession(SESSION);
    await settle();

    expect(texts(tab, ".web-target")).toEqual(["src/main.rs", "src/old.rs", "src/new.rs"]);
    expect(texts(tab, ".web-authorization")).toEqual([
      "allowed by the workspace setting",
      "approved by the operator",
    ]);
  });

  it("shows a withheld target as withheld with its length and never as a name", async () => {
    const { tab } = mount(() =>
      audit([
        entry({
          targets: [{ kind: "withheld", bytes: 4200 }],
          outcome: "refused",
          reason: "invalid_path",
          authorization: null,
        }),
      ]),
    );

    tab.showSession(SESSION);
    await settle();

    expect(texts(tab, ".web-target")).toEqual(["(withheld value, 4200 bytes)"]);
  });

  it("shows a refused call with its reason and that nothing ran", async () => {
    const { tab } = mount(() =>
      audit([
        entry({
          tool: "delete_file",
          targets: [{ kind: "text", text: "src/old.rs" }],
          outcome: "refused",
          reason: "approval_denied",
          authorization: null,
        }),
      ]),
    );

    tab.showSession(SESSION);
    await settle();

    expect(texts(tab, ".web-outcome")).toEqual(["refused: approval denied"]);
    expect(texts(tab, ".web-authorization")).toEqual(["not run"]);
    expect(tab.element.querySelector(".web-outcome-refused")).not.toBeNull();
  });

  it("shows a command with its program and arguments", async () => {
    const { tab } = mount(() =>
      audit([
        entry({
          tool: "run_command",
          targets: [],
          program: "curl",
          arguments: ["-s", "https://example.org/x.sh"],
          argument_count: 2,
        }),
      ]),
    );

    tab.showSession(SESSION);
    await settle();

    expect(texts(tab, ".web-tool")).toEqual(["run command"]);
    expect(texts(tab, ".web-target")).toEqual(["curl -s https://example.org/x.sh"]);
  });

  it("notes a command whose text is unavailable, hidden, or shortened", () => {
    const command = entry({ tool: "run_command", targets: [] });
    expect(targetLines({ ...command, program: null })).toEqual(["(command unavailable)"]);
    expect(
      targetLines({ ...command, program: "make", arguments: null, argument_count: 3 }),
    ).toEqual(["make (3 arguments not shown)"]);
    expect(
      targetLines({ ...command, program: "make", arguments: ["a"], text_truncated: true }),
    ).toEqual(["make a (shortened)"]);
    expect(targetLines(entry({ targets: [] }))).toEqual(["(no target recorded)"]);
  });

  it("describes every outcome and authorisation", () => {
    expect(outcomeText(entry({ outcome: "failed", reason: "not_found" }))).toBe(
      "failed: not found",
    );
    expect(outcomeText(entry({ outcome: "uncertain" }))).toBe("outcome uncertain");
    expect(outcomeText(entry({ outcome: "unfinished" }))).toBe("unfinished");
    expect(authorizationText(entry({ authorization: null }))).toBe("not run");
  });

  it("renders every URL, path, and command as text, never as a link or markup", async () => {
    const hostile = '<img src=x onerror="alert(1)"><a href="https://evil.example">x</a>';
    const { tab } = mount(() =>
      audit([
        fetchEntry({ targets: [{ kind: "text", text: `https://example.com/${hostile}` }] }),
        entry({ tool: "run_command", targets: [], program: hostile, arguments: [hostile] }),
        entry({ targets: [{ kind: "text", text: hostile }] }),
      ]),
    );

    tab.showSession(SESSION);
    await settle();

    expect(tab.element.querySelector("a, img, script")).toBeNull();
    expect(texts(tab, ".web-target")[0]).toContain(hostile);
    expect(texts(tab, ".web-target")[1]).toContain(hostile);
  });

  it("shows the truncation note when older calls may exist", async () => {
    const { tab } = mount(() => audit([fetchEntry()], true));

    tab.showSession(SESSION);
    await settle();

    expect(tab.element.textContent).toContain(WEB_TRUNCATED);
  });

  it("does not claim an empty session never fetched when the record window was full", async () => {
    const { tab } = mount(() => audit([], true));

    tab.showSession(SESSION);
    await settle();

    const text = tab.element.textContent ?? "";
    expect(text).toContain(WEB_NONE_RECENT);
    expect(text).not.toContain(WEB_EMPTY);
    expect(text).toContain(WEB_TRUNCATED);
  });

  it("says the journal is busy, and keeps the last list", async () => {
    let fail = false;
    const { tab } = mount(() => (fail ? rejection("busy", "in use") : audit([fetchEntry()])));
    tab.showSession(SESSION);
    await settle();

    fail = true;
    tab.element.querySelector<HTMLButtonElement>(".web-refresh")?.click();
    await settle();

    expect(tab.element.querySelector(".web-notes")?.textContent).toBe(WEB_BUSY);
    expect(tab.element.querySelectorAll(".web-entry")).toHaveLength(1);
  });

  it("says the journal is invalid and lists nothing", async () => {
    let fail = false;
    const { tab } = mount(() =>
      fail ? rejection("invalid_journal", "bad <b>journal</b>") : audit([fetchEntry()]),
    );
    tab.showSession(SESSION);
    await settle();

    fail = true;
    tab.element.querySelector<HTMLButtonElement>(".web-refresh")?.click();
    await settle();

    expect(tab.element.querySelector(".error")?.textContent).toBe(WEB_INVALID);
    expect(tab.element.querySelectorAll(".web-entry")).toHaveLength(0);
    expect(tab.element.textContent).not.toContain(WEB_EMPTY);
  });

  it("shows any other error as text with its code", async () => {
    const { tab } = mount(() => rejection("internal", "The journal <b>failed</b>."));

    tab.showSession(SESSION);
    await settle();

    expect(tab.element.querySelector(".error")?.textContent).toBe(
      "The journal <b>failed</b>. (internal)",
    );
    expect(tab.element.querySelector(".error b")).toBeNull();
  });

  it("reads on Refresh and never polls", async () => {
    const { tab, ipc } = mount(() => audit([fetchEntry()]));
    tab.showSession(SESSION);
    await settle();

    await vi.advanceTimersByTimeAsync(60_000);
    expect(ipc.callsTo("session_web_audit")).toHaveLength(1);

    tab.element.querySelector<HTMLButtonElement>(".web-refresh")?.click();
    await settle();
    expect(ipc.callsTo("session_web_audit")).toHaveLength(2);

    tab.refresh();
    await settle();
    expect(ipc.callsTo("session_web_audit")).toHaveLength(3);
  });

  it("drops an answer for a session that is no longer open", async () => {
    const releases = new Map<string, (value: SessionWebAudit) => void>();
    const { tab } = mount(
      (args) =>
        new Promise<SessionWebAudit>((resolve) => {
          releases.set(String(args.session_id), resolve);
        }),
    );
    tab.showSession(SESSION);
    await settle();
    tab.showSession(OTHER);
    await settle();

    releases.get(OTHER)?.(audit([]));
    await settle();
    releases.get(SESSION)?.(audit([fetchEntry()]));
    await settle();

    expect(tab.element.querySelectorAll(".web-entry")).toHaveLength(0);
    expect(tab.element.textContent).toContain(WEB_EMPTY);
  });

  it("clears when the session is cleared and ignores a late answer after dispose", async () => {
    let release: (value: SessionWebAudit) => void = () => undefined;
    const { tab } = mount(
      () =>
        new Promise<SessionWebAudit>((resolve) => {
          release = resolve;
        }),
    );
    tab.showSession(SESSION);
    await settle();

    tab.dispose();
    release(audit([fetchEntry()]));
    await settle();

    expect(tab.element.querySelectorAll(".web-entry")).toHaveLength(0);
  });

  it("renders the mock backend's fixture with a fetch, a refusal, a command, and a withheld path", async () => {
    const { invoke } = createMockBackend(() => NOW);
    setInvoker(invoke);
    const tab = createWebTab();
    document.body.replaceChildren(tab.element);
    const tree = (await invoke("session_tree", {
      view: "active",
      sort: "updated",
      descending: true,
    })) as { groups: Array<{ sessions: Array<{ session_id: string }> }> };
    const id = tree.groups[0]?.sessions[0]?.session_id ?? SESSION;

    tab.showSession(id);
    await settle();

    expect(tab.element.querySelectorAll(".web-entry-fetch")).toHaveLength(1);
    expect(tab.element.querySelectorAll(".web-entry-after").length).toBeGreaterThan(2);
    expect(texts(tab, ".web-outcome").some((text) => text.startsWith("refused"))).toBe(true);
    expect(texts(tab, ".web-target").some((text) => text.startsWith("curl "))).toBe(true);
    expect(texts(tab, ".web-target").some((text) => text.includes("withheld"))).toBe(true);
  });
});
