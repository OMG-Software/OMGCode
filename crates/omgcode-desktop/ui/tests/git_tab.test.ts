import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { GitStatusView } from "../src/ipc/types";
import { createGitTab, groupPaths } from "../src/main/git_tab";
import { createMockBackend } from "../src/mock/backend";
import { buttonByText, installFakeIpc, rejection, settle } from "./helpers";

const BRANCH = { state: "branch", name: "main", note: null } as const;

function status(overrides: Partial<GitStatusView> = {}): GitStatusView {
  return { branch: BRANCH, state: "ready", note: null, paths: [], truncated: false, ...overrides };
}

function rows(root: HTMLElement): string[] {
  return Array.from(root.querySelectorAll(".git-row")).map((row) => row.textContent ?? "");
}

beforeEach(() => {
  vi.useFakeTimers();
  document.body.replaceChildren();
});

afterEach(() => {
  vi.useRealTimers();
  setInvoker(null);
});

describe("the Git status tab", () => {
  it("asks for no session until one is shown, then reads the status for its id", async () => {
    const ipc = installFakeIpc({ workspace_git_status: () => status() });
    const tab = createGitTab();
    expect(tab.element.textContent).toContain("No session open.");
    expect(ipc.callsTo("workspace_git_status")).toHaveLength(0);

    tab.showSession("s-1");
    await settle();
    expect(ipc.callsTo("workspace_git_status")[0]?.args).toEqual({ session_id: "s-1" });
    expect(tab.element.textContent).toContain("Branch: main");
    expect(tab.element.textContent).toContain("No changes.");
    tab.dispose();
  });

  it("groups paths as Staged, Changed, Untracked, and Conflicted with letters and tooltips", async () => {
    installFakeIpc({
      workspace_git_status: () =>
        status({
          paths: [
            { path: "a.rs", index: "added", worktree: null },
            { path: "b.rs", index: "modified", worktree: "modified" },
            { path: "c.rs", index: null, worktree: "deleted" },
            { path: "d/", index: null, worktree: "untracked" },
            { path: "e.rs", index: null, worktree: "conflicted" },
          ],
        }),
    });
    const tab = createGitTab();
    tab.showSession("s-1");
    await settle();

    const titles = Array.from(tab.element.querySelectorAll(".git-group-title")).map(
      (node) => node.textContent,
    );
    expect(titles).toEqual(["Staged (2)", "Changed (2)", "Untracked (1)", "Conflicted (1)"]);
    expect(rows(tab.element)).toEqual(["Aa.rs", "Mb.rs", "Mb.rs", "Dc.rs", "?d/", "Ue.rs"]);
    expect(tab.element.querySelector(".git-state")?.getAttribute("title")).toBe("Staged: added");
    expect(groupPaths([])).toHaveLength(0);
    tab.dispose();
  });

  it("shows names as text, never as markup", async () => {
    installFakeIpc({
      workspace_git_status: () =>
        status({
          paths: [
            { path: '<img src=x onerror="alert(1)">.txt', index: null, worktree: "untracked" },
          ],
        }),
    });
    const tab = createGitTab();
    tab.showSession("s-1");
    await settle();
    expect(tab.element.querySelector("img")).toBeNull();
    expect(rows(tab.element)[0]).toContain("<img src=x");
    // The tooltip is fixed text, never the path.
    expect(tab.element.querySelector(".git-state")?.getAttribute("title")).toBe(
      "Untracked: not in the repository",
    );
    tab.dispose();
  });

  it("says when the folder is not a repository, and shows errors as text", async () => {
    const ipc = installFakeIpc({
      workspace_git_status: () => status({ state: "not_repository" }),
    });
    const tab = createGitTab();
    tab.showSession("s-1");
    await settle();
    expect(tab.element.textContent).toContain("Not a Git repository.");

    ipc.on("workspace_git_status", () =>
      rejection("unknown_workspace", "The workspace is not registered."),
    );
    buttonByText(tab.element, "Refresh").click();
    await settle();
    expect(tab.element.textContent).toContain(
      "Git status unavailable: The workspace is not registered.",
    );
    expect(tab.element.querySelector("[role=alert]")).not.toBeNull();

    ipc.on("workspace_git_status", () =>
      status({ state: "unavailable", note: "the repository cannot be read" }),
    );
    buttonByText(tab.element, "Refresh").click();
    await settle();
    expect(tab.element.textContent).toContain(
      "Git status unavailable: the repository cannot be read",
    );
    tab.dispose();
  });

  it("notes a truncated list", async () => {
    installFakeIpc({
      workspace_git_status: () =>
        status({
          paths: [{ path: "a", index: null, worktree: "untracked" }],
          truncated: true,
        }),
    });
    const tab = createGitTab();
    tab.showSession("s-1");
    await settle();
    expect(tab.element.textContent).toContain("Showing the first 1 changed paths; more changed.");
    tab.dispose();
  });

  it("clears on showSession(null) and reloads for a new session", async () => {
    const ipc = installFakeIpc({
      workspace_git_status: (args) =>
        status({
          paths: [{ path: `${String(args.session_id)}.rs`, index: null, worktree: "modified" }],
        }),
    });
    const tab = createGitTab();
    tab.showSession("s-1");
    await settle();
    expect(rows(tab.element)).toEqual(["Ms-1.rs"]);

    tab.showSession(null);
    await settle();
    expect(tab.element.querySelector(".git-row")).toBeNull();
    expect(tab.element.textContent).toContain("No session open.");
    expect(ipc.callsTo("workspace_git_status")).toHaveLength(1);

    tab.showSession("s-2");
    await settle();
    expect(rows(tab.element)).toEqual(["Ms-2.rs"]);
    tab.dispose();
  });

  it("drops a stale answer from an earlier session", async () => {
    const pending: Array<(value: GitStatusView) => void> = [];
    installFakeIpc({
      workspace_git_status: () => new Promise<GitStatusView>((resolve) => pending.push(resolve)),
    });
    const tab = createGitTab();
    tab.showSession("s-1");
    tab.showSession("s-2");
    await settle();
    pending[1]?.(status({ paths: [{ path: "new.rs", index: null, worktree: "modified" }] }));
    await settle();
    pending[0]?.(status({ paths: [{ path: "old.rs", index: null, worktree: "modified" }] }));
    await settle();
    expect(rows(tab.element)).toEqual(["Mnew.rs"]);
    tab.dispose();
  });

  it("reads again on refresh, but not before a session is shown", async () => {
    const ipc = installFakeIpc({ workspace_git_status: () => status() });
    const tab = createGitTab();
    tab.refresh();
    await settle();
    expect(ipc.callsTo("workspace_git_status")).toHaveLength(0);

    tab.showSession("s-1");
    await settle();
    tab.refresh();
    await settle();
    expect(ipc.callsTo("workspace_git_status")).toHaveLength(2);
    tab.dispose();
  });

  it("renders the mock backend's fake status", async () => {
    const { invoke } = createMockBackend();
    setInvoker(invoke);
    const tree = (await invoke("session_tree", {
      view: "active",
      sort: "updated",
      descending: true,
    })) as { groups: Array<{ sessions: Array<{ session_id: string }> }> };
    const id = tree.groups[0]?.sessions[0]?.session_id ?? "";
    const tab = createGitTab();
    tab.showSession(id);
    await settle();
    expect(tab.element.textContent).toContain("Branch: main");
    expect(tab.element.querySelector(".git-group-title")?.textContent).toBe("Staged (2)");
    expect(tab.element.querySelector("b")).toBeNull();
    tab.dispose();
  });
});
