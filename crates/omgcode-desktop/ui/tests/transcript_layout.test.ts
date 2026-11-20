import { afterEach, beforeEach, describe, expect, it } from "vitest";
import mainCss from "../src/styles/main.css?raw";

// jsdom computes the cascade but no layout, so these checks read the properties that decide how
// an entry is sized: a centred flex item fits its content; a stretched one fills the column.
let style: HTMLStyleElement;
let transcript: HTMLElement;

function entry(className: string): HTMLElement {
  const node = document.createElement("article");
  node.className = className;
  transcript.appendChild(node);
  return node;
}

beforeEach(() => {
  style = document.createElement("style");
  style.textContent = mainCss.replace(/@import[^;]*;/g, "");
  document.head.appendChild(style);
  transcript = document.createElement("div");
  transcript.className = "transcript";
  document.body.appendChild(transcript);
});

afterEach(() => {
  transcript.remove();
  style.remove();
});

describe("the transcript column (ADR-0072)", () => {
  it("is a share of the transcript's width, not a character count", () => {
    expect(mainCss).not.toMatch(/\d+ch\b/);
    expect(getComputedStyle(transcript).getPropertyValue("--chat-column").trim()).toBe("90%");
  });

  it("centres a reply at the column's width", () => {
    const reply = getComputedStyle(entry("entry bubble bubble-assistant"));
    expect(reply.alignSelf).toBe("center");
    expect(reply.width).toBe("90%");
  });

  it("centres the operator's bubble and sizes it to its text, up to the column's width", () => {
    const own = getComputedStyle(entry("entry bubble bubble-user"));
    expect(own.alignSelf).toBe("center");
    expect(own.width).toBe("auto");
    expect(own.maxWidth).toBe("90%");
  });
});
