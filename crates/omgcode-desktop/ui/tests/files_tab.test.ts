import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import { IpcFailure } from "../src/ipc/errors";
import type { WorkspaceEntry, WorkspaceListing } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import {
  createFilesTab,
  FILES_EMPTY,
  FILES_MAX_ROWS,
  FILES_NO_SESSION,
} from "../src/main/files_tab";
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

function dir(name: string): WorkspaceEntry {
  return { name, kind: "directory", lossy: false, openable: true };
}
function file(name: string): WorkspaceEntry {
  return { name, kind: "file", lossy: false, openable: false };
}
function listing(entries: WorkspaceEntry[], truncated = false): WorkspaceListing {
  return { entries, truncated };
}

/** A fake workspace keyed by the path's components joined with `/`. */
function fakeTree(directories: Record<string, WorkspaceListing>) {
  return (args: Record<string, unknown>): unknown => {
    const key = (args.path as string[]).join("/");
    const found = directories[key];
    if (found === undefined) {
      throw new IpcFailure({ code: "invalid_request", message: "The folder could not be listed." });
    }
    return found;
  };
}

const TREE = {
  "": listing([dir("src"), dir("empty"), file("Cargo.toml")]),
  src: listing([dir("main"), file("lib.rs")]),
  "src/main": listing([file("app.ts")]),
  empty: listing([]),
};

let ipc: FakeIpc;

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  setInvoker(null);
  vi.useRealTimers();
  document.body.replaceChildren();
});

function mount(handler: (args: Record<string, unknown>) => unknown = fakeTree(TREE)) {
  ipc = installFakeIpc({ workspace_entries: handler });
  const tab = createFilesTab();
  document.body.replaceChildren(tab.content);
  return tab;
}

function names(scope: ParentNode = document): string[] {
  return Array.from(scope.querySelectorAll(".files-name")).map((n) => n.textContent ?? "");
}

function dirButton(name: string): HTMLButtonElement {
  const found = Array.from(document.querySelectorAll<HTMLButtonElement>("button.files-dir")).find(
    (b) => b.querySelector(".files-name")?.textContent === name,
  );
  if (found === undefined) {
    throw new Error(`No directory ${name}`);
  }
  return found;
}

describe("the Files tab", () => {
  it("asks to open a session and lists nothing until one is open and the tab is visible", async () => {
    const tab = mount();
    expect(document.querySelector(".files-message")?.textContent).toBe(FILES_NO_SESSION);
    tab.setSession("s-1");
    tab.onVisible?.(false);
    await settle();
    expect(ipc.callsTo("workspace_entries")).toHaveLength(0);
    tab.onVisible?.(true);
    await settle();
    expect(ipc.callsTo("workspace_entries")).toEqual([
      { command: "workspace_entries", args: { session_id: "s-1", path: [] } },
    ]);
    expect(names()).toEqual(["src", "empty", "Cargo.toml"]);
    expect((document.querySelector(".files-message") as HTMLElement).hidden).toBe(true);
  });

  it("opens a directory lazily, with a chevron state, and closes it again", async () => {
    const tab = mount();
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    const src = dirButton("src");
    expect(src.getAttribute("aria-expanded")).toBe("false");
    expect(ipc.callsTo("workspace_entries")).toHaveLength(1);
    src.click();
    await settle();
    expect(src.getAttribute("aria-expanded")).toBe("true");
    expect(ipc.callsTo("workspace_entries")[1]?.args).toEqual({ session_id: "s-1", path: ["src"] });
    expect(names(src.parentElement as HTMLElement)).toEqual(["src", "main", "lib.rs"]);
    dirButton("main").click();
    await settle();
    expect(ipc.callsTo("workspace_entries")[2]?.args).toEqual({
      session_id: "s-1",
      path: ["src", "main"],
    });
    expect(names()).toContain("app.ts");
    src.click();
    expect(src.getAttribute("aria-expanded")).toBe("false");
    expect((src.nextElementSibling as HTMLElement).hidden).toBe(true);
    src.click();
    await settle();
    // Opening it again lists it again, so it shows what is on disk now.
    expect(ipc.callsTo("workspace_entries").length).toBeGreaterThanOrEqual(4);
  });

  it("says that an empty directory is empty and shows an error as text", async () => {
    const tab = mount();
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    dirButton("empty").click();
    await settle();
    expect(document.body.textContent).toContain(FILES_EMPTY);

    ipc.on("workspace_entries", () => {
      throw new IpcFailure({ code: "busy", message: "The data is in use." });
    });
    tab.refresh();
    await settle();
    const error = document.querySelector(".files-error");
    expect(error?.textContent).toBe("The folder could not be listed. The data is in use.");
  });

  it("refreshes the root and keeps the open directories open", async () => {
    const tab = mount();
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    dirButton("src").click();
    await settle();
    const before = ipc.callsTo("workspace_entries").length;
    (document.querySelector('[aria-label="Refresh files"]') as HTMLButtonElement).click();
    await settle();
    expect(ipc.callsTo("workspace_entries").length).toBe(before + 2);
    expect(dirButton("src").getAttribute("aria-expanded")).toBe("true");
    expect(names()).toEqual(["src", "main", "lib.rs", "empty", "Cargo.toml"]);
  });

  it("reads nothing on a refresh while hidden and lists again when shown", async () => {
    const tab = mount();
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    tab.onVisible?.(false);
    const before = ipc.callsTo("workspace_entries").length;
    tab.refresh();
    await settle();
    expect(ipc.callsTo("workspace_entries").length).toBe(before);
    tab.onVisible?.(true);
    await settle();
    expect(ipc.callsTo("workspace_entries").length).toBe(before + 1);
  });

  it("clears on a session change and drops a reply that arrives for the old session", async () => {
    let release: (value: WorkspaceListing) => void = () => undefined;
    const tab = mount((args) =>
      args.session_id === "s-1"
        ? new Promise<WorkspaceListing>((resolve) => {
            release = resolve;
          })
        : listing([file("other.txt")]),
    );
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    tab.setSession("s-2");
    await settle();
    expect(names()).toEqual(["other.txt"]);
    release(listing([file("stale.txt")]));
    await settle();
    expect(names()).toEqual(["other.txt"]);
    tab.setSession(null);
    expect(names()).toEqual([]);
    expect(document.querySelector(".files-message")?.textContent).toBe(FILES_NO_SESSION);
    expect(
      (document.querySelector('[aria-label="Refresh files"]') as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("renders hostile names as text and never sends a lossy name back", async () => {
    const hostile = "<img src=x onerror=alert(1)>";
    const replacement = String.fromCharCode(0xfffd);
    const lossyName = `caf${replacement}`;
    const tab = mount(() =>
      listing([
        dir(hostile),
        // A lossy name stays closed even if the flag were set.
        { name: lossyName, kind: "directory", lossy: true, openable: true },
        { name: "link", kind: "other", lossy: false, openable: false },
      ]),
    );
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    expect(document.querySelector("img")).toBeNull();
    expect(names()).toEqual([hostile, lossyName, "link"]);
    const lossy = Array.from(document.querySelectorAll(".files-dir")).find((n) =>
      n.textContent?.includes("caf"),
    ) as HTMLElement;
    expect(lossy.tagName).toBe("DIV");
    lossy.click();
    await settle();
    expect(ipc.callsTo("workspace_entries")).toHaveLength(1);
  });

  it("shows a directory Rust would refuse as a closed row and never lists it", async () => {
    const tab = mount(() =>
      listing([{ name: "src\\generated", kind: "directory", lossy: false, openable: false }]),
    );
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    expect(document.querySelector("button.files-dir")).toBeNull();
    const row = document.querySelector(".files-dir") as HTMLElement;
    expect(row.tagName).toBe("DIV");
    expect(row.title).toBe(
      "This folder's name or depth cannot be used as a path here, so it is not opened.",
    );
    row.click();
    await settle();
    expect(ipc.callsTo("workspace_entries")).toHaveLength(1);
  });

  it("notes a truncated directory and bounds the rows it draws", async () => {
    const tab = mount(() => listing([file("a"), file("b")], true));
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    expect(document.querySelector(".files-note")?.textContent).toBe(
      "Only the first 2 entries are shown.",
    );
    const many = Array.from({ length: FILES_MAX_ROWS + 10 }, (_, i) => file(`f${i}`));
    ipc.on("workspace_entries", () => listing(many));
    tab.refresh();
    await settle();
    expect(document.querySelectorAll("li")).toHaveLength(FILES_MAX_ROWS);
    // The note counts the rows drawn, not the entries the broker returned.
    expect(document.querySelector(".files-note")?.textContent).toBe(
      `Only the first ${FILES_MAX_ROWS} entries are shown.`,
    );
  });

  it("keeps no rows in a closed folder, so another folder draws its full share", async () => {
    const big = Array.from({ length: FILES_MAX_ROWS }, (_, i) => file(`b${i}`));
    const tab = mount(
      fakeTree({
        "": listing([dir("big"), dir("small")]),
        big: listing(big),
        small: listing([file("one"), file("two"), file("three")]),
      }),
    );
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    dirButton("big").click();
    await settle();
    expect(document.querySelectorAll("li")).toHaveLength(FILES_MAX_ROWS);
    dirButton("big").click();
    expect(document.querySelectorAll("li")).toHaveLength(2);
    dirButton("small").click();
    await settle();
    const small = dirButton("small").parentElement as HTMLElement;
    expect(names(small)).toEqual(["small", "one", "two", "three"]);
    expect(small.querySelector(".files-note")).toBeNull();
  });

  it("draws nothing from a listing that lands after its folder was closed", async () => {
    let release: (value: WorkspaceListing) => void = () => undefined;
    const tab = mount((args) =>
      (args.path as string[]).length === 0
        ? listing([dir("slow")])
        : new Promise<WorkspaceListing>((resolve) => {
            release = resolve;
          }),
    );
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    dirButton("slow").click();
    await settle();
    dirButton("slow").click();
    release(listing([file("late.txt")]));
    await settle();
    expect(names()).toEqual(["slow"]);
    expect(document.querySelectorAll("li")).toHaveLength(1);
  });

  it("adds no note when a listing that fills the row cap was drawn in full", async () => {
    const exact = Array.from({ length: FILES_MAX_ROWS }, (_, i) => file(`f${i}`));
    const tab = mount(() => listing(exact));
    tab.setSession("s-1");
    tab.onVisible?.(true);
    await settle();
    expect(document.querySelectorAll("li")).toHaveLength(FILES_MAX_ROWS);
    expect(document.querySelector(".files-note")).toBeNull();
  });
});

describe("the Files tab in the main window", () => {
  let app: MainApp | null = null;
  afterEach(() => {
    app?.dispose();
    app = null;
  });

  const s = session({ session_id: "s-one", title: "One" });

  it("opens from the rail, follows the open session, and lists only after the tab is shown", async () => {
    ipc = installFakeIpc(
      mainHandlers({
        session_tree: () => tree([{ id: "g1", folder: "C:\\work\\one", sessions: [s] }]),
        session_transcript: () => page(s, [userMessage(1, "hi")]),
        workspace_entries: fakeTree(TREE),
      }),
    );
    const root = mountRoot();
    app = startMain(root);
    await settle();
    (root.querySelector('[data-session-id="s-one"]') as HTMLElement).click();
    await settle();
    // Collapsed on the Terminal tab: nothing was listed.
    expect(ipc.callsTo("workspace_entries")).toHaveLength(0);
    (root.querySelector('button[aria-label="Show files panel"]') as HTMLButtonElement).click();
    await settle();
    const shell = root.querySelector(".shell") as HTMLElement;
    expect(shell.classList.contains("right-collapsed")).toBe(false);
    expect(root.querySelector('[role="tab"][aria-selected="true"]')?.textContent).toBe("Files");
    expect(ipc.callsTo("workspace_entries")[0]?.args).toEqual({ session_id: "s-one", path: [] });
    expect(names(root)).toEqual(["src", "empty", "Cargo.toml"]);
  });
});
