/**
 * Renders a recorded assistant reply as a subset of Markdown (ADR-0067).
 *
 * The reply is untrusted model output. The parser builds elements through `el` and text through
 * `text`, so no string ever becomes markup: raw HTML in a reply is shown as literal text. Links
 * and images are never live. A link shows its text followed by its destination as plain text, and
 * an image shows its description and source; nothing navigates and nothing loads.
 *
 * Supported: paragraphs, ATX headings, thematic breaks, block quotes, bullet and ordered lists
 * (nested by indentation), fenced code blocks with a copy button, GitHub-style tables, inline
 * code, strong, emphasis, strikethrough, and backslash escapes. Anything else stays text.
 *
 * The work is bounded. A reply over `MARKDOWN_MAX_CHARS` is not parsed. Block and inline nesting
 * have fixed depth limits, past which the remaining source is shown as text. Closing-delimiter
 * searches are cached so that a reply full of unmatched delimiters is still parsed in close to
 * linear time, and no regular expression here can backtrack over a whole line more than once.
 */
import { el, text } from "../dom";
import { copyText, feedbackButton } from "./clipboard";

/** Longest reply, in UTF-16 code units, rendered as Markdown. A longer one is shown as text. */
export const MARKDOWN_MAX_CHARS = 100_000;
/** Deepest nesting of block quotes and list items; deeper lines are shown as paragraph text. */
export const MAX_BLOCK_DEPTH = 6;
/** Deepest nesting of emphasis and link text; deeper source is shown as text. */
export const MAX_INLINE_DEPTH = 8;
/** Widest table; a wider header row is shown as paragraph text. */
export const MAX_TABLE_COLUMNS = 32;
const MAX_LANGUAGE_CHARS = 32;
const MAX_LINK_DESTINATION_CHARS = 2048;

const FENCE_OPEN = /^( {0,3})(`{3,}|~{3,})(.*)$/;
const HEADING_OPEN = /^ {0,3}(#{1,6})(?:[ \t]|$)/;
const THEMATIC_BREAK = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const BLOCK_QUOTE = /^ {0,3}> ?/;
const LIST_ITEM = /^( {0,3})([-*+]|(\d{1,9})([.)]))(?:([ \t]+)(.*))?$/;
const DELIMITER_CELL = /^:?-+:?$/;
const WHITESPACE = /\s/;
const WORD_CHAR = /[\p{L}\p{N}]/u;
const ASCII_PUNCTUATION = /[!-/:-@[-`{-~]/;

const HEADING_TAGS = ["h1", "h2", "h3", "h4", "h5", "h6"] as const;

type Lines = readonly string[];

/**
 * Renders `source` as Markdown inside a `.bubble-text.md` container, or returns `null` when it
 * is too long to parse; the caller then shows it as plain text.
 */
export function renderMarkdown(source: string): HTMLElement | null {
  if (source.length > MARKDOWN_MAX_CHARS) {
    return null;
  }
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  return el("div", { class: "bubble-text md" }, ...parseBlocks(lines, 0));
}

// ---------------------------------------------------------------------------------------------
// Blocks

function lineAt(lines: Lines, index: number): string {
  return lines[index] ?? "";
}

function isBlank(line: string): boolean {
  return line.trim() === "";
}

/** Leading indentation in columns, with a tab advancing to the next multiple of four. */
function indentWidth(line: string): number {
  let column = 0;
  for (const ch of line) {
    if (ch === " ") {
      column += 1;
    } else if (ch === "\t") {
      column += 4 - (column % 4);
    } else {
      break;
    }
  }
  return column;
}

/** Removes up to `width` columns of leading indentation. */
function stripIndent(line: string, width: number): string {
  let column = 0;
  let index = 0;
  while (index < line.length && column < width) {
    const ch = line.charAt(index);
    if (ch === " ") {
      column += 1;
    } else if (ch === "\t") {
      column += 4 - (column % 4);
    } else {
      break;
    }
    index += 1;
  }
  // A tab that crossed the boundary leaves its remaining columns as spaces.
  return " ".repeat(Math.max(0, column - width)) + line.slice(index);
}

interface Fence {
  indent: number;
  marker: string;
  info: string;
}

function fenceOpen(line: string): Fence | null {
  const match = FENCE_OPEN.exec(line);
  if (match === null) {
    return null;
  }
  const marker = match[2] ?? "";
  const info = match[3] ?? "";
  // A backtick fence's info string cannot hold a backtick, so "```a```" stays inline code.
  if (marker.startsWith("`") && info.includes("`")) {
    return null;
  }
  return { indent: (match[1] ?? "").length, marker, info: info.trim() };
}

function isFenceClose(line: string, fence: Fence): boolean {
  if (indentWidth(line) > 3) {
    return false;
  }
  const body = line.trim();
  const ch = fence.marker.charAt(0);
  let run = 0;
  while (body.charAt(run) === ch) {
    run += 1;
  }
  return run >= fence.marker.length && run === body.length;
}

interface ListMarker {
  ordered: boolean;
  /** The bullet character, or the ordered delimiter `.` or `)`. */
  kind: string;
  start: number;
  contentIndent: number;
  first: string;
}

function listMarker(line: string): ListMarker | null {
  const match = LIST_ITEM.exec(line);
  if (match === null || THEMATIC_BREAK.test(line)) {
    return null;
  }
  const indent = (match[1] ?? "").length;
  const marker = match[2] ?? "";
  const digits = match[3];
  const spacing = match[5] ?? "";
  const rest = match[6] ?? "";
  const markerEnd = indent + marker.length;
  // An empty item, or one whose content starts more than four columns out, takes one space.
  const contentIndent =
    rest === "" || spacing.length > 4 ? markerEnd + 1 : markerEnd + spacing.length;
  return {
    ordered: digits !== undefined,
    kind: digits === undefined ? marker : (match[4] ?? "."),
    start: digits === undefined ? 1 : Number.parseInt(digits, 10),
    contentIndent,
    first: rest,
  };
}

/** Whether `line` begins a block that ends a paragraph. */
function interruptsParagraph(line: string, depth: number): boolean {
  if (
    fenceOpen(line) !== null ||
    HEADING_OPEN.test(line) ||
    THEMATIC_BREAK.test(line) ||
    (depth < MAX_BLOCK_DEPTH && BLOCK_QUOTE.test(line))
  ) {
    return true;
  }
  const item = depth < MAX_BLOCK_DEPTH ? listMarker(line) : null;
  // As in CommonMark, only a non-empty item, and for an ordered list one numbered 1, interrupts.
  return item !== null && item.first.trim() !== "" && (!item.ordered || item.start === 1);
}

function parseBlocks(lines: Lines, depth: number): Node[] {
  const out: Node[] = [];
  let paragraph: string[] = [];
  const flush = (): void => {
    if (paragraph.length > 0) {
      out.push(el("p", {}, ...parseInline(paragraph.join("\n"), 0)));
      paragraph = [];
    }
  };

  let i = 0;
  while (i < lines.length) {
    const line = lineAt(lines, i);
    if (isBlank(line)) {
      flush();
      i += 1;
      continue;
    }

    const fence = fenceOpen(line);
    if (fence !== null) {
      flush();
      const body: string[] = [];
      i += 1;
      // An unclosed fence runs to the end of the reply, as in CommonMark.
      while (i < lines.length && !isFenceClose(lineAt(lines, i), fence)) {
        body.push(stripIndent(lineAt(lines, i), fence.indent));
        i += 1;
      }
      i += 1;
      out.push(codeBlock(fence.info, body.join("\n")));
      continue;
    }

    const heading = HEADING_OPEN.exec(line);
    if (heading !== null) {
      flush();
      const level = (heading[1] ?? "#").length;
      const tag = HEADING_TAGS[level - 1] ?? "h6";
      out.push(
        el(tag, { class: "md-heading" }, ...parseInline(headingText(line, heading[0].length), 0)),
      );
      i += 1;
      continue;
    }

    if (THEMATIC_BREAK.test(line)) {
      flush();
      out.push(el("hr"));
      i += 1;
      continue;
    }

    if (depth < MAX_BLOCK_DEPTH && BLOCK_QUOTE.test(line)) {
      flush();
      const body: string[] = [];
      while (i < lines.length && BLOCK_QUOTE.test(lineAt(lines, i))) {
        body.push(lineAt(lines, i).replace(BLOCK_QUOTE, ""));
        i += 1;
      }
      out.push(el("blockquote", {}, ...parseBlocks(body, depth + 1)));
      continue;
    }

    const table = depth < MAX_BLOCK_DEPTH ? tableAt(lines, i, depth) : null;
    if (table !== null) {
      flush();
      out.push(table.node);
      i = table.next;
      continue;
    }

    const item = depth < MAX_BLOCK_DEPTH ? listMarker(line) : null;
    if (item !== null && (paragraph.length === 0 || interruptsParagraph(line, depth))) {
      flush();
      const list = listAt(lines, i, depth);
      out.push(list.node);
      i = list.next;
      continue;
    }

    paragraph.push(line.trimStart());
    i += 1;
  }
  flush();
  return out;
}

/** The text of an ATX heading line after its opening marker, without a closing `#` run. */
function headingText(line: string, openLength: number): string {
  const body = line.slice(openLength).trim();
  let end = body.length;
  while (end > 0 && body.charAt(end - 1) === "#") {
    end -= 1;
  }
  if (end === body.length) {
    return body;
  }
  if (end === 0) {
    return "";
  }
  const before = body.charAt(end - 1);
  return before === " " || before === "\t" ? body.slice(0, end).trimEnd() : body;
}

function listAt(lines: Lines, start: number, depth: number): { node: HTMLElement; next: number } {
  const first = listMarker(lineAt(lines, start));
  const items: HTMLElement[] = [];
  let i = start;
  while (first !== null && i < lines.length) {
    const marker = listMarker(lineAt(lines, i));
    if (marker === null || marker.ordered !== first.ordered || marker.kind !== first.kind) {
      break;
    }
    const body = [marker.first];
    i += 1;
    while (i < lines.length) {
      const line = lineAt(lines, i);
      if (isBlank(line)) {
        // A blank line stays in the item only when indented content follows it.
        let next = i + 1;
        while (next < lines.length && isBlank(lineAt(lines, next))) {
          next += 1;
        }
        if (next < lines.length && indentWidth(lineAt(lines, next)) >= marker.contentIndent) {
          for (; i < next; i += 1) {
            body.push("");
          }
          continue;
        }
        break;
      }
      if (indentWidth(line) >= marker.contentIndent) {
        body.push(stripIndent(line, marker.contentIndent));
      } else if (listMarker(line) !== null || interruptsParagraph(line, depth)) {
        break;
      } else {
        // A lazy continuation line of the item's paragraph.
        body.push(line.trimStart());
      }
      i += 1;
    }
    items.push(el("li", {}, ...parseBlocks(body, depth + 1)));

    // Blank lines between items of the same list keep the list open.
    let next = i;
    while (next < lines.length && isBlank(lineAt(lines, next))) {
      next += 1;
    }
    const following = next < lines.length ? listMarker(lineAt(lines, next)) : null;
    if (
      following === null ||
      following.ordered !== first.ordered ||
      following.kind !== first.kind
    ) {
      break;
    }
    i = next;
  }

  if (first === null || !first.ordered) {
    return { node: el("ul", {}, ...items), next: i };
  }
  const attrs: Record<string, string> = first.start === 1 ? {} : { start: String(first.start) };
  return { node: el("ol", { attrs }, ...items), next: i };
}

/** Splits a table row on unescaped pipes, without its outer pipes. */
function tableCells(line: string): string[] {
  let body = line.trim();
  if (body.startsWith("|")) {
    body = body.slice(1);
  }
  if (body.endsWith("|") && !body.endsWith("\\|")) {
    body = body.slice(0, -1);
  }
  const cells: string[] = [];
  let current = "";
  for (let k = 0; k < body.length; k += 1) {
    const ch = body.charAt(k);
    if (ch === "\\" && body.charAt(k + 1) === "|") {
      current += "|";
      k += 1;
    } else if (ch === "|") {
      cells.push(current.trim());
      current = "";
    } else {
      current += ch;
    }
  }
  cells.push(current.trim());
  return cells;
}

type Alignment = "left" | "right" | "center" | null;

function delimiterRow(line: string, columns: number): Alignment[] | null {
  if (!line.includes("|") || indentWidth(line) > 3) {
    return null;
  }
  const cells = tableCells(line);
  if (cells.length !== columns || !cells.every((cell) => DELIMITER_CELL.test(cell))) {
    return null;
  }
  return cells.map((cell) => {
    const left = cell.startsWith(":");
    const right = cell.endsWith(":");
    if (left && right) {
      return "center";
    }
    if (right) {
      return "right";
    }
    return left ? "left" : null;
  });
}

function tableCell(tag: "th" | "td", source: string, align: Alignment): HTMLElement {
  const options = align === null ? {} : { class: `md-align-${align}` };
  return el(tag, options, ...parseInline(source, 0));
}

function tableAt(
  lines: Lines,
  start: number,
  depth: number,
): { node: HTMLElement; next: number } | null {
  const headerLine = lineAt(lines, start);
  if (!headerLine.includes("|") || start + 1 >= lines.length) {
    return null;
  }
  const header = tableCells(headerLine);
  if (header.length > MAX_TABLE_COLUMNS) {
    return null;
  }
  const alignments = delimiterRow(lineAt(lines, start + 1), header.length);
  if (alignments === null) {
    return null;
  }
  const columns = header.length;
  const head = el(
    "tr",
    {},
    ...header.map((cell, column) => tableCell("th", cell, alignments[column] ?? null)),
  );
  const rows: HTMLElement[] = [];
  let i = start + 2;
  while (i < lines.length) {
    const line = lineAt(lines, i);
    if (isBlank(line) || interruptsParagraph(line, depth)) {
      break;
    }
    const cells = tableCells(line);
    const row = el("tr");
    for (let column = 0; column < columns; column += 1) {
      row.appendChild(tableCell("td", cells[column] ?? "", alignments[column] ?? null));
    }
    rows.push(row);
    i += 1;
  }
  const table = el(
    "table",
    {},
    el("thead", {}, head),
    rows.length === 0 ? null : el("tbody", {}, ...rows),
  );
  return { node: el("div", { class: "md-table" }, table), next: i };
}

function codeBlock(info: string, content: string): HTMLElement {
  const language = info.split(/[ \t]/, 1)[0] ?? "";
  const label =
    language === ""
      ? "Code"
      : language.length > MAX_LANGUAGE_CHARS
        ? `${language.slice(0, MAX_LANGUAGE_CHARS)}…`
        : language;
  const code = el("code", { text: content });
  // Copy what the block shows. Bidirectional overrides were already replaced by visible
  // markers, so the clipboard cannot hold text that reads differently from the screen.
  const copy = feedbackButton("Copy", "Copied", () => copyText(code.textContent ?? ""), {
    class: "md-copy",
    attrs: { "aria-label": `Copy ${label} block` },
  });
  return el(
    "div",
    { class: "md-code" },
    el("div", { class: "md-code-head" }, el("span", { class: "md-code-lang", text: label }), copy),
    el("pre", {}, code),
  );
}

// ---------------------------------------------------------------------------------------------
// Inline content

/**
 * Finds closing delimiters for one inline parse. A closer's validity depends only on its own
 * position, so the first valid closer at or after `from` is reusable for any later `from` up to
 * it. Each search then starts past the previous result and the total scan is linear per key.
 */
class CloserIndex {
  private readonly found = new Map<string, { from: number; at: number }>();

  constructor(private readonly source: string) {}

  find(delimiter: string, from: number, valid: (at: number) => boolean): number {
    const cached = this.found.get(delimiter);
    if (cached !== undefined && from >= cached.from && (cached.at === -1 || cached.at >= from)) {
      return cached.at;
    }
    let at = this.source.indexOf(delimiter, from);
    while (at !== -1 && !valid(at)) {
      at = this.source.indexOf(delimiter, at + 1);
    }
    this.found.set(delimiter, { from, at });
    return at;
  }
}

/** For each unescaped `[`, the index of its matching `]`, or -1. */
function bracketPairs(source: string): Int32Array {
  const pairs = new Int32Array(source.length).fill(-1);
  const open: number[] = [];
  for (let k = 0; k < source.length; k += 1) {
    const ch = source.charAt(k);
    if (ch === "\\") {
      k += 1;
    } else if (ch === "[") {
      open.push(k);
    } else if (ch === "]") {
      const at = open.pop();
      if (at !== undefined) {
        pairs[at] = k;
      }
    }
  }
  return pairs;
}

function isSpace(ch: string): boolean {
  return ch === "" || WHITESPACE.test(ch);
}

function isWordChar(ch: string): boolean {
  return ch !== "" && WORD_CHAR.test(ch);
}

function runLength(source: string, at: number, ch: string): number {
  let end = at;
  while (source.charAt(end) === ch) {
    end += 1;
  }
  return end - at;
}

function parseInline(source: string, depth: number): Node[] {
  if (depth >= MAX_INLINE_DEPTH) {
    return [text(source)];
  }
  const out: Node[] = [];
  const closers = new CloserIndex(source);
  const brackets = bracketPairs(source);
  let buffer = "";
  const flush = (): void => {
    if (buffer !== "") {
      out.push(text(buffer));
      buffer = "";
    }
  };

  let i = 0;
  while (i < source.length) {
    const ch = source.charAt(i);

    if (ch === "\\" && ASCII_PUNCTUATION.test(source.charAt(i + 1))) {
      buffer += source.charAt(i + 1);
      i += 2;
      continue;
    }

    if (ch === "`") {
      const run = runLength(source, i, "`");
      const fence = "`".repeat(run);
      const close = closers.find(
        fence,
        i + run,
        (at) => source.charAt(at - 1) !== "`" && source.charAt(at + run) !== "`",
      );
      if (close === -1) {
        buffer += fence;
      } else {
        flush();
        out.push(el("code", { text: codeSpanText(source.slice(i + run, close)) }));
      }
      i = close === -1 ? i + run : close + run;
      continue;
    }

    if (ch === "[" || (ch === "!" && source.charAt(i + 1) === "[")) {
      const image = ch === "!";
      const link = linkAt(source, image ? i + 1 : i, brackets, closers);
      if (link !== null) {
        flush();
        out.push(linkNode(link.label, link.destination, image, depth));
        i = link.next;
      } else {
        buffer += ch;
        i += 1;
      }
      continue;
    }

    if (ch === "*" || ch === "_" || ch === "~") {
      const run = runLength(source, i, ch);
      const span = emphasisAt(source, i, run, ch, closers);
      if (span === null) {
        buffer += ch.repeat(run);
        i += run;
      } else {
        flush();
        out.push(
          emphasisNode(span.strength, ch, source.slice(i + span.strength, span.close), depth),
        );
        i = span.close + span.strength;
      }
      continue;
    }

    buffer += ch;
    i += 1;
  }
  flush();
  return out;
}

function codeSpanText(raw: string): string {
  const flat = raw.replace(/\n/g, " ");
  const padded = flat.length >= 2 && flat.startsWith(" ") && flat.endsWith(" ");
  return padded && flat.trim() !== "" ? flat.slice(1, -1) : flat;
}

function emphasisAt(
  source: string,
  at: number,
  run: number,
  ch: string,
  closers: CloserIndex,
): { strength: number; close: number } | null {
  const after = source.charAt(at + run);
  if (isSpace(after)) {
    return null;
  }
  // An underscore inside a word, as in snake_case, is not emphasis.
  if (ch === "_" && isWordChar(source.charAt(at - 1))) {
    return null;
  }
  const strengths = ch === "~" ? (run >= 2 ? [2] : []) : [3, 2, 1].filter((n) => n <= run);
  for (const strength of strengths) {
    const delimiter = ch.repeat(strength);
    const close = closers.find(delimiter, at + run, (k) => {
      if (isSpace(source.charAt(k - 1)) || source.charAt(k - 1) === ch) {
        return false;
      }
      const next = source.charAt(k + strength);
      return next !== ch && !(ch === "_" && isWordChar(next));
    });
    if (close !== -1) {
      return { strength, close };
    }
  }
  return null;
}

function emphasisNode(strength: number, ch: string, inner: string, depth: number): HTMLElement {
  const children = parseInline(inner, depth + 1);
  if (ch === "~") {
    return el("del", {}, ...children);
  }
  if (strength === 3) {
    return el("em", {}, el("strong", {}, ...children));
  }
  return strength === 2 ? el("strong", {}, ...children) : el("em", {}, ...children);
}

function linkAt(
  source: string,
  open: number,
  brackets: Int32Array,
  closers: CloserIndex,
): { label: string; destination: string; next: number } | null {
  const close = brackets[open] ?? -1;
  if (close === -1 || source.charAt(close + 1) !== "(") {
    return null;
  }
  const end = closers.find(")", close + 2, () => true);
  if (end === -1) {
    return null;
  }
  const inside = source.slice(close + 2, end);
  if (inside.includes("\n") || inside.length > MAX_LINK_DESTINATION_CHARS) {
    return null;
  }
  // A title after the destination is dropped.
  const destination = inside.trim().split(/\s/, 1)[0] ?? "";
  const unwrapped =
    destination.startsWith("<") && destination.endsWith(">")
      ? destination.slice(1, -1)
      : destination;
  return { label: source.slice(open + 1, close), destination: unwrapped, next: end + 1 };
}

/**
 * A link or image as inert text: the label, then the destination in a muted span. The destination
 * is always shown, so a label cannot pose as a different address, and nothing is fetched.
 */
function linkNode(label: string, destination: string, image: boolean, depth: number): HTMLElement {
  const node = el("span", { class: image ? "md-image" : "md-link" });
  if (image) {
    node.appendChild(text(label === "" ? "[image]" : `[image: ${label}]`));
  } else {
    for (const child of parseInline(label, depth + 1)) {
      node.appendChild(child);
    }
  }
  if (destination !== "" && destination !== label) {
    node.appendChild(el("span", { class: "md-link-url", text: ` (${destination})` }));
  }
  return node;
}
