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

  it("sets inline style only for section sizes, in src/ui/size_property.ts (ADR-0072, ADR-0101)", () => {
    // A write: a call on `.style`, an assignment to `.style` or one of its properties, or another
    // way to add style rules. Reading a computed style, as capture.ts does, is not one.
    const write =
      /\.style(\.[A-Za-z]+)?\s*(\(|=(?!=))|\.style\.[A-Za-z]+\s*\(|cssText|adoptedStyleSheets|CSSStyleSheet|setAttribute\(\s*["']style/;
    const styled = Object.entries(sources)
      .filter(([, text]) => write.test(text))
      .map(([key]) => relative(key));
    expect(styled).toEqual(["src/ui/size_property.ts"]);
    const sizes = sources["../src/ui/size_property.ts"] ?? "";
    const writes = sizes.match(/\.style\.[A-Za-z]+\([^)]*\)/g) ?? [];
    // One write of a checked name and a whole number of pixels, and one removal of a checked name.
    expect(writes).toEqual([
      // biome-ignore lint/suspicious/noTemplateCurlyInString: the expected source text, not a template.
      ".style.setProperty(name, `${pixels}px`)",
      ".style.removeProperty(name)",
    ]);
    expect(sizes).toMatch(/if \(!SIZE_PROPERTIES\.includes\(name\)\) \{\s*throw/);
    expect(sizes).toMatch(
      /if \(!Number\.isSafeInteger\(pixels\) \|\| pixels < 0 \|\| pixels > MAX_SECTION_SIZE\) \{\s*throw/,
    );
    // Every listed property is a size custom property.
    const listed = /SIZE_PROPERTIES = \[([^\]]*)\]/.exec(sizes)?.[1] ?? "";
    const names = [...listed.matchAll(/"([^"]*)"/g)].map((match) => match[1]);
    expect(names.length).toBeGreaterThan(0);
    for (const name of names) {
      expect(name).toMatch(/^--[a-z]+(-[a-z]+)*-(width|height)$/);
    }
    // The main window's columns still pass a clamped width for one of two sides.
    const layout = sources["../src/main/layout.ts"] ?? "";
    expect(layout).toMatch(/setSizeProperty\(element, `--\$\{state\.side\}-width`, state\.width\)/);
    expect(layout).toMatch(/type Side = "left" \| "right";/);
    expect(layout).toMatch(/state\.width = clamp\(Math\.round\(requested\)/);
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

  it("keeps the settings commands inside the settings client", () => {
    const users = Object.entries(sources)
      .filter(([, text]) =>
        /settings_(overview|choose_root|register_workspace|set_permissions|choose_package_cache|clear_package_cache)|settings_client/.test(
          text,
        ),
      )
      .map(([key]) => relative(key));
    expect(users.sort()).toEqual([
      "src/ipc/settings_client.ts",
      "src/mock/backend.ts",
      "src/settings/app.ts",
    ]);
  });

  it("keeps the settings window free of main, approvals, and chat code", () => {
    const settings = Object.entries(sources).filter(
      ([key]) => key.startsWith("../src/settings/") || key === "../src/settings.ts",
    );
    expect(settings.length).toBeGreaterThanOrEqual(2);
    for (const [key, text] of settings) {
      expect(text, key).not.toMatch(/from\s+["'](\.\.?\/)+(main|approvals)(\/|["'])/);
      expect(text, key).not.toMatch(/approvals_client|chat_events|\bapi\./);
      expect(text, key).not.toMatch(/\bmock\b/);
    }
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
    expect(Object.keys(styles).length).toBeGreaterThanOrEqual(4);
    for (const [key, text] of Object.entries(styles)) {
      // An empty string would mean Vitest stubbed the CSS and this scan checked nothing.
      expect(text.length, key).toBeGreaterThan(0);
      expect(text, key).not.toMatch(/https?:|url\(\s*["']?\/\//);
    }
  });
});

describe("HTML entry files", () => {
  it("finds all three entries", () => {
    expect(Object.keys(pages).map(relative).sort()).toEqual([
      "approvals.html",
      "index.html",
      "settings.html",
    ]);
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
