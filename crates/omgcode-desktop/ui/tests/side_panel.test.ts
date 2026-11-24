import { afterEach, describe, expect, it, vi } from "vitest";
import { el } from "../src/dom";
import { createSidePanel, type SideTab } from "../src/main/side_panel";

function tab(id: string, onVisible?: (visible: boolean) => void): SideTab {
  const spec: SideTab = {
    id,
    label: id.charAt(0).toUpperCase() + id.slice(1),
    icon: "terminal",
    content: el("p", { text: `content of ${id}` }),
  };
  if (onVisible !== undefined) {
    spec.onVisible = onVisible;
  }
  return spec;
}

function build(tabs: SideTab[]) {
  const onCollapse = vi.fn();
  const onExpand = vi.fn();
  const panel = createSidePanel(tabs, { onCollapse, onExpand });
  document.body.replaceChildren(panel.element);
  return { panel, onCollapse, onExpand };
}

function tabButton(id: string): HTMLButtonElement {
  return document.querySelector(`[role="tab"][data-tab="${id}"]`) as HTMLButtonElement;
}

function pane(id: string): HTMLElement {
  return document.getElementById(`side-pane-${id}`) as HTMLElement;
}

function key(target: HTMLElement, name: string): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true }));
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("the side panel's tabs", () => {
  it("shows a tablist with the first tab selected and its pane labelled by it", () => {
    build([tab("terminal"), tab("files")]);
    const list = document.querySelector('[role="tablist"]') as HTMLElement;
    expect(list.getAttribute("aria-label")).toBe("Side panel tabs");
    expect(Array.from(list.querySelectorAll('[role="tab"]')).map((t) => t.textContent)).toEqual([
      "Terminal",
      "Files",
    ]);
    expect(tabButton("terminal").getAttribute("aria-selected")).toBe("true");
    expect(tabButton("files").getAttribute("aria-selected")).toBe("false");
    expect(tabButton("files").tabIndex).toBe(-1);
    expect(pane("terminal").hidden).toBe(false);
    expect(pane("files").hidden).toBe(true);
    expect(pane("files").getAttribute("role")).toBe("tabpanel");
    expect(pane("files").getAttribute("aria-labelledby")).toBe(tabButton("files").id);
    expect(tabButton("files").getAttribute("aria-controls")).toBe("side-pane-files");
  });

  it("moves between tabs with the arrow keys, wrapping at the ends", () => {
    const { panel } = build([tab("terminal"), tab("files")]);
    panel.setCollapsed(false);
    tabButton("terminal").focus();
    key(tabButton("terminal"), "ArrowRight");
    expect(panel.current()).toBe("files");
    expect(document.activeElement).toBe(tabButton("files"));
    expect(pane("files").hidden).toBe(false);
    expect(pane("terminal").hidden).toBe(true);
    key(tabButton("files"), "ArrowRight");
    expect(panel.current()).toBe("terminal");
    key(tabButton("terminal"), "ArrowLeft");
    expect(panel.current()).toBe("files");
  });

  it("selects a tab on a click and remembers it across collapsing and expanding", () => {
    const { panel } = build([tab("terminal"), tab("files")]);
    panel.setCollapsed(false);
    tabButton("files").click();
    expect(panel.current()).toBe("files");
    panel.setCollapsed(true);
    panel.setCollapsed(false);
    expect(panel.current()).toBe("files");
    expect(pane("files").hidden).toBe(false);
  });

  it("takes any number of tabs from the list it is given", () => {
    const { panel } = build([tab("terminal"), tab("files"), tab("git"), tab("tasks")]);
    expect(document.querySelectorAll('[role="tab"]')).toHaveLength(4);
    panel.select("tasks");
    expect(pane("tasks").hidden).toBe(false);
    expect(document.querySelectorAll(".panel-rail button")).toHaveLength(4);
  });

  it("refuses an empty tab list", () => {
    expect(() => createSidePanel([], { onCollapse: vi.fn(), onExpand: vi.fn() })).toThrow();
  });
});

describe("the collapsed rail", () => {
  it("shows one icon per tab and expands on the tab that was clicked", () => {
    const { panel, onExpand } = build([tab("terminal"), tab("files")]);
    const rail = document.querySelector(".panel-rail") as HTMLElement;
    expect(rail.hidden).toBe(false);
    expect(
      Array.from(rail.querySelectorAll("button")).map((b) => b.getAttribute("aria-label")),
    ).toEqual(["Show terminal panel", "Show files panel"]);
    expect((document.querySelector(".terminal-full") as HTMLElement).hidden).toBe(true);
    (rail.querySelector('[aria-label="Show files panel"]') as HTMLButtonElement).click();
    expect(onExpand).toHaveBeenCalledTimes(1);
    expect(panel.current()).toBe("files");
    panel.setCollapsed(false);
    expect(rail.hidden).toBe(true);
    expect(pane("files").hidden).toBe(false);
    expect(tabButton("files").getAttribute("aria-selected")).toBe("true");
  });

  it("moves focus between the selected tab and its rail icon", () => {
    const { panel } = build([tab("terminal"), tab("files")]);
    panel.setCollapsed(false);
    tabButton("files").click();
    tabButton("files").focus();
    panel.setCollapsed(true);
    expect(document.activeElement?.getAttribute("aria-label")).toBe("Show files panel");
    panel.setCollapsed(false);
    expect(document.activeElement).toBe(tabButton("files"));
  });

  it("calls the collapse handler from the header button", () => {
    const { panel, onCollapse } = build([tab("terminal")]);
    panel.setCollapsed(false);
    (document.querySelector('[aria-label="Collapse side panel"]') as HTMLButtonElement).click();
    expect(onCollapse).toHaveBeenCalledTimes(1);
  });
});

describe("the fixed footer", () => {
  it("is exposed, empty, outside the scrolling panes, and visible on every tab", () => {
    const { panel } = build([tab("terminal"), tab("files")]);
    panel.setCollapsed(false);
    expect(panel.footer.classList.contains("side-footer")).toBe(true);
    expect(panel.footer.childElementCount).toBe(0);
    expect(panel.footer.closest(".side-panes")).toBeNull();
    expect(panel.element.contains(panel.footer)).toBe(true);
    expect(panel.footer.parentElement?.classList.contains("terminal-full")).toBe(true);
    for (const id of ["terminal", "files"]) {
      panel.select(id);
      expect(panel.footer.hidden).toBe(false);
      expect(panel.footer.closest("[hidden]")).toBeNull();
    }
    panel.footer.append(el("span", { text: "kept" }));
    panel.select("terminal");
    expect(panel.footer.textContent).toBe("kept");
  });
});

describe("tab visibility", () => {
  it("tells a tab when it is shown and when it is not, only on a change", () => {
    const files = vi.fn();
    const terminal = vi.fn();
    const { panel } = build([tab("terminal", terminal), tab("files", files)]);
    expect(terminal).not.toHaveBeenCalled();
    panel.setCollapsed(false);
    expect(terminal).toHaveBeenLastCalledWith(true);
    panel.select("files");
    expect(terminal).toHaveBeenLastCalledWith(false);
    expect(files).toHaveBeenLastCalledWith(true);
    panel.setCollapsed(true);
    expect(files).toHaveBeenLastCalledWith(false);
    expect(files).toHaveBeenCalledTimes(2);
    panel.setCollapsed(true);
    expect(files).toHaveBeenCalledTimes(2);
  });
});
