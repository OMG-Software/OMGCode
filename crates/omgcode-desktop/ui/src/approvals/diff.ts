/**
 * A pure, bounded line diff for the approvals review.
 *
 * Bound. Each side may hold at most `MAX_DIFF_LINES` (2,000) lines and the shortest edit
 * script may hold at most `MAX_EDIT_DISTANCE` (2,000) added or removed lines. Past either
 * bound `diffLines` returns `null` and the caller shows the unchanged old and new text.
 * The Myers search runs O((n + m) * d) steps for edit distance d <= 2,000, so it takes at
 * most about 8 million steps. It keeps one `Int32Array` of d + 1 entries per round, at most
 * about 2 million entries (8 MB), to rebuild the script. Lines are interned to integers
 * first so comparing two lines never costs their length.
 */

export const MAX_DIFF_LINES = 2000;
export const MAX_EDIT_DISTANCE = 2000;
/** A run of unchanged lines longer than this is collapsed. */
export const COLLAPSE_AFTER = 6;
/** Unchanged lines kept on each side of a change when a run is collapsed. */
export const CONTEXT_LINES = 3;

export type DiffKind = "same" | "add" | "remove";

export interface DiffLine {
  kind: DiffKind;
  text: string;
}

export interface DiffGap {
  kind: "gap";
  /** Unchanged lines hidden by the collapse. */
  count: number;
}

export type DiffRow = DiffLine | DiffGap;

/** Splits text on line feeds. The empty string has no lines; a trailing line feed adds an empty line. */
export function splitLines(value: string): string[] {
  return value === "" ? [] : value.split("\n");
}

function intern(lines: readonly string[], table: Map<string, number>): number[] {
  return lines.map((line) => {
    let id = table.get(line);
    if (id === undefined) {
      id = table.size;
      table.set(line, id);
    }
    return id;
  });
}

/** Myers shortest edit script over line ids, or null once the distance passes `limit`. */
function myers(a: readonly number[], b: readonly number[], limit: number): DiffKind[] | null {
  const n = a.length;
  const m = b.length;
  const max = Math.min(n + m, limit);
  const offset = max + 1;
  const v = new Int32Array(2 * max + 3);
  const trace: Int32Array[] = [];
  let found = -1;
  for (let d = 0; d <= max && found < 0; d++) {
    for (let k = -d; k <= d; k += 2) {
      let x: number;
      if (k === -d || (k !== d && (v[offset + k - 1] ?? 0) < (v[offset + k + 1] ?? 0))) {
        x = v[offset + k + 1] ?? 0;
      } else {
        x = (v[offset + k - 1] ?? 0) + 1;
      }
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[offset + k] = x;
      if (x >= n && y >= m) {
        found = d;
        break;
      }
    }
    if (found < 0) {
      trace.push(v.slice(offset - d, offset + d + 1));
    }
  }
  if (found < 0) {
    return null;
  }
  const ops: DiffKind[] = [];
  let x = n;
  let y = m;
  for (let d = found; d > 0; d--) {
    const prev = trace[d - 1];
    if (prev === undefined) {
      return null;
    }
    const at = (k: number): number => prev[k + d - 1] ?? 0;
    const k = x - y;
    const down = k === -d || (k !== d && at(k - 1) < at(k + 1));
    const prevK = down ? k + 1 : k - 1;
    const prevX = at(prevK);
    const startX = down ? prevX : prevX + 1;
    while (x > startX) {
      ops.push("same");
      x--;
      y--;
    }
    ops.push(down ? "add" : "remove");
    if (down) {
      y--;
    } else {
      x--;
    }
  }
  while (x > 0 && y > 0) {
    ops.push("same");
    x--;
    y--;
  }
  return ops.reverse();
}

/**
 * Compares two texts line by line. Returns null when either side passes `MAX_DIFF_LINES` or the
 * edit distance passes `MAX_EDIT_DISTANCE`. Removed lines come before added lines in a change.
 */
export function diffLines(oldText: string, newText: string): DiffLine[] | null {
  const oldLines = splitLines(oldText);
  const newLines = splitLines(newText);
  if (oldLines.length > MAX_DIFF_LINES || newLines.length > MAX_DIFF_LINES) {
    return null;
  }
  const table = new Map<string, number>();
  const a = intern(oldLines, table);
  const b = intern(newLines, table);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) {
    head++;
  }
  let tail = 0;
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  ) {
    tail++;
  }
  const ops = myers(
    a.slice(head, a.length - tail),
    b.slice(head, b.length - tail),
    MAX_EDIT_DISTANCE,
  );
  if (ops === null) {
    return null;
  }
  const out: DiffLine[] = [];
  for (let i = 0; i < head; i++) {
    out.push({ kind: "same", text: oldLines[i] ?? "" });
  }
  let x = head;
  let y = head;
  for (const op of ops) {
    if (op === "same") {
      out.push({ kind: "same", text: oldLines[x] ?? "" });
      x++;
      y++;
    } else if (op === "remove") {
      out.push({ kind: "remove", text: oldLines[x] ?? "" });
      x++;
    } else {
      out.push({ kind: "add", text: newLines[y] ?? "" });
      y++;
    }
  }
  for (let i = 0; i < tail; i++) {
    out.push({ kind: "same", text: oldLines[x + i] ?? "" });
  }
  return out;
}

/**
 * Replaces each run of more than `COLLAPSE_AFTER` unchanged lines with a gap row, keeping
 * `CONTEXT_LINES` lines next to a change. The start and end of the text have no change on
 * their outer side, so only the inner side keeps context.
 */
export function collapseUnchanged(lines: readonly DiffLine[]): DiffRow[] {
  const rows: DiffRow[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line === undefined) {
      break;
    }
    if (line.kind !== "same") {
      rows.push(line);
      i++;
      continue;
    }
    let end = i;
    while (end < lines.length && lines[end]?.kind === "same") {
      end++;
    }
    const run = lines.slice(i, end);
    if (run.length <= COLLAPSE_AFTER) {
      rows.push(...run);
    } else {
      const atStart = i === 0;
      const atEnd = end === lines.length;
      const before = atStart ? 0 : CONTEXT_LINES;
      const after = atEnd ? 0 : CONTEXT_LINES;
      rows.push(...run.slice(0, before));
      rows.push({ kind: "gap", count: run.length - before - after });
      rows.push(...run.slice(run.length - after));
    }
    i = end;
  }
  return rows;
}
