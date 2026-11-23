import { afterEach, beforeEach, describe, expect, it } from "vitest";
import mainCss from "../src/styles/main.css?raw";

// jsdom computes the cascade but no layout, so check the properties that size and align a bubble.
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

describe("the transcript column", () => {
  it.each(["bubble-user", "bubble-assistant", "bubble-assistant bubble-live"])(
    "centres %s within the transcript with left-aligned text",
    (classes) => {
      const bubble = getComputedStyle(entry(`entry bubble ${classes}`));
      expect(bubble.alignSelf).toBe("center");
      expect(bubble.width).toBe("96%");
      expect(bubble.textAlign).toBe("left");
    },
  );
});
