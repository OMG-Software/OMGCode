import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { el } from "../src/dom";
import { setInvoker } from "../src/ipc/client";
import { type MainApp, startMain } from "../src/main/app";
import { type PanelView, panelTab } from "../src/main/panel_tab";
import {
  type FakeIpc,
  installFakeIpc,
  mainHandlers,
  mountRoot,
  page,
  session,
  settle,
  tree,
  userMessage,
} from "./helpers";

function fakeView() {
  const view = {
    element: el("div", { text: "view" }),
    showSession: vi.fn<(sessionId: string | null) => void>(),
    refresh: vi.fn<() => void>(),
    dispose: vi.fn<() => void>(),
  } satisfies PanelView;
  return view;
}

describe("a session view as a side panel tab", () => {
  it("reads nothing while hidden and shows the open session when it becomes visible", () => {
    const view = fakeView();
    const tab = panelTab({ id: "git", label: "Git", icon: "gitStatus" }, view);
    expect(tab.content).toBe(view.element);
    tab.setSession("s-1");
    tab.refresh();
    expect(view.showSession).not.toHaveBeenCalled();
    expect(view.refresh).not.toHaveBeenCalled();
    tab.onVisible?.(true);
    expect(view.showSession).toHaveBeenLastCalledWith("s-1");
  });

  it("follows the open session while visible and ignores the same session again", () => {
    const view = fakeView();
    const tab = panelTab({ id: "git", label: "Git", icon: "gitStatus" }, view);
    tab.onVisible?.(true);
    expect(view.showSession).toHaveBeenLastCalledWith(null);
    tab.setSession("s-1");
    tab.setSession("s-1");
    tab.setSession("s-2");
    expect(view.showSession.mock.calls).toEqual([[null], ["s-1"], ["s-2"]]);
  });

  it("clears the view when hidden, so it polls and keeps nothing, and refreshes only while visible", () => {
    const view = fakeView();
    const tab = panelTab({ id: "processes", label: "Processes", icon: "processes" }, view);
    tab.setSession("s-1");
    tab.onVisible?.(true);
    tab.refresh();
    expect(view.refresh).toHaveBeenCalledTimes(1);
    tab.onVisible?.(false);
    expect(view.showSession).toHaveBeenLastCalledWith(null);
    tab.refresh();
    expect(view.refresh).toHaveBeenCalledTimes(1);
    // Shown again, it reads the open session afresh.
    tab.onVisible?.(true);
    expect(view.showSession).toHaveBeenLastCalledWith("s-1");
    tab.dispose();
    expect(view.dispose).toHaveBeenCalledTimes(1);
  });

  it("does not refresh a visible tab with no session open", () => {
    const view = fakeView();
    const tab = panelTab({ id: "context", label: "Context", icon: "context" }, view);
    tab.onVisible?.(true);
    tab.refresh();
    expect(view.refresh).not.toHaveBeenCalled();
  });
});

describe("the session tabs in the main window", () => {
  let app: MainApp | null = null;
  let ipc: FakeIpc;
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    app?.dispose();
    app = null;
    setInvoker(null);
    vi.useRealTimers();
    document.body.replaceChildren();
  });

  const s = session({ session_id: "s-one", title: "One" });

  function handlers() {
    return mainHandlers({
      session_tree: () => tree([{ id: "g1", folder: "C:\\work\\one", sessions: [s] }]),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      workspace_entries: () => ({ entries: [], truncated: false }),
      workspace_git_status: () => ({
        branch: { state: "branch", name: "main", note: null },
        state: "ready",
        note: null,
        paths: [],
        truncated: false,
      }),
      session_processes: () => ({ entries: [], truncated: false, run_details: "available" }),
      session_web_audit: () => ({ entries: [], truncated: false }),
      session_context_breakdown: (args) => ({
        session_id: args.session_id as string,
        parts: [],
        estimated_input_tokens: 0,
        max_output_tokens: 1024,
        effective_trigger: 4096,
        context_tokens: null,
        auto_compaction: true,
        estimate_scope: "pre_turn_conservative",
      }),
    });
  }

  function railButton(root: HTMLElement, label: string): HTMLButtonElement {
    return root.querySelector(`button[aria-label="${label}"]`) as HTMLButtonElement;
  }

  it("offers Terminal, Files, Git, Processes, Web, and Context, each with a rail icon", async () => {
    ipc = installFakeIpc(handlers());
    const root = mountRoot();
    app = startMain(root);
    await settle();
    const labels = Array.from(root.querySelectorAll(".side-tabs [role='tab']")).map(
      (tab) => tab.textContent,
    );
    expect(labels).toEqual(["Terminal", "Files", "Git", "Processes", "Web", "Context"]);
    for (const name of ["git", "processes", "web", "context"]) {
      expect(railButton(root, `Show ${name} panel`)).not.toBeNull();
    }
  });

  it("reads a session view only once its tab is shown, and only the shown one", async () => {
    ipc = installFakeIpc(handlers());
    const root = mountRoot();
    app = startMain(root);
    await settle();
    (root.querySelector('[data-session-id="s-one"]') as HTMLElement).click();
    await settle();
    for (const command of [
      "workspace_git_status",
      "session_processes",
      "session_web_audit",
      "session_context_breakdown",
    ]) {
      expect(ipc.callsTo(command)).toHaveLength(0);
    }

    railButton(root, "Show git panel").click();
    await settle();
    expect(root.querySelector('[role="tab"][aria-selected="true"]')?.textContent).toBe("Git");
    expect(ipc.callsTo("workspace_git_status").map((call) => call.args)).toEqual([
      { session_id: "s-one" },
    ]);
    expect(ipc.callsTo("session_processes")).toHaveLength(0);
    expect(ipc.callsTo("session_web_audit")).toHaveLength(0);

    (root.querySelector('.side-tabs [role="tab"][data-tab="context"]') as HTMLElement).click();
    await settle();
    expect(ipc.callsTo("session_context_breakdown").map((call) => call.args)).toEqual([
      { session_id: "s-one" },
    ]);
    // The Git tab is hidden now, so it read nothing more.
    expect(ipc.callsTo("workspace_git_status")).toHaveLength(1);
    expect(ipc.callsTo("session_processes")).toHaveLength(0);
  });
});
