import { afterEach, describe, expect, it, vi } from "vitest";
import { el } from "../src/dom";
import { createPopover } from "../src/ui/popover";
import { segmented } from "../src/ui/segmented";
import { createTabs, tabIdFor } from "../src/ui/tabs";

afterEach(() => {
  document.body.replaceChildren();
});

function key(target: Element, name: string): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true }));
}

describe("segmented", () => {
  const choices = [
    ["deny", "Deny"],
    ["prompt", "Prompt"],
    ["allow", "Allow"],
  ] as const;

  it("marks the current value and gives only it a tab stop", () => {
    const group = segmented({ label: "write", choices, value: "prompt", onChoose: vi.fn() });
    expect(group.getAttribute("role")).toBe("radiogroup");
    expect(group.getAttribute("aria-label")).toBe("write");
    const radios = Array.from(group.querySelectorAll("button"));
    expect(radios.map((r) => r.getAttribute("aria-checked"))).toEqual(["false", "true", "false"]);
    expect(radios.map((r) => r.tabIndex)).toEqual([-1, 0, -1]);
    expect(radios.map((r) => r.textContent)).toEqual(["Deny", "Prompt", "Allow"]);
  });

  it("reports a new choice and ignores the current one", () => {
    const onChoose = vi.fn();
    const group = segmented({ label: "read", choices, value: "deny", onChoose });
    const [deny, , allow] = Array.from(group.querySelectorAll("button"));
    deny?.click();
    expect(onChoose).not.toHaveBeenCalled();
    allow?.click();
    expect(onChoose).toHaveBeenCalledExactlyOnceWith("allow");
    // The control does not change itself; the caller re-renders from recorded state.
    expect(allow?.getAttribute("aria-checked")).toBe("false");
  });

  it("moves focus with arrow keys without choosing", () => {
    const onChoose = vi.fn();
    const group = segmented({ label: "execute", choices, value: "deny", onChoose });
    document.body.append(group);
    const radios = Array.from(group.querySelectorAll("button"));
    radios[0]?.focus();
    key(radios[0] as Element, "ArrowRight");
    expect(document.activeElement).toBe(radios[1]);
    key(radios[1] as Element, "ArrowLeft");
    key(radios[0] as Element, "ArrowLeft");
    expect(document.activeElement).toBe(radios[2]);
    expect(onChoose).not.toHaveBeenCalled();
  });

  it("does not report a click on a disabled choice", () => {
    const onChoose = vi.fn();
    const group = segmented({ label: "x", choices, value: "deny", onChoose, class: "access" });
    expect(group.className).toBe("segmented access");
    const allow = group.querySelectorAll("button")[2] as HTMLButtonElement;
    allow.disabled = true;
    allow.click();
    expect(onChoose).not.toHaveBeenCalled();
  });

  it("renders a hostile label as text", () => {
    const group = segmented({
      label: "x",
      choices: [["a", "<img src=x onerror=alert(1)>\u202e"]],
      value: "a",
      onChoose: vi.fn(),
    });
    expect(group.querySelector("img")).toBeNull();
    expect(group.textContent).toBe("<img src=x onerror=alert(1)>[U+202E]");
  });
});

describe("tabs", () => {
  const specs = [
    { id: "pending", label: "Pending" },
    { id: "history", label: "History" },
  ] as const;

  it("selects on click and reports the change once", () => {
    const onSelect = vi.fn();
    const tabs = createTabs("Approvals", specs, "pending", onSelect);
    const [pending, history] = Array.from(tabs.element.querySelectorAll("button"));
    expect(tabs.element.getAttribute("role")).toBe("tablist");
    expect(pending?.getAttribute("aria-selected")).toBe("true");
    history?.click();
    history?.click();
    expect(onSelect).toHaveBeenCalledExactlyOnceWith("history");
    expect(tabs.current()).toBe("history");
    expect(history?.tabIndex).toBe(0);
    expect(pending?.tabIndex).toBe(-1);
  });

  it("selects with arrow keys and wraps", () => {
    const onSelect = vi.fn();
    const tabs = createTabs("Approvals", specs, "pending", onSelect);
    document.body.append(tabs.element);
    const [pending, history] = Array.from(tabs.element.querySelectorAll("button"));
    key(pending as Element, "ArrowRight");
    expect(tabs.current()).toBe("history");
    expect(document.activeElement).toBe(history);
    key(history as Element, "ArrowRight");
    expect(tabs.current()).toBe("pending");
    expect(onSelect.mock.calls).toEqual([["history"], ["pending"]]);
  });

  it("selects without reporting and updates labels", () => {
    const onSelect = vi.fn();
    const tabs = createTabs("Approvals", specs, "pending", onSelect);
    tabs.select("history");
    expect(onSelect).not.toHaveBeenCalled();
    tabs.setLabel("pending", "Pending (3)");
    expect(tabs.element.querySelector('[data-tab="pending"]')?.textContent).toBe("Pending (3)");
  });

  it("links a tab to its panel only when the spec names one", () => {
    const tabs = createTabs(
      "Approvals",
      [
        { id: "pending", label: "Pending", panelId: "pending-panel" },
        { id: "history", label: "History" },
      ],
      "pending",
      vi.fn(),
    );
    const [pending, history] = Array.from(tabs.element.querySelectorAll("button"));
    expect(pending?.id).toBe(tabIdFor("pending-panel"));
    expect(pending?.getAttribute("aria-controls")).toBe("pending-panel");
    expect(history?.id).toBe("");
    expect(history?.hasAttribute("aria-controls")).toBe(false);
  });
});

describe("popover", () => {
  it("closes on Escape and on a click outside, but not inside", () => {
    const inside = document.createElement("button");
    const popover = createPopover("View", { class: "align-end", label: "View options" }, inside);
    document.body.append(popover.element);
    expect(popover.element.className).toBe("popover align-end");
    expect(popover.panel.getAttribute("aria-label")).toBe("View options");

    popover.element.open = true;
    inside.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(popover.element.open).toBe(true);
    document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(popover.element.open).toBe(false);

    popover.element.open = true;
    key(inside, "Escape");
    expect(popover.element.open).toBe(false);
    expect(document.activeElement).toBe(popover.summary);
  });

  it("shows an icon trigger named by the text, and tracks the open state", async () => {
    const popover = createPopover("Chat settings", { icon: "gear" }, el("p", { text: "x" }));
    document.body.append(popover.element);
    const { summary, panel } = popover;
    expect(summary.textContent).toBe("");
    expect(summary.title).toBe("Chat settings");
    expect(summary.getAttribute("aria-label")).toBe("Chat settings");
    expect(summary.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
    expect(summary.getAttribute("aria-controls")).toBe(panel.id);
    expect(summary.getAttribute("aria-expanded")).toBe("false");
    popover.element.open = true;
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(summary.getAttribute("aria-expanded")).toBe("true");
    popover.dispose();
  });

  it("stops listening after dispose", () => {
    const popover = createPopover("Skills", {});
    document.body.append(popover.element);
    popover.dispose();
    popover.element.open = true;
    document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(popover.element.open).toBe(true);
  });
});
