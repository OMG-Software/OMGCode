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
/**
 * Most table cells in one reply, header cells and padded cells included. A table that would pass
 * it is shown as paragraph text, whole, so a short row cannot be multiplied into many cells, and
 * every later table in the reply is shown as text too.
 */
export const MAX_TABLE_CELLS = 10_000;
const MAX_LANGUAGE_CHARS = 32;
const MAX_LINK_DESTINATION_CHARS = 2048;

const HEADING_OPEN = /^ {0,3}(#{1,6})(?:[ \t]|$)/;
const THEMATIC_BREAK = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const BLOCK_QUOTE = /^ {0,3}> ?/;
const LIST_MARKER = /^([-*+]|(\d{1,9})([.)]))/;
const DELIMITER_CELL = /^:?-+:?$/;
const WHITESPACE = /\s/;
const WORD_CHAR = /[\p{L}\p{N}]/u;
const ASCII_PUNCTUATION = /[!-/:-@[-`{-~]/;
const ESCAPED_PUNCTUATION = /\\([!-/:-@[-`{-~])/g;

const HEADING_TAGS = ["h1", "h2", "h3", "h4", "h5", "h6"] as const;

type Lines = readonly string[];

/** What is left of the per-reply limits, shared by every nested block. */
interface BlockBudget {
  tableCells: number;
}

/**
 * Renders `source` as Markdown inside a `.bubble-text.md` container, or returns `null` when it
 * is too long to parse; the caller then shows it as plain text.
 */
export function renderMarkdown(source: string): HTMLElement | null {
  if (source.length > MARKDOWN_MAX_CHARS) {
    return null;
  }
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  return el(
    "div",
    { class: "bubble-text md" },
    ...parseBlocks(lines, 0, { tableCells: MAX_TABLE_CELLS }),
  );
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

/** The number of leading spaces, not tabs, before the first other character. */
function leadingSpaces(line: string): number {
  let count = 0;
  while (line.charAt(count) === " ") {
    count += 1;
  }
  return count;
}

// The line scanners below walk each line once, so no pattern can backtrack over it.
function fenceOpen(line: string): Fence | null {
  const indent = leadingSpaces(line);
  const ch = line.charAt(indent);
  if (indent > 3 || (ch !== "`" && ch !== "~")) {
    return null;
  }
  const length = runLength(line, indent, ch);
  if (length < 3) {
    return null;
  }
  const marker = ch.repeat(length);
  const info = line.slice(indent + length);
  // A backtick fence's info string cannot hold a backtick, so "```a```" stays inline code.
  if (marker.startsWith("`") && info.includes("`")) {
    return null;
  }
  return { indent, marker, info: info.trim() };
}

function isSpaceOrTab(ch: string): boolean {
  return ch === " " || ch === "\t";
}

/**
 * Whether `line` closes `fence`: at most three columns of indentation, a run of the fence's
 * character at least as long as its opener, then only spaces and tabs. Other whitespace, such as
 * a no-break space, is content, so it does not end the block.
 */
function isFenceClose(line: string, fence: Fence): boolean {
  if (indentWidth(line) > 3) {
    return false;
  }
  let start = 0;
  while (isSpaceOrTab(line.charAt(start))) {
    start += 1;
  }
  const run = runLength(line, start, fence.marker.charAt(0));
  if (run < fence.marker.length) {
    return false;
  }
  for (let k = start + run; k < line.length; k += 1) {
    if (!isSpaceOrTab(line.charAt(k))) {
      return false;
    }
  }
  return true;
}

/** The fence still open after `line`, given the fence open before it. */
function fenceAfter(open: Fence | null, line: string): Fence | null {
  if (open === null) {
    return fenceOpen(line);
  }
  return isFenceClose(line, open) ? null : open;
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
  const indent = leadingSpaces(line);
  // The marker pattern is anchored and bounded; the spacing and content are split off by a scan.
  const match = indent > 3 ? null : LIST_MARKER.exec(line.slice(indent));
  if (match === null || THEMATIC_BREAK.test(line)) {
    return null;
  }
  const marker = match[1] ?? "";
  const digits = match[2];
  const markerEnd = indent + marker.length;
  const after = line.slice(markerEnd);
  // The spacing is measured in columns, with a tab advancing to the next multiple of four, as
  // continuation lines are; the indentation before the marker holds only spaces.
  let spacingLength = 0;
  let column = markerEnd;
  while (isSpaceOrTab(after.charAt(spacingLength))) {
    column += after.charAt(spacingLength) === "\t" ? 4 - (column % 4) : 1;
    spacingLength += 1;
  }
  // A marker must be followed by whitespace or the end of the line.
  if (after !== "" && spacingLength === 0) {
    return null;
  }
  const rest = after.slice(spacingLength);
  const spacingWidth = column - markerEnd;
  // An empty item, or one whose content starts more than four columns out, takes one space.
  const contentIndent = rest === "" || spacingWidth > 4 ? markerEnd + 1 : column;
  return {
    ordered: digits !== undefined,
    kind: digits === undefined ? marker : (match[3] ?? "."),
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

function parseBlocks(lines: Lines, depth: number, budget: BlockBudget): Node[] {
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
      out.push(el("blockquote", {}, ...parseBlocks(body, depth + 1, budget)));
      continue;
    }

    const table = depth < MAX_BLOCK_DEPTH ? tableAt(lines, i, depth, budget) : null;
    if (table !== null) {
      flush();
      out.push(table.node);
      i = table.next;
      continue;
    }

    const item = depth < MAX_BLOCK_DEPTH ? listMarker(line) : null;
    if (item !== null && (paragraph.length === 0 || interruptsParagraph(line, depth))) {
      flush();
      const list = listAt(lines, i, depth, budget);
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

function listAt(
  lines: Lines,
  start: number,
  depth: number,
  budget: BlockBudget,
): { node: HTMLElement; next: number } {
  const first = listMarker(lineAt(lines, start));
  const items: HTMLElement[] = [];
  let i = start;
  while (first !== null && i < lines.length) {
    const marker = listMarker(lineAt(lines, i));
    if (marker === null || marker.ordered !== first.ordered || marker.kind !== first.kind) {
      break;
    }
    const body = [marker.first];
    // A fence open in the item's body holds every indented line until it closes, and an
    // unindented line ends the item rather than joining the code as a lazy continuation.
    let fence = fenceAfter(null, marker.first);
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
        const content = stripIndent(line, marker.contentIndent);
        body.push(content);
        fence = fenceAfter(fence, content);
      } else if (fence !== null || listMarker(line) !== null || interruptsParagraph(line, depth)) {
        break;
      } else {
        // A lazy continuation line of the item's paragraph.
        body.push(line.trimStart());
      }
      i += 1;
    }
    items.push(el("li", {}, ...parseBlocks(body, depth + 1, budget)));

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
  budget: BlockBudget,
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
  // Count the cells before building any, so a table over the budget is not cut short. The count
  // stops once it passes the budget, and a refused table spends the rest of it: otherwise each
  // following line of a long run of delimiter-like rows would start the same long count again.
  let end = start + 2;
  while (end < lines.length && (end - start - 1) * columns <= budget.tableCells) {
    const line = lineAt(lines, end);
    if (isBlank(line) || interruptsParagraph(line, depth)) {
      break;
    }
    end += 1;
  }
  const cells = (end - start - 1) * columns;
  if (cells > budget.tableCells) {
    budget.tableCells = 0;
    return null;
  }
  budget.tableCells -= cells;
  const head = el(
    "tr",
    {},
    ...header.map((cell, column) => tableCell("th", cell, alignments[column] ?? null)),
  );
  const rows: HTMLElement[] = [];
  for (let i = start + 2; i < end; i += 1) {
    const values = tableCells(lineAt(lines, i));
    const row = el("tr");
    for (let column = 0; column < columns; column += 1) {
      row.appendChild(tableCell("td", values[column] ?? "", alignments[column] ?? null));
    }
    rows.push(row);
  }
  const table = el(
    "table",
    {},
    el("thead", {}, head),
    rows.length === 0 ? null : el("tbody", {}, ...rows),
  );
  return { node: el("div", { class: "md-table" }, table), next: end };
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
 * Finds closing emphasis delimiters for one inline parse. A closer's validity depends only on its
 * own position, so the first valid closer at or after `from` is reusable for any later `from` up
 * to it. Each search then starts past the previous result and the total scan is linear per key.
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

/** Longest HTML-like tag, from `<` to `>`, kept whole as literal text. */
const MAX_TAG_CHARS = 1000;
const TAG_NAME_START = /[A-Za-z]/;
const TAG_NAME_CHAR = /[A-Za-z0-9-]/;

/** Where one inline parse finds its literal text, code spans, tags, and links. */
interface InlineScan {
  /**
   * 1 where a character is literal: escaped by a backslash, or inside a code span, an HTML-like
   * tag, or a link destination. No delimiter or bracket at a literal position matches.
   */
  literal: Uint8Array;
  /** For each `[` that is not literal, the index of its matching `]`, or -1. */
  brackets: Int32Array;
  /** For each `[` whose `]` is followed by a destination, the index of the destination's `)`. */
  destinations: Int32Array;
  /** At the first character of a code span or tag, the index just past its end; otherwise 0. */
  spans: Int32Array;
}

/**
 * Scans `source` once from the left, as CommonMark reads inline content: an escape, a code span,
 * or a tag is taken where it starts, and a link destination is read from the raw characters once
 * its `]` is found, so a backtick inside an address never opens a code span. `parseInline` then
 * follows this scan rather than finding the same things again, so the two cannot disagree. Every
 * lookup is precomputed or bounded, so the scan is close to linear in the source length.
 */
function scanInline(source: string): InlineScan {
  const n = source.length;
  const scan: InlineScan = {
    literal: new Uint8Array(n),
    brackets: new Int32Array(n).fill(-1),
    destinations: new Int32Array(n).fill(-1),
    spans: new Int32Array(n),
  };
  const runs = backtickRuns(source);
  const ends = destinationEnds(source);
  const open: number[] = [];
  let i = 0;
  while (i < n) {
    const ch = source.charAt(i);
    if (ch === "\\" && ASCII_PUNCTUATION.test(source.charAt(i + 1))) {
      scan.literal[i + 1] = 1;
      i += 2;
    } else if (ch === "`") {
      const run = runLength(source, i, "`");
      const close = codeSpanClose(runs, i, run);
      i = close === -1 ? i + run : markSpan(scan, i, close + run);
    } else if (ch === "<") {
      const close = tagEnd(source, i);
      i = close === -1 ? i + 1 : markSpan(scan, i, close + 1);
    } else if (ch === "[") {
      open.push(i);
      i += 1;
    } else if (ch === "]") {
      const at = open.pop();
      const end = at === undefined ? -1 : destinationEnd(source, ends, i + 1);
      if (at !== undefined) {
        scan.brackets[at] = i;
      }
      if (at !== undefined && end !== -1) {
        scan.destinations[at] = end;
        scan.literal.fill(1, i + 1, end + 1);
        i = end + 1;
      } else {
        i += 1;
      }
    } else {
      i += 1;
    }
  }
  return scan;
}

/** Marks `[start, end)` as one literal span and returns `end`. */
function markSpan(scan: InlineScan, start: number, end: number): number {
  scan.literal.fill(1, start, end);
  scan.spans[start] = end;
  return end;
}

/** The start of every maximal backtick run, grouped by run length, in ascending order. */
function backtickRuns(source: string): Map<number, number[]> {
  const runs = new Map<number, number[]>();
  let at = source.indexOf("`");
  while (at !== -1) {
    const length = runLength(source, at, "`");
    const starts = runs.get(length);
    if (starts === undefined) {
      runs.set(length, [at]);
    } else {
      starts.push(at);
    }
    at = source.indexOf("`", at + length);
  }
  return runs;
}

/**
 * The start of the backtick run that closes a code span opened by `run` backticks at `at`: the
 * first later maximal run of the same length, found by binary search, or -1.
 */
function codeSpanClose(runs: Map<number, number[]>, at: number, run: number): number {
  const starts = runs.get(run);
  if (starts === undefined) {
    return -1;
  }
  let low = 0;
  let high = starts.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if ((starts[middle] ?? 0) < at + run) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  return starts[low] ?? -1;
}

/**
 * For each position, the first unescaped `)` or line break at or after it, or -1. A `)` is
 * escaped when an odd number of backslashes stands right before it. Built from the right in one
 * pass, so a destination's end is a single lookup however many links a reply attempts.
 */
function destinationEnds(source: string): Int32Array {
  const ends = new Int32Array(source.length);
  let found = -1;
  for (let k = source.length - 1; k >= 0; k -= 1) {
    const ch = source.charAt(k);
    if (ch === "\n" || (ch === ")" && !escapedAt(source, k))) {
      found = k;
    }
    ends[k] = found;
  }
  return ends;
}

function escapedAt(source: string, at: number): boolean {
  let backslashes = 0;
  while (source.charAt(at - backslashes - 1) === "\\") {
    backslashes += 1;
  }
  return backslashes % 2 === 1;
}

/**
 * The index of the `)` that ends a link destination whose `(` is at `open`, or -1 when there is
 * no `(`, the destination crosses a line, or it is longer than `MAX_LINK_DESTINATION_CHARS`.
 */
function destinationEnd(source: string, ends: Int32Array, open: number): number {
  if (source.charAt(open) !== "(") {
    return -1;
  }
  const end = ends[open + 1] ?? -1;
  if (end === -1 || source.charAt(end) !== ")" || end - open - 1 > MAX_LINK_DESTINATION_CHARS) {
    return -1;
  }
  return end;
}

/**
 * The index of the `>` ending an HTML-like tag that starts at `at`: `<name>`, `</name>`, or a
 * name followed by whitespace or `/` and up to `MAX_TAG_CHARS` characters without `<`. The tag
 * is shown as text, but it is kept whole, so a delimiter inside it matches nothing outside it.
 * Each search stops at the next `<`, so the searches never overlap.
 */
function tagEnd(source: string, at: number): number {
  let k = at + 1;
  if (source.charAt(k) === "/") {
    k += 1;
  }
  if (!TAG_NAME_START.test(source.charAt(k))) {
    return -1;
  }
  while (TAG_NAME_CHAR.test(source.charAt(k))) {
    k += 1;
  }
  const next = source.charAt(k);
  if (next === ">") {
    return k;
  }
  if (next !== "/" && !isSpace(next)) {
    return -1;
  }
  const limit = Math.min(source.length, at + MAX_TAG_CHARS);
  for (; k < limit; k += 1) {
    const ch = source.charAt(k);
    if (ch === ">") {
      return k;
    }
    if (ch === "<") {
      return -1;
    }
  }
  return -1;
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
  const scan = scanInline(source);
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

    const spanEnd = scan.spans[i] ?? 0;
    if (ch === "`") {
      const run = runLength(source, i, "`");
      if (spanEnd === 0) {
        buffer += "`".repeat(run);
        i += run;
      } else {
        flush();
        out.push(el("code", { text: codeSpanText(source.slice(i + run, spanEnd - run)) }));
        i = spanEnd;
      }
      continue;
    }

    if (ch === "<" && spanEnd !== 0) {
      // An HTML-like tag is shown as the text it is.
      buffer += source.slice(i, spanEnd);
      i = spanEnd;
      continue;
    }

    if (ch === "[" || (ch === "!" && source.charAt(i + 1) === "[")) {
      const image = ch === "!";
      const link = linkAt(source, image ? i + 1 : i, scan);
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
      const span = emphasisAt(source, i, run, ch, scan.literal, closers);
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
  literal: Uint8Array,
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
      // An escaped marker, or one inside inline code, a tag, or an address, closes nothing.
      if (literal[k] === 1 || literal[k + strength - 1] === 1) {
        return false;
      }
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
  scan: InlineScan,
): { label: string; destination: string; next: number } | null {
  const close = scan.brackets[open] ?? -1;
  const end = scan.destinations[open] ?? -1;
  if (close === -1 || end === -1) {
    return null;
  }
  // A title after the destination is dropped.
  const destination =
    source
      .slice(close + 2, end)
      .trim()
      .split(/\s/, 1)[0] ?? "";
  const unwrapped =
    destination.startsWith("<") && destination.endsWith(">")
      ? destination.slice(1, -1)
      : destination;
  return {
    label: source.slice(open + 1, close),
    destination: unwrapped.replace(ESCAPED_PUNCTUATION, "$1"),
    next: end + 1,
  };
}

/**
 * A link or image as inert text: the label, then the destination in a muted span. The destination
 * is shown unless the label already reads exactly as it, so a label cannot pose as a different
 * address, and nothing is fetched.
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
  // Compare what the label shows, not its source: "[`x`](x)" shows "x", and "[*x*](*x*)" does not
  // show "*x*". The label shows overrides as markers and the destination holds them raw, so an
  // address with an override never matches and is always shown, through `text`.
  const shown = image ? label : (node.textContent ?? "");
  if (destination !== "" && destination !== shown) {
    node.appendChild(el("span", { class: "md-link-url", text: ` (${destination})` }));
  }
  return node;
}
