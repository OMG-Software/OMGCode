import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TranscriptEntry } from "../src/ipc/types";
import { type MainApp, startMain } from "../src/main/app";
import {
  backdrop,
  CAPTURE_PADDING,
  CaptureError,
  captureScale,
  codePointClusters,
  graphemes,
  MAX_CANVAS_AREA,
  MAX_CANVAS_SIDE,
  MAX_TEXT_RUNS,
  paintElement,
} from "../src/main/capture";
import { COPY_FEEDBACK_MS } from "../src/main/clipboard";
import { renderMarkdown } from "../src/main/markdown";
import { plainText } from "../src/main/plain_text";
import { createLiveEntry, entryContexts, renderEntry, replyLabel } from "../src/main/transcript";
import mainCss from "../src/styles/main.css?raw";
import {
  installFakeIpc,
  mainHandlers,
  mountRoot,
  page,
  session,
  settle,
  tree,
  userMessage,
} from "./helpers";

const RLO = String.fromCharCode(0x202e);
const at = "2026-01-02T10:00:00Z";

function reply(seq: number, text: string): TranscriptEntry {
  return {
    seq,
    timestamp: at,
    kind: "message",
    role: "assistant",
    source: "chat",
    text,
    call_id: null,
  };
}

function manifest(seq: number, model: string): TranscriptEntry {
  return {
    seq,
    timestamp: at,
    kind: "request_manifest",
    request_id: `r${seq}`,
    model,
    prompt_version: "v1",
    tools: [],
  };
}

function turnStarted(seq: number): TranscriptEntry {
  return { seq, timestamp: at, kind: "turn_started", request_id: `t${seq}` };
}

function actionButton(node: Element, label: string): HTMLButtonElement {
  const found = Array.from(node.querySelectorAll<HTMLButtonElement>(".reply-action")).find(
    (b) => b.textContent === label,
  );
  if (found === undefined) {
    throw new Error(`no ${label} button`);
  }
  return found;
}

function stubClipboard(clipboard: Partial<Clipboard>): void {
  Object.defineProperty(navigator, "clipboard", { value: clipboard, configurable: true });
}

function md(source: string): HTMLElement {
  const node = renderMarkdown(source);
  if (node === null) {
    throw new Error("expected Markdown");
  }
  return node;
}

afterEach(() => {
  Reflect.deleteProperty(Range.prototype, "getClientRects");
  vi.useRealTimers();
  vi.restoreAllMocks();
  Reflect.deleteProperty(navigator, "clipboard");
  Reflect.deleteProperty(globalThis, "ClipboardItem");
});

describe("model labels", () => {
  it("labels each reply with its turn's model and clears it at the next turn", () => {
    const entries = [
      turnStarted(1),
      manifest(2, "qwen2.5-coder"),
      reply(3, "first"),
      manifest(4, "gemma-4"),
      reply(5, "after a tool round"),
      turnStarted(6),
      reply(7, "no manifest in this turn"),
    ];
    const contexts = entryContexts(entries);
    expect(contexts.map((c) => c.model)).toEqual([
      null,
      "qwen2.5-coder",
      "qwen2.5-coder",
      "gemma-4",
      "gemma-4",
      null,
      null,
    ]);
    const labels = entries.map(
      (entry, i) => renderEntry(entry, contexts[i]).querySelector(".entry-label")?.textContent,
    );
    expect(labels[2]).toBe("qwen2.5-coder");
    expect(labels[4]).toBe("gemma-4");
    expect(labels[6]).toBe("Assistant");
  });

  it("falls back to Assistant for an empty id and keeps a hostile id as marked text", () => {
    expect(replyLabel(null)).toBe("Assistant");
    expect(replyLabel("  ")).toBe("Assistant");
    const node = renderEntry(reply(1, "x"), { model: `<img src=x>${RLO}gpj` });
    const label = node.querySelector(".entry-label");
    expect(label?.textContent).toBe("<img src=x>[U+202E]gpj");
    expect(label?.getAttribute("title")).toBe("<img src=x>[U+202E]gpj");
    expect(node.querySelector("img")).toBeNull();
  });

  it("names the live entry after the model, or Assistant when it is not known", () => {
    expect(createLiveEntry("llama3").element.textContent).toContain(
      "llama3 (generating, not saved yet)",
    );
    expect(createLiveEntry().element.textContent).toContain(
      "Assistant (generating, not saved yet)",
    );
  });

  it("shows the model on a reply loaded in the window", async () => {
    vi.useFakeTimers();
    const s = session({ session_id: "s-model", title: "Model" });
    installFakeIpc(
      mainHandlers({
        session_tree: () => tree([{ id: "g1", folder: "C:\\work\\one", sessions: [s] }]),
        session_transcript: () =>
          page(s, [
            turnStarted(1),
            manifest(2, "lfm2.5:latest"),
            userMessage(3, "hi"),
            reply(4, "ok"),
          ]),
      }),
    );
    const root = mountRoot();
    const app: MainApp = startMain(root);
    await settle();
    (root.querySelector('[data-session-id="s-model"]') as HTMLElement).click();
    await settle();
    expect(root.querySelector(".bubble-assistant .entry-label")?.textContent).toBe("lfm2.5:latest");
    expect(root.querySelector(".bubble-user .entry-label")?.textContent).toBe("You");
    app.dispose();
  });
});

describe("reply actions", () => {
  const SOURCE =
    "Run `server.py`:\n\n```bash\npython3 server.py\n```\n\n- **one**\n- [docs](https://x.example)";

  it("appear only on assistant replies, outside the image", () => {
    const node = renderEntry(reply(1, SOURCE), { model: "m" });
    const row = node.querySelector(".reply-actions");
    expect(row?.getAttribute("data-capture")).toBe("skip");
    expect(Array.from(node.querySelectorAll(".reply-action")).map((b) => b.textContent)).toEqual([
      "Copy Markdown",
      "Copy text",
      "Copy image",
    ]);
    expect(renderEntry(userMessage(2, "hi")).querySelector(".reply-actions")).toBeNull();
    expect(createLiveEntry("m").element.querySelector(".reply-actions")).toBeNull();
  });

  it("copies the Markdown source, with overrides as their visible markers", async () => {
    const writes: string[] = [];
    stubClipboard({ writeText: async (value: string) => void writes.push(value) });
    const source = `${SOURCE}\n\n${RLO}txt`;
    const button = actionButton(renderEntry(reply(1, source)), "Copy Markdown");
    button.click();
    await vi.waitFor(() => expect(button.textContent).toBe("Copied"));
    expect(writes).toEqual([source.replace(RLO, "[U+202E]")]);
  });

  it("copies the reply as text without Markdown symbols", async () => {
    const writes: string[] = [];
    stubClipboard({ writeText: async (value: string) => void writes.push(value) });
    const button = actionButton(renderEntry(reply(1, SOURCE)), "Copy text");
    button.click();
    await vi.waitFor(() => expect(button.textContent).toBe("Copied"));
    expect(writes).toEqual([
      "Run server.py:\n\npython3 server.py\n\n• one\n• docs (https://x.example)",
    ]);
  });

  it("copies a reply over the Markdown limit as its plain text", async () => {
    const writes: string[] = [];
    stubClipboard({ writeText: async (value: string) => void writes.push(value) });
    const long = `**${"a".repeat(100_001)}**`;
    actionButton(renderEntry(reply(1, long)), "Copy text").click();
    await vi.waitFor(() => expect(writes).toEqual([long]));
  });

  it("reports a refused write and restores the label", async () => {
    vi.useFakeTimers();
    stubClipboard({ writeText: () => Promise.reject(new Error("denied")) });
    const button = actionButton(renderEntry(reply(1, "x")), "Copy Markdown");
    button.click();
    await vi.waitFor(() => expect(button.textContent).toBe("Copy failed"));
    vi.advanceTimersByTime(COPY_FEEDBACK_MS);
    expect(button.textContent).toBe("Copy Markdown");
  });
});

describe("plain text", () => {
  it("keeps list numbers, indents nested items, and separates blocks", () => {
    const text = plainText(
      md("# Title\n\n3. three\n4. four\n   - inner\n\n> quoted\n\n---\n\nend ~~old~~ *new*"),
    );
    expect(text).toBe("Title\n\n3. three\n4. four\n   • inner\n\nquoted\n\nend old new");
  });

  it("separates table cells with tabs and keeps code exactly", () => {
    const text = plainText(md("| a | b |\n| - | - |\n| 1 | 2 |\n\n```\n  x\n\ty\n```"));
    expect(text).toBe("a\tb\n1\t2\n\n  x\n\ty");
  });

  it("separates blocks inside a list item and leaves blank lines unindented", () => {
    const text = plainText(md("1. run\n\n   ```\n   a\n\n   b\n   ```"));
    expect(text).toBe("1. run\n\n   a\n\n   b");
    expect(text.split("\n").some((line) => line !== "" && line.trim() === "")).toBe(false);
  });

  it("keeps paragraphs in an item apart and a nested list on the next line", () => {
    expect(plainText(md("- one\n\n  two\n  - inner"))).toBe("• one\n\n  two\n  • inner");
  });

  it("describes an image as the reply shows it", () => {
    expect(plainText(md("![cat](c.png)"))).toBe("[image: cat] (c.png)");
  });
});

/** A 2D context that records the calls the painter makes. */
function recordingContext(): { context: CanvasRenderingContext2D; calls: string[] } {
  const calls: string[] = [];
  const state = { fillStyle: "", strokeStyle: "", font: "", lineWidth: 1, textBaseline: "" };
  const record =
    (name: string) =>
    (...args: unknown[]): void => {
      calls.push(`${name}(${args.map(String).join(",")})${name === "fill" ? state.fillStyle : ""}`);
    };
  const context = {
    ...state,
    fillRect: record("fillRect"),
    fillText: record("fillText"),
    beginPath: record("beginPath"),
    rect: record("rect"),
    moveTo: record("moveTo"),
    arcTo: record("arcTo"),
    closePath: record("closePath"),
    fill: () => calls.push(`fill ${context.fillStyle}`),
    stroke: () => calls.push(`stroke ${context.strokeStyle}`),
    save: record("save"),
    restore: record("restore"),
    clip: record("clip"),
    setLineDash: record("setLineDash"),
    scale: record("scale"),
    measureText: () => ({ fontBoundingBoxAscent: 10 }),
  };
  return { context: context as unknown as CanvasRenderingContext2D, calls };
}

function placeAt(node: Element, left: number, top: number, width: number, height: number): void {
  Object.defineProperty(node, "getBoundingClientRect", {
    value: () => ({ left, top, width, height, right: left + width, bottom: top + height }),
    configurable: true,
  });
}

/** Lays out text ranges on one line, eight pixels per code unit, from x = 100, y = 50. */
function stubRangeRects(): void {
  // jsdom does not lay out text, so Range has no getClientRects to spy on.
  Object.defineProperty(Range.prototype, "getClientRects", {
    configurable: true,
    value(this: Range) {
      const width = (this.endOffset - this.startOffset) * 8;
      return [{ left: 100 + this.startOffset * 8, top: 50, width, height: 16 }];
    },
  });
}

describe("image painting", () => {
  beforeEach(() => {
    stubRangeRects();
  });

  it("paints backgrounds and text relative to the origin, without buttons", () => {
    const root = document.createElement("article");
    root.style.backgroundColor = "rgb(22, 24, 27)";
    root.append(document.createTextNode("Hi there"));
    const skipped = document.createElement("div");
    skipped.setAttribute("data-capture", "skip");
    skipped.append("hidden row");
    const control = document.createElement("button");
    control.append("Copy");
    root.append(skipped, control);
    document.body.append(root);
    placeAt(root, 90, 40, 200, 30);

    const { context, calls } = recordingContext();
    paintElement(
      context,
      root,
      { x: 90 - CAPTURE_PADDING, y: 40 - CAPTURE_PADDING },
      {
        runs: 100,
      },
    );
    expect(calls).toContain("fill rgb(22, 24, 27)");
    expect(calls).toContain(
      `fillText(Hi,${100 - 90 + CAPTURE_PADDING},${50 - 40 + CAPTURE_PADDING + 10})`,
    );
    expect(calls).toContain(
      `fillText(there,${124 - 90 + CAPTURE_PADDING},${50 - 40 + CAPTURE_PADDING + 10})`,
    );
    expect(calls.some((c) => c.includes("hidden") || c.includes("Copy"))).toBe(false);
    root.remove();
  });

  it("paints each line of a wrapped inline element, not its bounding box", () => {
    const root = document.createElement("p");
    const code = document.createElement("code");
    code.style.display = "inline";
    code.style.backgroundColor = "rgb(38, 42, 47)";
    code.append("wrapped code");
    root.append("before ", code, " after");
    document.body.append(root);
    placeAt(root, 0, 0, 300, 40);
    placeAt(code, 10, 0, 280, 40);
    Object.defineProperty(code, "getClientRects", {
      value: () => [
        { left: 200, top: 0, width: 90, height: 20 },
        { left: 10, top: 20, width: 60, height: 20 },
      ],
      configurable: true,
    });
    const { context, calls } = recordingContext();
    paintElement(context, root, { x: 0, y: 0 }, { runs: 100 });
    const rects = calls.filter((c) => c.startsWith("rect("));
    expect(rects).toContain("rect(200,0,90,20)");
    expect(rects).toContain("rect(10,20,60,20)");
    expect(rects).not.toContain("rect(10,0,280,40)");
    expect(calls.filter((c) => c === "fill rgb(38, 42, 47)")).toHaveLength(2);
    root.remove();
  });

  it("draws list markers, which have no node of their own", () => {
    const root = md("3. a\n4. b\n   - c");
    document.body.append(root);
    const { context, calls } = recordingContext();
    paintElement(context, root, { x: 0, y: 0 }, { runs: 100 });
    const markers = calls
      .filter((c) => /^fillText\((\d+\.|•|◦),/.test(c))
      .map((c) => c.split(",")[0]);
    expect(markers).toEqual(["fillText(3.", "fillText(4.", "fillText(◦"]);
    root.remove();
  });

  it("strikes through text nested inside a line-through element", () => {
    const root = document.createElement("p");
    const del = document.createElement("del");
    del.style.textDecorationLine = "line-through";
    const strong = document.createElement("strong");
    strong.append("x");
    del.append(strong);
    root.append(del, " y");
    document.body.append(root);
    // The nested element's own computed style has no line-through, as in a browser.
    expect(getComputedStyle(strong).textDecorationLine).not.toContain("line-through");
    const { context, calls } = recordingContext();
    paintElement(context, root, { x: 0, y: 0 }, { runs: 100 });
    const strikes = calls.filter((c) => c.startsWith("fillRect(") && c.endsWith(",1)"));
    // One line through "x", none through "y", which is outside the deletion.
    expect(strikes).toEqual(["fillRect(100,58.8,8,1)"]);
    root.remove();
  });

  it("places each user-perceived character of a wrapped word as one glyph", () => {
    const accented = `e${String.fromCharCode(0x301)}`;
    const thumb = String.fromCodePoint(0x1f44d, 0x1f3fd);
    expect(graphemes(`${accented}${thumb}x`)).toEqual([accented, thumb, "x"]);

    // A range over the whole word reports two line boxes, so the word is placed in pieces.
    Object.defineProperty(Range.prototype, "getClientRects", {
      configurable: true,
      value(this: Range) {
        const length = this.endOffset - this.startOffset;
        const rect = { left: 100 + this.startOffset * 8, top: 50, width: length * 8, height: 16 };
        return length > 4 ? [rect, { ...rect, top: 66 }] : [rect];
      },
    });
    const root = document.createElement("p");
    root.append(`${accented}${thumb}x`);
    document.body.append(root);
    const { context, calls } = recordingContext();
    paintElement(context, root, { x: 0, y: 0 }, { runs: 100 });
    const glyphs = calls
      .filter((c) => c.startsWith("fillText("))
      .map((c) => c.slice("fillText(".length).split(",")[0]);
    expect(glyphs).toEqual([accented, thumb, "x"]);
    root.remove();
  });

  it("keeps joined glyphs whole without Intl.Segmenter", () => {
    const accented = `e${String.fromCharCode(0x301)}`;
    const thumb = String.fromCodePoint(0x1f44d, 0x1f3fd);
    const family = String.fromCodePoint(0x1f468, 0x200d, 0x1f469, 0x200d, 0x1f467);
    const heart = String.fromCodePoint(0x2764, 0xfe0f);
    const britain = String.fromCodePoint(0x1f1ec, 0x1f1e7);
    const france = String.fromCodePoint(0x1f1eb, 0x1f1f7);
    const word = `${accented}${thumb}${family}${heart}${britain}${france}ab`;
    const expected = [accented, thumb, family, heart, britain, france, "a", "b"];
    expect(codePointClusters(word)).toEqual(expected);
    // The fallback agrees with the segmenter on these clusters.
    expect(graphemes(word)).toEqual(expected);
  });

  it("pads the image with the colour behind the entry", () => {
    const pane = document.createElement("section");
    pane.style.backgroundColor = "rgb(1, 2, 3)";
    const wrapper = document.createElement("div");
    const entry = document.createElement("article");
    wrapper.append(entry);
    pane.append(wrapper);
    document.body.append(pane);
    expect(backdrop(entry)).toBe("rgb(1, 2, 3)");
    pane.style.backgroundColor = "";
    expect(backdrop(entry)).toBe("#000000");
    pane.remove();
  });

  it("lets a wide code block wrap while it is captured", () => {
    const rules = mainCss.replace(/\s+/g, " ");
    expect(rules).toContain(".capturing .md-code pre { overflow: visible; }");
    expect(rules).toContain(
      ".capturing .md-code pre code { white-space: pre-wrap; overflow-wrap: anywhere; }",
    );
  });

  it("refuses an entry with more text runs than the budget", () => {
    const root = document.createElement("div");
    root.append(document.createTextNode("a b c d"));
    document.body.append(root);
    const { context } = recordingContext();
    expect(() => paintElement(context, root, { x: 0, y: 0 }, { runs: 3 })).toThrow(CaptureError);
    expect(MAX_TEXT_RUNS).toBeGreaterThan(1000);
    root.remove();
  });

  it("reduces the scale to fit the canvas bounds and refuses an entry far too large", () => {
    expect(captureScale(400, 300, 2)).toBe(2);
    expect(captureScale(400, 300, 3)).toBe(2);
    expect(captureScale(400, 300, 0.5)).toBe(1);
    const tall = captureScale(700, 6000, 2);
    expect(6000 * tall).toBeLessThanOrEqual(MAX_CANVAS_SIDE);
    expect(700 * 6000 * tall * tall).toBeLessThanOrEqual(MAX_CANVAS_AREA + 1);
    expect(() => captureScale(700, 40_000, 2)).toThrow(CaptureError);
  });
});

describe("copy image", () => {
  it("paints the entry and writes one PNG to the clipboard", async () => {
    stubRangeRects();
    const { context, calls } = recordingContext();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(
      context as unknown as ReturnType<HTMLCanvasElement["getContext"]>,
    );
    const png = new Blob(["png"], { type: "image/png" });
    vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation((callback) => {
      callback(png);
    });
    const items: Array<Record<string, Promise<Blob>>> = [];
    class FakeItem {
      constructor(readonly parts: Record<string, Promise<Blob>>) {
        items.push(parts);
      }
    }
    Object.defineProperty(globalThis, "ClipboardItem", { value: FakeItem, configurable: true });
    stubClipboard({ write: async () => undefined, writeText: async () => undefined });

    const node = renderEntry(reply(1, "Hello"), { model: "m" });
    document.body.append(node);
    placeAt(node, 0, 0, 300, 80);
    const button = actionButton(node, "Copy image");
    button.click();
    await vi.waitFor(() => expect(button.textContent).toBe("Copied"));
    expect(items).toHaveLength(1);
    expect(Object.keys(items[0] ?? {})).toEqual(["image/png"]);
    await expect(items[0]?.["image/png"]).resolves.toBe(png);
    expect(calls.some((c) => c.startsWith("fillText(Hello"))).toBe(true);
    expect(calls.some((c) => c.includes("Copy"))).toBe(false);
    expect(node.classList.contains("capturing")).toBe(false);
    node.remove();
  });

  it("reports a failure when the entry has no area or no image clipboard exists", async () => {
    stubClipboard({ writeText: async () => undefined });
    const node = renderEntry(reply(1, "Hello"));
    const button = actionButton(node, "Copy image");
    button.click();
    await vi.waitFor(() => expect(button.textContent).toBe("Copy failed"));
  });
});
