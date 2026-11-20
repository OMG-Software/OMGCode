import { afterEach, describe, expect, it, vi } from "vitest";
import type { TranscriptEntry } from "../src/ipc/types";
import {
  MARKDOWN_MAX_CHARS,
  MAX_BLOCK_DEPTH,
  MAX_INLINE_DEPTH,
  MAX_TABLE_COLUMNS,
  renderMarkdown,
} from "../src/main/markdown";
import { renderEntry } from "../src/main/transcript";
import { HOSTILE_IMG } from "./helpers";

function md(source: string): HTMLElement {
  const node = renderMarkdown(source);
  if (node === null) {
    throw new Error("expected the source to render");
  }
  return node;
}

function only<T extends Element>(root: Element, selector: string): T {
  const found = root.querySelectorAll<T>(selector);
  expect(found.length, selector).toBe(1);
  const first = found[0];
  if (first === undefined) {
    throw new Error(selector);
  }
  return first;
}

function stubClipboard(writeText: (value: string) => Promise<void>): void {
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
}

const RLO = "\u202e";

const EXAMPLE = [
  "To run this web server, you can execute the following command in your terminal " +
    "(assuming you are in the same directory as `server.py`):",
  "",
  "```bash",
  "python3 server.py",
  "```",
].join("\n");

afterEach(() => {
  vi.useRealTimers();
  Reflect.deleteProperty(navigator, "clipboard");
});

describe("the reply from the request", () => {
  it("renders the paragraph with inline code and a fenced block with its language", () => {
    const root = md(EXAMPLE);
    const paragraph = only(root, "p");
    expect(paragraph.textContent).toContain("in your terminal");
    expect(only(paragraph, "code").textContent).toBe("server.py");

    const block = only(root, ".md-code");
    expect(only(block, ".md-code-lang").textContent).toBe("bash");
    expect(only(block, "pre > code").textContent).toBe("python3 server.py");
    expect(only<HTMLButtonElement>(block, "button.md-copy").type).toBe("button");
  });

  it("copies the block's text and reports it, then restores the label", async () => {
    vi.useFakeTimers();
    const writes: string[] = [];
    stubClipboard(async (value) => {
      writes.push(value);
    });
    const copy = only<HTMLButtonElement>(md(EXAMPLE), "button.md-copy");
    copy.click();
    await vi.waitFor(() => expect(copy.textContent).toBe("Copied"));
    expect(writes).toEqual(["python3 server.py"]);
    vi.advanceTimersByTime(1500);
    expect(copy.textContent).toBe("Copy");
  });

  it("says so when the clipboard refuses the write", async () => {
    stubClipboard(() => Promise.reject(new Error("denied")));
    const copy = only<HTMLButtonElement>(md(EXAMPLE), "button.md-copy");
    copy.click();
    await vi.waitFor(() => expect(copy.textContent).toBe("Copy failed"));
  });

  it("says so when no clipboard exists", async () => {
    const copy = only<HTMLButtonElement>(md(EXAMPLE), "button.md-copy");
    copy.click();
    await vi.waitFor(() => expect(copy.textContent).toBe("Copy failed"));
  });

  it("copies what is shown, with a bidirectional override as its visible marker", async () => {
    const writes: string[] = [];
    stubClipboard(async (value) => {
      writes.push(value);
    });
    const root = md(`\`\`\`sh\nrm -rf ${RLO}gpj.sh\n\`\`\``);
    const shown = only(root, "pre > code").textContent ?? "";
    expect(shown).toBe("rm -rf [U+202E]gpj.sh");
    only<HTMLButtonElement>(root, "button.md-copy").click();
    await vi.waitFor(() => expect(writes).toEqual([shown]));
  });
});

describe("blocks", () => {
  it("renders headings, rules, quotes, and paragraphs", () => {
    const root = md("# Title\n\n## Part ##\n\ntext\nmore\n\n---\n\n> quoted\n> line");
    expect(only(root, "h1").textContent).toBe("Title");
    expect(only(root, "h2").textContent).toBe("Part");
    expect(only(root, ":scope > p").textContent).toBe("text\nmore");
    expect(root.querySelectorAll("hr")).toHaveLength(1);
    expect(only(root, "blockquote").textContent).toBe("quoted\nline");
  });

  it("does not treat a hash without a space as a heading", () => {
    const root = md("#include <stdio.h>");
    expect(root.querySelector("h1")).toBeNull();
    expect(only(root, "p").textContent).toBe("#include <stdio.h>");
  });

  it("renders bullet, ordered, and nested lists", () => {
    const root = md("- one\n- two\n  - inner\n\n3. three\n4. four");
    const bullets = root.querySelectorAll(":scope > ul > li");
    expect(bullets).toHaveLength(2);
    expect(only(root, "ul ul > li").textContent).toBe("inner");
    const ordered = only(root, "ol");
    expect(ordered.getAttribute("start")).toBe("3");
    expect(ordered.querySelectorAll("li")).toHaveLength(2);
  });

  it("keeps a list open across blank lines and holds a code block in an item", () => {
    const root = md("1. first\n\n2. second\n\n   ```\n   code\n   ```\n");
    expect(only(root, "ol").querySelectorAll(":scope > li")).toHaveLength(2);
    expect(only(root, "li pre > code").textContent).toBe("code");
  });

  it("lets a list follow a paragraph line", () => {
    const root = md("Steps:\n1. a\n2. b");
    expect(only(root, ":scope > p").textContent).toBe("Steps:");
    expect(only(root, "ol").querySelectorAll("li")).toHaveLength(2);
  });

  it("keeps the content of an unclosed fence and of a longer fence", () => {
    expect(only(md("```\na\n\n# not a heading"), "pre > code").textContent).toBe(
      "a\n\n# not a heading",
    );
    const nested = md("````md\n```js\nx\n```\n````");
    expect(only(nested, "pre > code").textContent).toBe("```js\nx\n```");
  });

  it("keeps tabs and blank lines inside a code block", () => {
    const root = md("```make\nall:\n\tcc main.c\n\n\n```");
    expect(only(root, "pre > code").textContent).toBe("all:\n\tcc main.c\n\n");
  });

  it("labels a block without a language and shortens a long one", () => {
    expect(only(md("```\nx\n```"), ".md-code-lang").textContent).toBe("Code");
    const long = "a".repeat(100);
    expect(only(md(`\`\`\`${long}\nx\n\`\`\``), ".md-code-lang").textContent).toBe(
      `${"a".repeat(32)}…`,
    );
  });

  it("renders a table with alignment and pads short rows", () => {
    const root = md("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 |\n| x \\| y | `z` | w |");
    expect([...root.querySelectorAll("th")].map((th) => th.textContent)).toEqual(["a", "b", "c"]);
    const rows = root.querySelectorAll("tbody tr");
    expect(rows).toHaveLength(2);
    expect(rows[0]?.querySelectorAll("td")).toHaveLength(3);
    expect(rows[1]?.querySelector("td")?.textContent).toBe("x | y");
    expect(root.querySelector("th.md-align-center")?.textContent).toBe("b");
    expect(root.querySelector("th.md-align-right")?.textContent).toBe("c");
  });

  it("leaves a table wider than the limit as text", () => {
    const header = `|${"h|".repeat(MAX_TABLE_COLUMNS + 1)}`;
    const delimiter = `|${"-|".repeat(MAX_TABLE_COLUMNS + 1)}`;
    const root = md(`${header}\n${delimiter}`);
    expect(root.querySelector("table")).toBeNull();
  });
});

describe("inline content", () => {
  it("renders strong, emphasis, both, and strikethrough", () => {
    const root = md("**b** *i* ***bi*** ~~s~~ __u__ _e_");
    expect([...root.querySelectorAll("strong")].map((n) => n.textContent)).toEqual([
      "b",
      "bi",
      "u",
    ]);
    expect([...root.querySelectorAll("em")].map((n) => n.textContent)).toEqual(["i", "bi", "e"]);
    expect(only(root, "del").textContent).toBe("s");
  });

  it("nests emphasis inside emphasis", () => {
    const root = md("*a **b** c*");
    const em = only(root, "em");
    expect(em.textContent).toBe("a b c");
    expect(only(em, "strong").textContent).toBe("b");
  });

  it("leaves snake_case, lone stars, and escapes as text", () => {
    const root = md("call my_func_name, 2 * 3 * 4, \\*not em\\*, a ~ b");
    expect(root.querySelector("em, strong, del")).toBeNull();
    expect(root.textContent).toBe("call my_func_name, 2 * 3 * 4, *not em*, a ~ b");
  });

  it("keeps the contents of a code span literal", () => {
    const root = md("use `` a `*b*` c `` here");
    expect(only(root, "code").textContent).toBe("a `*b*` c");
    expect(root.querySelector("em")).toBeNull();
  });
});

describe("untrusted content stays inert", () => {
  it("shows raw HTML as text in every position", () => {
    const hostile = `${HOSTILE_IMG}<script>alert(1)</script>`;
    const root = md(
      [
        hostile,
        `# ${hostile}`,
        `- ${hostile}`,
        `> ${hostile}`,
        `\`${hostile}\``,
        `[${hostile}](${hostile})`,
        `| ${hostile} |\n| - |\n| ${hostile} |`,
        `\`\`\`html\n${hostile}\n\`\`\``,
      ].join("\n\n"),
    );
    expect(root.querySelector("img, script, iframe, a, object, embed, style")).toBeNull();
    for (const node of root.querySelectorAll("*")) {
      for (const name of node.getAttributeNames()) {
        expect(name === "class" || name === "type" || name === "aria-label", name).toBe(true);
      }
    }
    expect(root.textContent).toContain(HOSTILE_IMG);
  });

  it("shows a link's destination beside its text and never makes it live", () => {
    const root = md('see [the docs](https://example.com/docs "title") or [x](javascript:alert(1))');
    expect(root.querySelector("a")).toBeNull();
    const links = root.querySelectorAll(".md-link");
    expect(links).toHaveLength(2);
    expect(links[0]?.textContent).toBe("the docs (https://example.com/docs)");
    expect(links[1]?.querySelector(".md-link-url")?.textContent).toBe(" (javascript:alert(1)");
  });

  it("does not let a link's text pose as a different address", () => {
    const root = md("[https://bank.example](https://evil.example)");
    expect(root.textContent).toBe("https://bank.example (https://evil.example)");
  });

  it("describes an image instead of loading it", () => {
    const root = md("![a cat](https://example.com/cat.png)");
    expect(root.querySelector("img")).toBeNull();
    expect(only(root, ".md-image").textContent).toBe(
      "[image: a cat] (https://example.com/cat.png)",
    );
  });

  it("marks bidirectional overrides in text and code", () => {
    const root = md(`a${RLO}b \`c${RLO}d\``);
    expect(root.textContent).toBe("a[U+202E]b c[U+202E]d");
  });
});

describe("bounds", () => {
  it("does not parse a reply over the limit", () => {
    expect(renderMarkdown("a".repeat(MARKDOWN_MAX_CHARS + 1))).toBeNull();
    expect(renderMarkdown("a".repeat(MARKDOWN_MAX_CHARS))).not.toBeNull();
  });

  it("stops nesting block quotes and lists at the depth limit", () => {
    const quotes = md(`${">".repeat(50)} deep`);
    expect(quotes.querySelectorAll("blockquote")).toHaveLength(MAX_BLOCK_DEPTH);
    expect(quotes.textContent).toContain("deep");

    const items = Array.from({ length: 40 }, (_, n) => `${"  ".repeat(n)}- item ${n}`).join("\n");
    const lists = md(items);
    expect(lists.querySelectorAll("ul").length).toBeLessThanOrEqual(MAX_BLOCK_DEPTH);
    expect(lists.textContent).toContain("item 39");
  });

  it("stops nesting emphasis at the depth limit and keeps the text", () => {
    const depth = MAX_INLINE_DEPTH + 4;
    const source = `${"*a ".repeat(depth)}x${" b*".repeat(depth)}`;
    const root = md(source);
    expect(root.querySelectorAll("em").length).toBeLessThanOrEqual(MAX_INLINE_DEPTH);
    expect(root.textContent).toContain("x");
  });

  it.each([
    ["unmatched stars", "*a ".repeat(30_000)],
    ["unmatched underscores", "_a ".repeat(30_000)],
    ["unmatched brackets", "[".repeat(90_000)],
    ["unclosed links", "[a](".repeat(20_000)],
    ["backtick runs", Array.from({ length: 400 }, (_, n) => "`".repeat(n + 1)).join(" a ")],
    ["spaces before a heading close", `# ${" ".repeat(90_000)}x`],
    ["a long rule-like line", `${"- ".repeat(45_000)}x`],
    ["table-like rows", `a|b\n-|-\n${"|".repeat(90_000)}`],
  ])("parses %s in bounded time", (_name, source) => {
    expect(source.length).toBeLessThanOrEqual(MARKDOWN_MAX_CHARS);
    const started = performance.now();
    expect(renderMarkdown(source)).not.toBeNull();
    expect(performance.now() - started).toBeLessThan(3000);
  });
});

describe("transcript messages", () => {
  const base = { seq: 1, timestamp: "2026-01-02T10:00:00Z" };
  const message = (role: "user" | "assistant" | "tool", text: string): TranscriptEntry => ({
    ...base,
    kind: "message",
    role,
    source: "chat",
    text,
    call_id: null,
  });

  it("renders an assistant reply as Markdown", () => {
    const node = renderEntry(message("assistant", EXAMPLE));
    expect(node.querySelector(".bubble-text.md .md-code pre > code")?.textContent).toBe(
      "python3 server.py",
    );
  });

  it("shows the operator's and a tool's messages as written", () => {
    for (const role of ["user", "tool"] as const) {
      const node = renderEntry(message(role, "**bold** `code`"));
      expect(node.querySelector(".md, strong, code")).toBeNull();
      expect(node.querySelector(".bubble-text")?.textContent).toBe("**bold** `code`");
    }
  });

  it("shows a reply over the limit as plain text", () => {
    const long = `**${"a".repeat(MARKDOWN_MAX_CHARS)}**`;
    const node = renderEntry(message("assistant", long));
    expect(node.querySelector(".md, strong")).toBeNull();
    expect(node.querySelector(".bubble-text")?.textContent).toBe(long);
  });
});
