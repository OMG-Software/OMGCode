import { describe, expect, it } from "vitest";
// A glob never matches the file that contains it, so this file reads its
// own source directly. Without it the scan below would skip this file.
import ownSource from "./security.test.ts?raw";

const sources = import.meta.glob<string>("../src/**/*.ts", {
  query: "?raw",
  import: "default",
  eager: true,
});
const styles = import.meta.glob<string>("../src/**/*.css", {
  query: "?raw",
  import: "default",
  eager: true,
});
const pages = import.meta.glob<string>("../*.html", {
  query: "?raw",
  import: "default",
  eager: true,
});

const tests = import.meta.glob<string>("./*.ts", {
  query: "?raw",
  import: "default",
  eager: true,
});

const FORBIDDEN: Array<[string, RegExp]> = [
  ["innerHTML", /\binnerHTML\b/],
  ["outerHTML", /\bouterHTML\b/],
  ["insertAdjacentHTML", /\binsertAdjacentHTML\b/],
  ["document.write", /\bdocument\s*\.\s*write(ln)?\b/],
  ["DOMParser", /\bDOMParser\b/],
  ["eval", /\beval\s*\(/],
  ["new Function", /\bnew\s+Function\b/],
  ["string timer", /\b(setTimeout|setInterval)\s*\(\s*["'`]/],
  ["computed dynamic import", /\bimport\s*\(\s*[^"'\s)]/],
  ["window.open", /\bwindow\s*\.\s*open\b/],
  ["localStorage", /\blocalStorage\b/],
  ["sessionStorage", /\bsessionStorage\b/],
  ["indexedDB", /\bindexedDB\b/],
  ["cookie", /\bdocument\s*\.\s*cookie\b/],
  ["target blank", /_blank/],
];

function relative(key: string): string {
  return key.replace("../", "");
}

describe("source scan", () => {
  it("scans a plausible number of source files", () => {
    expect(Object.keys(sources).length).toBeGreaterThan(20);
  });

  it("holds no raw bidirectional control character", () => {
    // A raw control character can reorder how a line of source is displayed,
    // so source and tests name these characters with escapes, as this
    // pattern does.
    const raw = /[\u202a-\u202e\u2066-\u2069]/;
    const scanned = {
      ...sources,
      ...styles,
      ...pages,
      ...tests,
      "./security.test.ts": ownSource,
    };
    expect(Object.keys(tests).length).toBeGreaterThan(5);
    const offenders = Object.entries(scanned)
      .filter(([, text]) => raw.test(text))
      .map(([key]) => key);
    expect(offenders).toEqual([]);
  });

  it.each(FORBIDDEN)("uses no %s", (name, pattern) => {
    const offenders = Object.entries(sources)
      .filter(([, text]) => pattern.test(text))
      .map(([key]) => relative(key));
    expect(offenders, name).toEqual([]);
  });

  it("imports @tauri-apps/api only in src/ipc/client.ts", () => {
    const importers = Object.entries(sources)
      .filter(([, text]) => /["']@tauri-apps\//.test(text))
      .map(([key]) => relative(key));
    expect(importers).toEqual(["src/ipc/client.ts"]);
  });

  it("keeps approval commands out of the main window sources", () => {
    const main = Object.entries(sources).filter(
      ([key]) => key.startsWith("../src/main/") || key === "../src/main.ts",
    );
    expect(main.length).toBeGreaterThan(5);
    for (const [key, text] of main) {
      expect(text, key).not.toMatch(/approval_decide|approval_detail|approvals_client/);
    }
  });

  it("keeps chat_subscribe out of the approvals window and inside chat_events", () => {
    const users = Object.entries(sources)
      .filter(([, text]) => text.includes("chat_subscribe"))
      .map(([key]) => relative(key));
    expect(users.sort()).toEqual(["src/ipc/chat_events.ts", "src/mock/backend.ts"]);
  });

  it("keeps the approvals window free of main-window code", () => {
    const approvals = Object.entries(sources).filter(
      ([key]) => key.startsWith("../src/approvals/") || key === "../src/approvals.ts",
    );
    expect(approvals.length).toBeGreaterThan(3);
    for (const [key, text] of approvals) {
      expect(text, key).not.toMatch(/from\s+["'](\.\.?\/)+main(\/|["'])/);
      expect(text, key).not.toMatch(/\bmock\b/);
    }
  });

  it("loads no remote resources from CSS", () => {
    for (const [key, text] of Object.entries(styles)) {
      expect(text, key).not.toMatch(/https?:|url\(\s*["']?\/\//);
    }
  });
});

describe("HTML entry files", () => {
  it("finds both entries", () => {
    expect(Object.keys(pages).map(relative).sort()).toEqual(["approvals.html", "index.html"]);
  });

  it.each(Object.entries(pages))(
    "%s has no inline script or style and no remote URL",
    (_key, html) => {
      const scripts = Array.from(html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi));
      expect(scripts).toHaveLength(1);
      expect(scripts[0]?.[1]).toMatch(/\bsrc="\.\/src\/[a-z]+\.ts"/);
      expect(scripts[0]?.[2]?.trim()).toBe("");
      expect(html).not.toMatch(/<style\b/i);
      expect(html).not.toMatch(/\sstyle\s*=/i);
      expect(html).not.toMatch(/\son[a-z]+\s*=/i);
      expect(html).not.toMatch(/https?:/i);
      expect(html).not.toMatch(/target\s*=/i);
      expect(html).not.toMatch(/<a\b/i);
    },
  );
});
