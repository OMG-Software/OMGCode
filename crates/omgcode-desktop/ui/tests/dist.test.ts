import { describe, expect, it } from "vitest";

const files = import.meta.glob<string>("../dist/**/*.{js,css,html}", {
  query: "?raw",
  import: "default",
  eager: true,
});

const byPath = new Map<string, string>(
  Object.entries(files).map(([key, text]) => [key.replace("../dist/", ""), text]),
);

const MARKER = "OMGCODE_MOCK_FIXTURE_MARKER";

/** JS files reachable from an HTML entry, following static and dynamic relative imports. */
function reachableScripts(entry: string): Map<string, string> {
  const html = byPath.get(entry) ?? "";
  const seen = new Map<string, string>();
  const queue: string[] = [];
  for (const match of html.matchAll(/(?:src|href)="\.\/([^"]+\.js)"/g)) {
    queue.push(match[1] as string);
  }
  while (queue.length > 0) {
    const path = queue.pop() as string;
    const text = byPath.get(path);
    if (text === undefined || seen.has(path)) {
      continue;
    }
    seen.set(path, text);
    for (const match of text.matchAll(/(?:from|import)\s*\(?\s*["'](\.{1,2}\/[^"']+\.js)["']/g)) {
      const resolved = new URL(match[1] as string, `file:///${path}`).pathname.slice(1);
      queue.push(resolved);
    }
  }
  return seen;
}

const present = byPath.size > 0;
const suite = present ? describe : describe.skip;

suite(
  present ? "built dist/" : "built dist/ is missing: run `npm run test:dist` to build and check it",
  () => {
    it("main window bundle never mentions approval_decide or approval_detail", () => {
      const scripts = reachableScripts("index.html");
      expect(scripts.size).toBeGreaterThan(0);
      for (const [path, text] of scripts) {
        expect(text, path).not.toContain("approval_decide");
        expect(text, path).not.toContain("approval_detail");
      }
    });

    it("approvals window bundle does call approval_decide", () => {
      const joined = Array.from(reachableScripts("approvals.html").values()).join("\n");
      expect(joined).toContain("approval_decide");
      expect(joined).toContain("approval_detail");
    });

    it("contains no mock fixture marker in any asset", () => {
      for (const [path, text] of byPath) {
        expect(text, path).not.toContain(MARKER);
      }
    });

    it("has HTML with no inline script, inline style or remote URL", () => {
      for (const entry of ["index.html", "approvals.html"]) {
        const html = byPath.get(entry) ?? "";
        expect(html, entry).not.toBe("");
        for (const match of html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)) {
          expect(match[1]?.trim(), entry).toBe("");
        }
        expect(html, entry).not.toMatch(/<style\b|\sstyle\s*=|https?:/i);
      }
    });
  },
);
