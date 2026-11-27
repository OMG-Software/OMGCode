import { describe, expect, it } from "vitest";
import { append, el, neutralise } from "../src/dom";
import { iconButton, setIconButton } from "../src/ui/icons";

const OVERRIDE = String.fromCharCode(0x202e);
const HOSTILE = `abc${OVERRIDE}def`;

describe("el attributes", () => {
  it("neutralises the attributes a person reads or hears, like text", () => {
    const node = el("input", {
      attrs: {
        title: HOSTILE,
        "aria-label": HOSTILE,
        "aria-description": HOSTILE,
        placeholder: HOSTILE,
      },
    });
    for (const name of ["title", "aria-label", "aria-description", "placeholder"]) {
      expect(node.getAttribute(name)).toBe(neutralise(HOSTILE));
      expect(node.getAttribute(name)).not.toContain(OVERRIDE);
    }
  });

  it("leaves data attributes exactly as given", () => {
    const node = el("option", { attrs: { value: HOSTILE, "data-id": HOSTILE } });
    expect(node.getAttribute("value")).toBe(HOSTILE);
    expect(node.getAttribute("data-id")).toBe(HOSTILE);
  });

  it("neutralises text that was already neutralised to the same value", () => {
    const once = neutralise(HOSTILE);
    expect(el("span", { attrs: { title: once } }).title).toBe(once);
  });

  it("still refuses attributes that execute or navigate", () => {
    for (const name of ["onclick", "style", "href", "src", "srcdoc"]) {
      expect(() => el("a", { attrs: { [name]: "x" } })).toThrow(/not allowed/);
    }
  });
});

describe("append", () => {
  it("keeps a string child as one literal text node", () => {
    const parent = document.createElement("div");
    append(parent, "<img src=x onerror=1>");
    expect(parent.childNodes).toHaveLength(1);
    expect(parent.firstChild?.nodeType).toBe(Node.TEXT_NODE);
    expect(parent.firstChild?.textContent).toBe("<img src=x onerror=1>");
    expect(parent.querySelector("img")).toBeNull();
  });

  it("appends a node child as given and skips empty children", () => {
    const parent = document.createElement("div");
    const child = document.createElement("span");
    append(parent, child, null, undefined, false);
    expect(parent.childNodes).toHaveLength(1);
    expect(parent.firstChild).toBe(child);
  });
});

describe("icon buttons", () => {
  it("neutralise the label in the tooltip and the accessible name", () => {
    const node = iconButton("refresh", HOSTILE, () => undefined);
    expect(node.title).toBe(neutralise(HOSTILE));
    expect(node.getAttribute("aria-label")).toBe(neutralise(HOSTILE));
    setIconButton(node, "refresh", `x${OVERRIDE}`);
    expect(node.title).not.toContain(OVERRIDE);
    expect(node.getAttribute("aria-label")).not.toContain(OVERRIDE);
  });
});
