import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setInvoker } from "../src/ipc/client";
import type { ContextBreakdown, ContextPart } from "../src/ipc/types";
import {
  CONTEXT_EMPTY,
  CONTEXT_LOADING,
  CONTEXT_SCOPE,
  createContextTab,
} from "../src/main/context_tab";
import { createMockBackend } from "../src/mock/backend";
import { icon } from "../src/ui/icons";
import { installFakeIpc, rejection } from "./helpers";

const SESSION = "11111111-1111-4111-8111-111111111111";
/** A right-to-left override, built at run time so no source file holds the raw character. */
const OVERRIDE = String.fromCharCode(0x202e);

function part(
  id: ContextPart["id"],
  tokens: number,
  extra: Partial<ContextPart> = {},
): ContextPart {
  return { id, tokens, included: true, ...extra };
}

/** A breakdown whose parts add up to its estimate, as the Rust command's do. */
function breakdown(overrides: Partial<ContextBreakdown> = {}): ContextBreakdown {
  const parts: ContextPart[] = overrides.parts ?? [
    part("system", 1000),
    part("global_instructions", 200),
    part("workspace_instructions", 0, { included: false, reason: "none_saved" }),
    part("tools", 800),
    part("summary", 0, { included: false, reason: "no_compaction" }),
    part("history", 1000, { messages: 3 }),
    part("skills", 0, { included: false, reason: "selected_per_turn" }),
    part("framing", 320),
  ];
  return {
    session_id: SESSION,
    parts,
    estimated_input_tokens: parts.reduce((total, one) => total + one.tokens, 0),
    max_output_tokens: 1024,
    effective_trigger: 6000,
    context_tokens: 10_000,
    auto_compaction: true,
    estimate_scope: "pre_turn_conservative",
    ...overrides,
  };
}

async function settle(): Promise<void> {
  for (let i = 0; i < 6; i += 1) {
    await Promise.resolve();
  }
}

function textOf(element: Element): string {
  return element.textContent ?? "";
}

function rowOf(root: Element, id: string): Element {
  const found = root.querySelector(`tr[data-part="${id}"]`);
  if (found === null) {
    throw new Error(`no row for ${id}`);
  }
  return found;
}

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void } {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

beforeEach(() => {
  document.body.replaceChildren();
});

afterEach(() => {
  setInvoker(null);
  document.body.replaceChildren();
});

describe("context tab", () => {
  it("shows an empty state and calls nothing while no session is open", async () => {
    const ipc = installFakeIpc();
    const tab = createContextTab();
    document.body.append(tab.element);
    expect(textOf(tab.element)).toContain(CONTEXT_EMPTY);
    tab.showSession(null);
    tab.refresh();
    await settle();
    expect(ipc.calls).toHaveLength(0);
    expect(tab.element.querySelector<HTMLButtonElement>(".ctx-refresh")?.disabled).toBe(true);
    expect(tab.element.getAttribute("aria-label")).toBe("Context breakdown");
  });

  it("asks for the session and shows a loading line until the answer arrives", async () => {
    const gate = deferred<ContextBreakdown>();
    const ipc = installFakeIpc({ session_context_breakdown: () => gate.promise });
    const tab = createContextTab();
    tab.showSession(SESSION);
    await settle();
    expect(ipc.callsTo("session_context_breakdown")).toEqual([
      { command: "session_context_breakdown", args: { session_id: SESSION } },
    ]);
    expect(textOf(tab.element)).toContain(CONTEXT_LOADING);
    expect(tab.element.querySelector(".ctx-body")?.getAttribute("aria-busy")).toBe("true");
    gate.resolve(breakdown());
    await settle();
    expect(tab.element.querySelector(".ctx-body")?.hasAttribute("aria-busy")).toBe(false);
    expect(tab.element.querySelector(".ctx-report")).not.toBeNull();
  });

  it("draws a bar segment per included part and marks the trigger", async () => {
    installFakeIpc({ session_context_breakdown: () => breakdown() });
    const tab = createContextTab();
    tab.showSession(SESSION);
    await settle();
    const segments = Array.from(tab.element.querySelectorAll(".ctx-seg"));
    // System, global instructions, tools, history, and framing; not-included parts have none.
    expect(segments).toHaveLength(5);
    // The scale is the 10,000-token capacity, so the 1,000-token system part is 100 wide of 1000.
    expect(segments[0]?.getAttribute("x")).toBe("0");
    expect(segments[0]?.getAttribute("width")).toBe("100");
    expect(segments[1]?.getAttribute("x")).toBe("100");
    const marker = tab.element.querySelector(".ctx-trigger");
    expect(marker?.getAttribute("x1")).toBe("600");
    // Every segment names its part in a tooltip, and the image has a text alternative.
    expect(segments[0]?.querySelector("title")?.textContent).toBe("System prompt: 1,000 tokens");
    expect(tab.element.querySelector(".ctx-bar")?.getAttribute("role")).toBe("img");
    expect(tab.element.querySelector(".ctx-bar")?.getAttribute("aria-label")).toContain(
      "3,320 estimated input tokens",
    );
    // No inline style: the colours come from classes.
    expect(tab.element.querySelector("[style]")).toBeNull();
    expect(segments[0]?.classList.contains("ctx-fill-system")).toBe(true);
  });

  it("labels every part in text, with tokens, shares, and the reason it is left out", async () => {
    installFakeIpc({ session_context_breakdown: () => breakdown() });
    const tab = createContextTab();
    tab.showSession(SESSION);
    await settle();
    const root = tab.element;
    const cells = (id: string): string[] =>
      Array.from(rowOf(root, id).children).map((cell) => textOf(cell));
    expect(cells("system")).toEqual(["System prompt", "1,000", "30.1%", ""]);
    expect(cells("history")).toEqual([
      "Conversation history",
      "1,000",
      "30.1%",
      "3 messages after the compaction boundary",
    ]);
    expect(cells("workspace_instructions")).toEqual([
      "Workspace instructions",
      "0",
      "0%",
      "None saved",
    ]);
    expect(cells("summary")[3]).toBe("No compaction yet");
    expect(cells("skills")).toEqual([
      "Selected skills",
      "0",
      "0%",
      "Added only when selected for a turn",
    ]);
    expect(cells("total")).toEqual(["Estimated input", "3,320", "100%", ""]);
    // All eight parts and the total row, in the order Rust sent them.
    expect(
      Array.from(root.querySelectorAll("tbody tr")).map((r) => r.getAttribute("data-part")),
    ).toEqual([
      "system",
      "global_instructions",
      "workspace_instructions",
      "tools",
      "summary",
      "history",
      "skills",
      "framing",
      "total",
    ]);
  });

  it("names why tools are not offered", async () => {
    installFakeIpc({
      session_context_breakdown: () =>
        breakdown({
          parts: [
            part("system", 100),
            part("tools", 0, { included: false, reason: "workspace_unregistered" }),
          ],
        }),
    });
    const tab = createContextTab();
    tab.showSession(SESSION);
    await settle();
    expect(textOf(rowOf(tab.element, "tools"))).toContain("Workspace is not registered");
  });

  it("states the totals and the scope of the estimate plainly", async () => {
    installFakeIpc({ session_context_breakdown: () => breakdown() });
    const tab = createContextTab();
    tab.showSession(SESSION);
    await settle();
    const text = textOf(tab.element);
    expect(text).toContain(CONTEXT_SCOPE);
    expect(text).toContain("Conservative estimate before the next turn");
    expect(text).toContain("Context capacity10,000 tokens");
    expect(text).toContain("Output reserve1,024 tokens");
    expect(text).toContain("Compaction trigger6,000 tokens, auto-compaction on");
    expect(text).toContain("Room before the trigger2,680 tokens");
    expect(text).toContain("33.2% of the 10,000-token context");
  });

  it("reports an estimate over the trigger and an unknown capacity", async () => {
    installFakeIpc({
      session_context_breakdown: () =>
        breakdown({ context_tokens: null, effective_trigger: 2000, auto_compaction: false }),
    });
    const tab = createContextTab();
    tab.showSession(SESSION);
    await settle();
    const text = textOf(tab.element);
    expect(text).toContain("Context capacityUnknown (not saved in settings)");
    expect(text).toContain("auto-compaction off");
    expect(text).toContain("Over the trigger by1,320 tokens");
    expect(text).toContain("context capacity unknown");
    // The bar still scales to something drawable: the estimate itself, the largest value.
    expect(tab.element.querySelectorAll(".ctx-seg").length).toBeGreaterThan(0);
    // The trigger at 2,000 of a 3,320 scale is marked.
    expect(tab.element.querySelector(".ctx-trigger")?.getAttribute("x1")).toBe("602.41");
  });

  it("shows a failure as text, never as markup", async () => {
    installFakeIpc({
      session_context_breakdown: () =>
        rejection("busy", `<img src=x onerror=alert(1)> in use ${OVERRIDE}`),
    });
    const tab = createContextTab();
    document.body.append(tab.element);
    tab.showSession(SESSION);
    await settle();
    const alert = tab.element.querySelector("[role=alert]");
    expect(alert?.textContent).toContain("The context could not be estimated: <img src=x");
    expect(alert?.textContent).toContain("[U+202E]");
    expect(tab.element.querySelector("img")).toBeNull();
    expect(tab.element.querySelector(".ctx-report")).toBeNull();
  });

  it("reloads on refresh and on the Refresh button", async () => {
    let tokensNow = 100;
    const ipc = installFakeIpc({
      session_context_breakdown: () =>
        breakdown({ parts: [part("system", tokensNow)], effective_trigger: 6000 }),
    });
    const tab = createContextTab();
    tab.showSession(SESSION);
    await settle();
    expect(textOf(rowOf(tab.element, "system"))).toContain("100");
    tokensNow = 250;
    tab.refresh();
    await settle();
    expect(textOf(rowOf(tab.element, "system"))).toContain("250");
    tokensNow = 400;
    tab.element.querySelector<HTMLButtonElement>(".ctx-refresh")?.click();
    await settle();
    expect(textOf(rowOf(tab.element, "system"))).toContain("400");
    expect(ipc.callsTo("session_context_breakdown")).toHaveLength(3);
  });

  it("ignores a slow answer for a session that is no longer open", async () => {
    const first = deferred<ContextBreakdown>();
    const second = deferred<ContextBreakdown>();
    const queue = [first, second];
    installFakeIpc({
      session_context_breakdown: () => queue.shift()?.promise ?? breakdown(),
    });
    const tab = createContextTab();
    tab.showSession(SESSION);
    tab.showSession("22222222-2222-4222-8222-222222222222");
    second.resolve(breakdown({ parts: [part("system", 222)] }));
    await settle();
    first.resolve(breakdown({ parts: [part("system", 111)] }));
    await settle();
    expect(textOf(rowOf(tab.element, "system"))).toContain("222");
    expect(textOf(tab.element)).not.toContain("111");
  });

  it("goes back to the empty state when the session closes, even with a load in flight", async () => {
    const gate = deferred<ContextBreakdown>();
    installFakeIpc({ session_context_breakdown: () => gate.promise });
    const tab = createContextTab();
    tab.showSession(SESSION);
    tab.showSession(null);
    gate.resolve(breakdown());
    await settle();
    expect(textOf(tab.element)).toContain(CONTEXT_EMPTY);
    expect(tab.element.querySelector(".ctx-report")).toBeNull();
  });

  it("ignores results and further calls after dispose", async () => {
    const gate = deferred<ContextBreakdown>();
    const ipc = installFakeIpc({ session_context_breakdown: () => gate.promise });
    const tab = createContextTab();
    tab.showSession(SESSION);
    tab.dispose();
    gate.resolve(breakdown());
    await settle();
    expect(tab.element.querySelector(".ctx-report")).toBeNull();
    tab.showSession(SESSION);
    tab.refresh();
    expect(ipc.callsTo("session_context_breakdown")).toHaveLength(1);
  });

  it("renders from the mock backend, whose parts add up", async () => {
    const { invoke } = createMockBackend();
    setInvoker(invoke);
    const tree = (await invoke("session_tree", {
      view: "active",
      sort: "updated",
      descending: true,
    })) as { groups: Array<{ sessions: Array<{ session_id: string }> }> };
    const id = tree.groups[0]?.sessions[0]?.session_id ?? "";
    const data = (await invoke("session_context_breakdown", {
      session_id: id,
    })) as ContextBreakdown;
    expect(data.parts.reduce((total, one) => total + one.tokens, 0)).toBe(
      data.estimated_input_tokens,
    );
    const tab = createContextTab();
    tab.showSession(id);
    await settle();
    expect(tab.element.querySelectorAll("tbody tr")).toHaveLength(9);
    await expect(
      invoke("session_context_breakdown", { session_id: "no-such-session" }),
    ).rejects.toMatchObject({ code: "not_found" });
  });
});

describe("context icon", () => {
  it("draws fixed shapes and carries no text", () => {
    const drawn = icon("context");
    expect(drawn.getAttribute("aria-hidden")).toBe("true");
    expect(drawn.querySelectorAll("rect, path").length).toBeGreaterThanOrEqual(2);
    expect(drawn.textContent).toBe("");
  });
});
