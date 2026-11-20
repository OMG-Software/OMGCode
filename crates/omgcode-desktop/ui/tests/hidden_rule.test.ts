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
      expect(sheet(name)).toMatch(/(^|\n|,\s*)\[hidden\]\s*\{\s*display:\s*none;\s*\}/);
    },
  );
});
