import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { SessionSummary } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import {
  CENTER_MIN_WIDTH,
  COLLAPSE_SNAP,
  createShellLayout,
  KEY_STEP,
  LARGE_KEY_STEP,
  LEFT_PANEL,
  RAIL_WIDTH,
  RIGHT_PANEL,
  SEPARATOR_WIDTH,
} from "../src/main/layout";
import { TERMINAL_UNAVAILABLE } from "../src/main/terminal_panel";
import {
  type FakeIpc,
  type Handler,
  installFakeIpc,
  mainHandlers,
  mountRoot,
  page,
  session,
  settle,
  tree,
  userMessage,
} from "./helpers";

const s = session({ session_id: "s-one", title: "One" });

let app: MainApp | null = null;
let root: HTMLElement;
let ipc: FakeIpc;

function treeOf(...sessions: SessionSummary[]): Handler {
  return () => tree([{ id: "g1", folder: "C:\\work\\one", sessions }]);
}

async function start(handlers: Record<string, Handler> = {}): Promise<void> {
  ipc = installFakeIpc(
    mainHandlers({
      session_tree: treeOf(s),
      session_transcript: () => page(s, [userMessage(1, "hi")]),
      ...handlers,
    }),
  );
  root = mountRoot();
  app = startMain(root);
  await settle();
}

function shell(): HTMLElement {
  return root.querySelector(".shell") as HTMLElement;
}

function byLabel(label: string): HTMLButtonElement {
  const found = root.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (found === null) {
    throw new Error(`No button labelled "${label}"`);
  }
  return found;
}

function separator(side: "left" | "right"): HTMLElement {
  return root.querySelector(`.separator-${side}`) as HTMLElement;
}

function width(side: "left" | "right"): string {
  return shell().style.getPropertyValue(`--${side}-width`);
}

function key(target: HTMLElement, name: string, shiftKey = false): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: name, shiftKey, bubbles: true }));
}

function pointer(target: EventTarget, type: string, clientX: number): void {
  target.dispatchEvent(new PointerEvent(type, { clientX, button: 0, bubbles: true }));
}

/** Gives the shell a laid-out width, which jsdom does not compute. */
function shellWidth(value: number): void {
  Object.defineProperty(shell(), "clientWidth", { configurable: true, value });
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  app?.dispose();
  app = null;
  setInvoker(null);
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("the main window's columns", () => {
  it("places sessions, a separator, the conversation, a separator, and the terminal", async () => {
    await start();
    const children = Array.from(shell().children).map((child) => child.className);
    expect(children).toEqual([
      "sidebar",
      "separator separator-left",
      "pane",
      "separator separator-right",
      "terminal-panel",
    ]);
    expect(width("left")).toBe(`${LEFT_PANEL.initial}px`);
    expect(shell().classList.contains("left-collapsed")).toBe(false);
    // The terminal panel holds no terminal yet, so it opens collapsed.
    expect(shell().classList.contains("right-collapsed")).toBe(true);
  });

  it("describes each separator as a window splitter for its panel", async () => {
    await start();
    const left = separator("left");
    expect(left.getAttribute("role")).toBe("separator");
    expect(left.getAttribute("aria-orientation")).toBe("vertical");
    expect(left.tabIndex).toBe(0);
    expect(left.getAttribute("aria-controls")).toBe("sessions-panel");
    expect(root.querySelector("#sessions-panel")?.classList.contains("sidebar")).toBe(true);
    expect(left.getAttribute("aria-valuenow")).toBe(String(LEFT_PANEL.initial));
    expect(left.getAttribute("aria-valuemin")).toBe(String(LEFT_PANEL.min));
    expect(left.getAttribute("aria-valuemax")).toBe(String(LEFT_PANEL.max));
    const right = separator("right");
    expect(right.getAttribute("aria-controls")).toBe("terminal-panel");
    expect(right.getAttribute("aria-valuetext")).toBe("Collapsed");
  });
});

describe("the sessions panel", () => {
  it("collapses to a rail of icons and moves focus to its expand button", async () => {
    await start();
    const collapse = byLabel("Collapse sessions panel");
    collapse.focus();
    collapse.click();
    expect(shell().classList.contains("left-collapsed")).toBe(true);
    expect((root.querySelector(".sidebar-full") as HTMLElement).hidden).toBe(true);
    const rail = root.querySelector(".sidebar-rail") as HTMLElement;
    expect(rail.hidden).toBe(false);
    expect(document.activeElement).toBe(byLabel("Expand sessions panel"));
    const labels = Array.from(rail.querySelectorAll("button")).map((b) =>
      b.getAttribute("aria-label"),
    );
    expect(labels).toEqual([
      "Expand sessions panel",
      "New session",
      "Search sessions",
      "Sessions",
      "Approvals",
      "Settings",
    ]);
    // Every rail button shows an icon (and the approvals badge), and names itself in a tooltip.
    for (const item of Array.from(rail.querySelectorAll("button"))) {
      expect(item.firstElementChild?.tagName.toLowerCase()).toBe("svg");
      expect(item.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
      expect(item.title).toBe(item.getAttribute("aria-label"));
      const visibleText = Array.from(item.querySelectorAll<HTMLElement>("span:not([hidden])"));
      expect(visibleText).toEqual([]);
    }
  });

  it("expands again and keeps its width", async () => {
    await start();
    key(separator("left"), "ArrowRight");
    byLabel("Collapse sessions panel").click();
    byLabel("Expand sessions panel").click();
    expect(shell().classList.contains("left-collapsed")).toBe(false);
    expect((root.querySelector(".sidebar-rail") as HTMLElement).hidden).toBe(true);
    expect(width("left")).toBe(`${LEFT_PANEL.initial + KEY_STEP}px`);
  });

  it("opens the search box from the rail's search icon", async () => {
    await start();
    byLabel("Collapse sessions panel").click();
    byLabel("Search sessions").click();
    expect(shell().classList.contains("left-collapsed")).toBe(false);
    expect(document.activeElement).toBe(root.querySelector(".search"));
  });

  it("focuses the open session from the rail's sessions icon", async () => {
    await start();
    (root.querySelector('[data-session-id="s-one"]') as HTMLElement).click();
    await settle();
    byLabel("Collapse sessions panel").click();
    byLabel("Sessions").click();
    const item = document.activeElement as HTMLElement;
    expect(item.dataset.sessionId).toBe("s-one");
  });

  it("starts a session, and opens the approvals and settings windows, from the rail", async () => {
    await start();
    byLabel("Collapse sessions panel").click();
    byLabel("New session").click();
    await settle();
    expect(ipc.callsTo("workspace_list")).toHaveLength(1);
    byLabel("Approvals").click();
    byLabel("Settings").click();
    await settle();
    expect(ipc.callsTo("approvals_show_window")).toHaveLength(1);
    expect(ipc.callsTo("settings_show_window")).toHaveLength(1);
    // Neither window opening changed the panel.
    expect(shell().classList.contains("left-collapsed")).toBe(true);
  });

  it("shows the waiting approvals on the rail", async () => {
    await start({ approvals_summary: () => ({ pending: 3 }) });
    const approvals = byLabel("Approvals, 3 waiting");
    const badge = approvals.querySelector(".rail-badge") as HTMLElement;
    expect(badge.hidden).toBe(false);
    expect(badge.textContent).toBe("3");
    ipc.on("approvals_summary", () => ({ pending: 0 }));
    await settle(3000);
    expect(approvals.getAttribute("aria-label")).toBe("Approvals");
    expect(badge.hidden).toBe(true);
  });
});

describe("the terminal panel", () => {
  it("says that no terminal exists and calls no command for one", async () => {
    await start();
    byLabel("Show terminal panel").click();
    expect(shell().classList.contains("right-collapsed")).toBe(false);
    const panel = root.querySelector(".terminal-panel") as HTMLElement;
    expect(panel.getAttribute("aria-label")).toBe("Terminal");
    expect(panel.querySelector(".terminal-empty")?.textContent).toBe(TERMINAL_UNAVAILABLE);
    expect(panel.querySelectorAll("input, textarea, [contenteditable]")).toHaveLength(0);
    await settle(3000);
    const commands = new Set(ipc.calls.map((call) => call.command));
    expect(Array.from(commands).filter((name) => /term|shell|pty|run/.test(name))).toEqual([]);
  });

  it("collapses again and moves focus to its rail", async () => {
    await start();
    byLabel("Show terminal panel").click();
    const collapse = byLabel("Collapse terminal panel");
    collapse.focus();
    collapse.click();
    expect(shell().classList.contains("right-collapsed")).toBe(true);
    expect(document.activeElement).toBe(byLabel("Show terminal panel"));
  });
});

describe("resizing with the keyboard", () => {
  it("moves the sessions separator by a step, or a large step with Shift", async () => {
    await start();
    const left = separator("left");
    key(left, "ArrowRight");
    expect(width("left")).toBe(`${LEFT_PANEL.initial + KEY_STEP}px`);
    key(left, "ArrowLeft", true);
    expect(width("left")).toBe(`${LEFT_PANEL.initial + KEY_STEP - LARGE_KEY_STEP}px`);
    expect(left.getAttribute("aria-valuenow")).toBe(
      String(LEFT_PANEL.initial + KEY_STEP - LARGE_KEY_STEP),
    );
  });

  it("stops at the limits, with Home and End going straight to them", async () => {
    await start();
    const left = separator("left");
    key(left, "End");
    expect(width("left")).toBe(`${LEFT_PANEL.max}px`);
    key(left, "ArrowRight");
    expect(width("left")).toBe(`${LEFT_PANEL.max}px`);
    key(left, "Home");
    expect(width("left")).toBe(`${LEFT_PANEL.min}px`);
    key(left, "ArrowLeft");
    expect(width("left")).toBe(`${LEFT_PANEL.min}px`);
    expect(shell().classList.contains("left-collapsed")).toBe(false);
  });

  it("grows the terminal panel with the left arrow, the way its separator moves", async () => {
    await start();
    const right = separator("right");
    key(right, "Enter");
    expect(shell().classList.contains("right-collapsed")).toBe(false);
    key(right, "ArrowLeft");
    expect(width("right")).toBe(`${RIGHT_PANEL.initial + KEY_STEP}px`);
    key(right, "ArrowRight");
    expect(width("right")).toBe(`${RIGHT_PANEL.initial}px`);
  });

  it("collapses and expands with Enter, and a growing arrow expands", async () => {
    await start();
    const left = separator("left");
    key(left, "Enter");
    expect(shell().classList.contains("left-collapsed")).toBe(true);
    expect(left.getAttribute("aria-valuetext")).toBe("Collapsed");
    key(left, "ArrowLeft");
    expect(shell().classList.contains("left-collapsed")).toBe(true);
    key(left, "ArrowRight");
    expect(shell().classList.contains("left-collapsed")).toBe(false);
    expect(width("left")).toBe(`${LEFT_PANEL.min}px`);
  });

  it("ignores other keys", async () => {
    await start();
    key(separator("left"), "a");
    expect(width("left")).toBe(`${LEFT_PANEL.initial}px`);
  });
});

describe("resizing with the pointer", () => {
  it("follows a drag, also after the pointer leaves the separator", async () => {
    await start();
    pointer(separator("left"), "pointerdown", 260);
    expect(shell().classList.contains("resizing")).toBe(true);
    pointer(document, "pointermove", 300);
    expect(width("left")).toBe(`${LEFT_PANEL.initial + 40}px`);
    pointer(document, "pointerup", 300);
    expect(shell().classList.contains("resizing")).toBe(false);
    pointer(document, "pointermove", 400);
    expect(width("left")).toBe(`${LEFT_PANEL.initial + 40}px`);
  });

  it("grows the terminal panel when its separator is dragged left", async () => {
    await start();
    byLabel("Show terminal panel").click();
    pointer(separator("right"), "pointerdown", 800);
    pointer(document, "pointermove", 700);
    pointer(document, "pointerup", 700);
    expect(width("right")).toBe(`${RIGHT_PANEL.initial + 100}px`);
  });

  it("collapses a panel dragged well below its minimum, and expands it when dragged back", async () => {
    await start();
    const start_x = LEFT_PANEL.initial;
    pointer(separator("left"), "pointerdown", start_x);
    pointer(document, "pointermove", LEFT_PANEL.min - COLLAPSE_SNAP + 10);
    expect(width("left")).toBe(`${LEFT_PANEL.min}px`);
    expect(shell().classList.contains("left-collapsed")).toBe(false);
    pointer(document, "pointermove", LEFT_PANEL.min - COLLAPSE_SNAP - 10);
    expect(shell().classList.contains("left-collapsed")).toBe(true);
    pointer(document, "pointerup", 0);

    pointer(separator("left"), "pointerdown", RAIL_WIDTH);
    pointer(document, "pointermove", 300);
    pointer(document, "pointerup", 300);
    expect(shell().classList.contains("left-collapsed")).toBe(false);
    expect(width("left")).toBe("300px");
  });

  it("ignores a button other than the primary one", async () => {
    await start();
    separator("left").dispatchEvent(
      new PointerEvent("pointerdown", { clientX: 260, button: 2, bubbles: true }),
    );
    pointer(document, "pointermove", 400);
    expect(width("left")).toBe(`${LEFT_PANEL.initial}px`);
  });

  it("toggles a panel on a double click", async () => {
    await start();
    separator("left").dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(shell().classList.contains("left-collapsed")).toBe(true);
  });
});

describe("keeping room for the conversation", () => {
  /** The conversation's width: the shell less both panel tracks and both separators. */
  function centerWidth(shellPx: number): number {
    const track = (side: "left" | "right") =>
      shell().classList.contains(`${side}-collapsed`)
        ? RAIL_WIDTH
        : Number.parseInt(width(side), 10);
    return shellPx - track("left") - track("right") - 2 * SEPARATOR_WIDTH;
  }

  it("takes room from the other panel when a panel expands", async () => {
    await start();
    shellWidth(900);
    // With the terminal collapsed, the sessions panel can reach its own maximum.
    key(separator("left"), "End");
    expect(width("left")).toBe(`${LEFT_PANEL.max}px`);
    byLabel("Show terminal panel").click();
    expect(width("right")).toBe(`${RIGHT_PANEL.min}px`);
    expect(width("left")).toBe(
      `${900 - CENTER_MIN_WIDTH - RIGHT_PANEL.min - 2 * SEPARATOR_WIDTH}px`,
    );
    expect(centerWidth(900)).toBe(CENTER_MIN_WIDTH);
    expect(separator("left").getAttribute("aria-valuenow")).toBe(
      String(900 - CENTER_MIN_WIDTH - RIGHT_PANEL.min - 2 * SEPARATOR_WIDTH),
    );
  });

  it("takes room from the other panel when a panel expands by keyboard or drag", async () => {
    await start();
    shellWidth(900);
    byLabel("Show terminal panel").click();
    key(separator("right"), "End");
    byLabel("Collapse sessions panel").click();
    key(separator("right"), "End");
    expect(centerWidth(900)).toBe(CENTER_MIN_WIDTH);
    // Expanding the sessions panel by key or by drag also leaves the conversation its minimum.
    key(separator("left"), "Enter");
    expect(centerWidth(900)).toBe(CENTER_MIN_WIDTH);
    key(separator("left"), "Enter");
    pointer(separator("left"), "pointerdown", RAIL_WIDTH);
    pointer(document, "pointermove", 400);
    pointer(document, "pointerup", 400);
    expect(centerWidth(900)).toBeGreaterThanOrEqual(CENTER_MIN_WIDTH);
  });

  it("does not let a panel take the conversation's minimum width", async () => {
    await start();
    shellWidth(1000);
    byLabel("Show terminal panel").click();
    const leftMax = 1000 - CENTER_MIN_WIDTH - RIGHT_PANEL.initial - 2 * SEPARATOR_WIDTH;
    expect(separator("left").getAttribute("aria-valuemax")).toBe(String(leftMax));
    // End asks for the panel's own maximum, which the conversation's minimum cuts to leftMax.
    expect(leftMax).toBeLessThan(LEFT_PANEL.max);
    key(separator("left"), "End");
    expect(width("left")).toBe(`${leftMax}px`);
    key(separator("left"), "ArrowRight");
    expect(width("left")).toBe(`${leftMax}px`);
  });

  it("narrows the terminal panel first, then the sessions panel, when the window narrows", async () => {
    await start();
    byLabel("Show terminal panel").click();
    // The terminal panel gives up only what the conversation needs.
    shellWidth(900);
    window.dispatchEvent(new Event("resize"));
    const rightFit = 900 - CENTER_MIN_WIDTH - LEFT_PANEL.initial - 2 * SEPARATOR_WIDTH;
    expect(rightFit).toBeGreaterThan(RIGHT_PANEL.min);
    expect(width("right")).toBe(`${rightFit}px`);
    expect(width("left")).toBe(`${LEFT_PANEL.initial}px`);
    // Narrower still, the terminal panel reaches its minimum and the sessions panel gives way.
    shellWidth(820);
    window.dispatchEvent(new Event("resize"));
    expect(width("right")).toBe(`${RIGHT_PANEL.min}px`);
    expect(width("left")).toBe(
      `${820 - CENTER_MIN_WIDTH - RIGHT_PANEL.min - 2 * SEPARATOR_WIDTH}px`,
    );
  });

  it("fits again when the shell's own size changes", async () => {
    const callbacks: Array<() => void> = [];
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: () => void) {
          callbacks.push(callback);
        }
        observe(): void {}
        disconnect(): void {}
      },
    );
    const left = { element: document.createElement("nav"), setCollapsed: vi.fn() };
    const right = { element: document.createElement("aside"), setCollapsed: vi.fn() };
    left.element.id = "l";
    right.element.id = "r";
    const layout = createShellLayout({ left, center: document.createElement("main"), right });
    layout.right.setCollapsed(false);
    Object.defineProperty(layout.element, "clientWidth", { configurable: true, value: 800 });
    expect(callbacks).toHaveLength(1);
    callbacks[0]?.();
    expect(layout.right.width()).toBe(RIGHT_PANEL.min);
    expect(layout.left.width()).toBe(LEFT_PANEL.min);
    expect(right.setCollapsed).toHaveBeenLastCalledWith(false);
    layout.dispose();
  });

  it("stops following the window and any drag once disposed", async () => {
    const left = { element: document.createElement("nav"), setCollapsed: vi.fn() };
    const right = { element: document.createElement("aside"), setCollapsed: vi.fn() };
    const layout = createShellLayout({ left, center: document.createElement("main"), right });
    const leftSeparator = layout.left.separator;
    pointer(leftSeparator, "pointerdown", 260);
    layout.dispose();
    expect(layout.element.classList.contains("resizing")).toBe(false);
    pointer(document, "pointermove", 400);
    expect(layout.left.width()).toBe(LEFT_PANEL.initial);
  });
});
