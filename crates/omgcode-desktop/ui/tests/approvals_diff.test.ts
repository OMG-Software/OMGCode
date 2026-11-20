import { describe, expect, it } from "vitest";
import { changeViews, createdView, editView, splitChanges } from "../src/approvals/changes";
import {
  COLLAPSE_AFTER,
  collapseUnchanged,
  type DiffLine,
  diffLines,
  MAX_DIFF_LINES,
  MAX_EDIT_DISTANCE,
  splitLines,
} from "../src/approvals/diff";
import { HOSTILE_IMG } from "./helpers";

function lines(count: number, prefix: string): string {
  return Array.from({ length: count }, (_, i) => `${prefix}${i}`).join("\n");
}

function kinds(result: DiffLine[] | null): string {
  const marks = { same: " ", add: "+", remove: "-" };
  return (result ?? []).map((l) => marks[l.kind]).join("");
}

function rebuild(result: DiffLine[], keep: "remove" | "add"): string[] {
  return result.filter((l) => l.kind === "same" || l.kind === keep).map((l) => l.text);
}

describe("diffLines", () => {
  it("splits text on line feeds and treats empty text as no lines", () => {
    expect(splitLines("")).toEqual([]);
    expect(splitLines("a\nb")).toEqual(["a", "b"]);
    expect(splitLines("a\n")).toEqual(["a", ""]);
  });

  it("marks identical text as unchanged", () => {
    expect(kinds(diffLines("a\nb\nc", "a\nb\nc"))).toBe("   ");
    expect(diffLines("", "")).toEqual([]);
  });

  it("marks a replaced line as removed then added", () => {
    expect(diffLines("a\nb\nc", "a\nx\nc")).toEqual([
      { kind: "same", text: "a" },
      { kind: "remove", text: "b" },
      { kind: "add", text: "x" },
      { kind: "same", text: "c" },
    ]);
  });

  it("handles pure insertions, pure deletions and empty sides", () => {
    expect(kinds(diffLines("", "a\nb"))).toBe("++");
    expect(kinds(diffLines("a\nb", ""))).toBe("--");
    expect(kinds(diffLines("a\nc", "a\nb\nc"))).toBe(" + ");
    expect(kinds(diffLines("a\nb\nc", "a\nc"))).toBe(" - ");
  });

  it("reproduces both sides from the script for many generated inputs", () => {
    let seed = 7;
    const next = (n: number): number => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed % n;
    };
    for (let round = 0; round < 200; round++) {
      const a = Array.from({ length: next(12) }, () => String(next(4))).join("\n");
      const b = Array.from({ length: next(12) }, () => String(next(4))).join("\n");
      const result = diffLines(a, b);
      expect(result).not.toBeNull();
      if (result !== null) {
        expect(rebuild(result, "remove")).toEqual(splitLines(a));
        expect(rebuild(result, "add")).toEqual(splitLines(b));
      }
    }
  });

  it("returns null past the line bound on either side", () => {
    expect(diffLines(lines(MAX_DIFF_LINES + 1, "x"), "a")).toBeNull();
    expect(diffLines("a", lines(MAX_DIFF_LINES + 1, "x"))).toBeNull();
    expect(diffLines(lines(MAX_DIFF_LINES, "x"), lines(MAX_DIFF_LINES, "x"))).not.toBeNull();
  });

  it("returns null past the edit-distance bound, and quickly", () => {
    const started = performance.now();
    const result = diffLines(lines(MAX_DIFF_LINES, "old"), lines(MAX_DIFF_LINES, "new"));
    expect(result).toBeNull();
    expect(MAX_EDIT_DISTANCE).toBeLessThan(2 * MAX_DIFF_LINES);
    expect(performance.now() - started).toBeLessThan(5000);
  });

  it("compares a large text with a few changes without giving up", () => {
    const a = lines(MAX_DIFF_LINES, "line");
    const b = a.replace("line10\n", "changed\n").replace("line1500\n", "");
    const result = diffLines(a, b);
    expect(result).not.toBeNull();
    expect((result ?? []).filter((l) => l.kind !== "same")).toHaveLength(3);
  });

  it("keeps hostile text as plain line text", () => {
    const result = diffLines(`a\n${HOSTILE_IMG}`, "a\nb");
    expect(result?.map((l) => l.text)).toEqual(["a", HOSTILE_IMG, "b"]);
  });
});

describe("collapseUnchanged", () => {
  const same = (n: number, p = "s"): DiffLine[] =>
    Array.from({ length: n }, (_, i) => ({ kind: "same", text: `${p}${i}` }));
  const change: DiffLine[] = [
    { kind: "remove", text: "old" },
    { kind: "add", text: "new" },
  ];

  it("keeps short runs whole", () => {
    const input = [...same(COLLAPSE_AFTER), ...change, ...same(COLLAPSE_AFTER)];
    expect(collapseUnchanged(input)).toEqual(input);
  });

  it("collapses a long run between changes to three lines of context each side", () => {
    const rows = collapseUnchanged([...change, ...same(20), ...change]);
    expect(rows.map((r) => r.kind)).toEqual([
      "remove",
      "add",
      "same",
      "same",
      "same",
      "gap",
      "same",
      "same",
      "same",
      "remove",
      "add",
    ]);
    expect(rows[5]).toEqual({ kind: "gap", count: 14 });
  });

  it("keeps context only beside the change at the start and end", () => {
    const rows = collapseUnchanged([...same(10, "head"), ...change, ...same(10, "tail")]);
    expect(rows[0]).toEqual({ kind: "gap", count: 7 });
    expect(rows.slice(1, 4).map((r) => (r.kind === "same" ? r.text : ""))).toEqual([
      "head7",
      "head8",
      "head9",
    ]);
    expect(rows[rows.length - 1]).toEqual({ kind: "gap", count: 7 });
  });

  it("collapses text with no change to one gap", () => {
    expect(collapseUnchanged(same(9))).toEqual([{ kind: "gap", count: 9 }]);
  });
});

describe("change views", () => {
  const field = (label: string, value: string) => ({ label, value, multiline: true });

  it("pairs numbered old and new texts and leaves other fields", () => {
    const split = splitChanges("workspace_multi_edit_file", [
      { label: "File", value: "f.rs", multiline: false },
      field("Old text 1", "a"),
      field("New text 1", "b"),
      field("Old text 2", "c"),
      field("New text 2", "d"),
    ]);
    expect(split.edits).toEqual([
      { oldText: "a", newText: "b" },
      { oldText: "c", newText: "d" },
    ]);
    expect(split.rest.map((f) => f.label)).toEqual(["File"]);
  });

  it("keeps an old text without its pair as a plain field", () => {
    const split = splitChanges("workspace_edit_file", [field("Old text", "a")]);
    expect(split.edits).toEqual([]);
    expect(split.rest).toHaveLength(1);
  });

  it("takes create content only for the create tool", () => {
    const content = [field("Content", "x")];
    expect(splitChanges("workspace_create_file", content).created).toBe("x");
    expect(splitChanges("workspace_run_command", content).created).toBeNull();
  });

  it("labels each edit of a multi-edit and names a single edit Changes", () => {
    const two = changeViews({
      edits: [
        { oldText: "a", newText: "b" },
        { oldText: "c", newText: "d" },
      ],
      created: null,
      rest: [],
    });
    expect(two.map((v) => v.querySelector("h3")?.textContent)).toEqual([
      "Edit 1 of 2",
      "Edit 2 of 2",
    ]);
    const one = changeViews({ edits: [{ oldText: "a", newText: "b" }], created: null, rest: [] });
    expect(one[0]?.querySelector("h3")?.textContent).toBe("Changes");
  });

  it("renders prefixes, classes, gaps and hostile text as text only", () => {
    const old = `${lines(12, "keep")}\n${HOSTILE_IMG}\n${lines(12, "tail")}`;
    const view = editView({ oldText: old, newText: old.replace(HOSTILE_IMG, "safe") }, null);
    const rowsOf = (cls: string): string[] =>
      Array.from(view.querySelectorAll(`.${cls}`)).map((n) => n.textContent ?? "");
    expect(rowsOf("diff-remove")).toEqual([`-${HOSTILE_IMG}`]);
    expect(rowsOf("diff-add")).toEqual(["+safe"]);
    expect(rowsOf("diff-gap")).toEqual(["9 unchanged lines", "9 unchanged lines"]);
    expect(view.querySelectorAll(".diff-same")).toHaveLength(6);
    expect(view.querySelector("img")).toBeNull();
    expect(view.querySelector("[style]")).toBeNull();
  });

  it("falls back to the old and new texts when too large", () => {
    const view = editView(
      { oldText: lines(MAX_DIFF_LINES + 1, "o"), newText: HOSTILE_IMG },
      "Edit 1 of 2",
    );
    expect(view.textContent).toContain("Too large to compare line by line.");
    expect(view.querySelector(".diff")).toBeNull();
    const blocks = Array.from(view.querySelectorAll("pre")).map((p) => p.textContent);
    expect(blocks[1]).toBe(HOSTILE_IMG);
    expect(view.querySelector("img")).toBeNull();
  });

  it("shows created content as added lines only", () => {
    const view = createdView(`x\n${HOSTILE_IMG}`);
    expect(view.querySelectorAll(".diff-add")).toHaveLength(2);
    expect(view.querySelector(".diff-remove")).toBeNull();
    expect(view.querySelector("img")).toBeNull();
  });
});
