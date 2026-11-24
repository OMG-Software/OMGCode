import { describe, expect, it } from "vitest";

const sheets = import.meta.glob<string>("../src/styles/*.css", {
  query: "?raw",
  import: "default",
  eager: true,
});

function sheet(name: string): string {
  const entry = Object.entries(sheets).find(([path]) => path.endsWith(`/${name}`));
  return entry?.[1] ?? "";
}

describe("the hidden attribute in each window stylesheet", () => {
  // `.settings-nav { display: flex }` and `.field { display: grid }` outrank the
  // browser's default `[hidden]`, so each window needs its own generic rule.
  it.each(["main.css", "approvals.css", "settings.css"])(
    "%s hides an element that has the hidden attribute",
    (name) => {
      expect(sheet(name)).toMatch(
        /(^|\n|,\s*)\[hidden\]\s*\{[^}]*display:\s*none(\s*!important)?;\s*\}/,
      );
    },
  );

  // A text match accepts a rule that still loses the cascade: `[hidden]` and `.settings-nav`
  // have equal specificity, so the later `display: flex` won. Check the computed style instead.
  // The settings sidebar, and the collapsible Workspaces group inside it, whose list sets
  // `display: flex` and is hidden when the group is collapsed.
  it.each([
    ["nav", "settings-nav"],
    ["div", "nav-children"],
  ] as const)("hides a settings %s.%s when it has the hidden attribute", (tag, className) => {
    const style = document.createElement("style");
    style.textContent = sheet("settings.css").replace(/@import[^;]*;/g, "");
    document.head.appendChild(style);
    const node = document.createElement(tag);
    node.className = className;
    node.hidden = true;
    document.body.appendChild(node);
    try {
      expect(getComputedStyle(node).display).toBe("none");
      node.hidden = false;
      expect(getComputedStyle(node).display).toBe("flex");
    } finally {
      node.remove();
      style.remove();
    }
  });
});
