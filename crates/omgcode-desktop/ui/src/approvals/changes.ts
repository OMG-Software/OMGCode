import { append, el } from "../dom";
import type { ApprovalField } from "../ipc/types";
import {
  collapseUnchanged,
  type DiffLine,
  type DiffRow,
  diffLines,
  MAX_DIFF_LINES,
  splitLines,
} from "./diff";

export interface EditChange {
  oldText: string;
  newText: string;
}

export interface ChangeSplit {
  /** Edits in review order. Empty unless both texts of an edit are present. */
  edits: EditChange[];
  /** The content a create tool would write, or null. */
  created: string | null;
  /** Fields that are shown as a plain definition list. */
  rest: ApprovalField[];
}

const OLD_LABEL = /^Old text(?: (\d+))?$/;
const NEW_LABEL = /^New text(?: (\d+))?$/;
const CREATE_TOOL = "workspace_create_file";

/**
 * Separates the fields a diff can show from the others. An old text is paired with the new
 * text that has the same number (or none); a text without its pair stays a plain field.
 */
export function splitChanges(tool: string | null, fields: readonly ApprovalField[]): ChangeSplit {
  const news = new Map<string, ApprovalField>();
  for (const field of fields) {
    const match = NEW_LABEL.exec(field.label);
    if (match !== null && !news.has(match[1] ?? "")) {
      news.set(match[1] ?? "", field);
    }
  }
  const used = new Set<ApprovalField>();
  const edits: EditChange[] = [];
  for (const field of fields) {
    const match = OLD_LABEL.exec(field.label);
    const partner = match === null ? undefined : news.get(match[1] ?? "");
    if (match !== null && partner !== undefined && !used.has(partner)) {
      used.add(field);
      used.add(partner);
      edits.push({ oldText: field.value, newText: partner.value });
    }
  }
  let created: string | null = null;
  if (tool === CREATE_TOOL) {
    const content = fields.find((f) => f.label === "Content");
    if (content !== undefined) {
      used.add(content);
      created = content.value;
    }
  }
  return { edits, created, rest: fields.filter((f) => !used.has(f)) };
}

const PREFIX: Record<DiffLine["kind"], string> = { same: " ", add: "+", remove: "-" };

function diffRow(row: DiffRow): HTMLElement {
  if (row.kind === "gap") {
    return el("div", { class: "diff-gap", text: `${row.count} unchanged lines` });
  }
  return el(
    "div",
    { class: `diff-row diff-${row.kind}` },
    el("span", { class: "diff-mark", text: PREFIX[row.kind] }),
    el("span", { class: "diff-text", text: row.text }),
  );
}

function diffBox(rows: readonly DiffRow[]): HTMLElement {
  const box = el("div", { class: "diff", attrs: { role: "group", "aria-label": "Changes" } });
  append(box, ...rows.map(diffRow));
  return box;
}

function textBlock(label: string, value: string): HTMLElement {
  return el("div", { class: "text-block" }, el("h4", { text: label }), el("pre", { text: value }));
}

/** One edit as a line diff, or as the two texts when they are too large to compare. */
export function editView(edit: EditChange, title: string | null): HTMLElement {
  const lines = diffLines(edit.oldText, edit.newText);
  const heading = title === null ? null : el("h3", { class: "change-title", text: title });
  if (lines === null) {
    return el(
      "section",
      { class: "change" },
      heading,
      el("p", { class: "muted", text: "Too large to compare line by line." }),
      textBlock("Old text", edit.oldText),
      textBlock("New text", edit.newText),
    );
  }
  return el("section", { class: "change" }, heading, diffBox(collapseUnchanged(lines)));
}

/** New file content as added lines, or as one block when it has too many lines. */
export function createdView(content: string): HTMLElement {
  const lines = splitLines(content);
  if (lines.length > MAX_DIFF_LINES) {
    return el(
      "section",
      { class: "change" },
      el("h3", { class: "change-title", text: "New file" }),
      el("p", { class: "muted", text: "Too large to show line by line." }),
      textBlock("Content", content),
    );
  }
  const added: DiffLine[] = lines.map((text) => ({ kind: "add", text }));
  return el(
    "section",
    { class: "change" },
    el("h3", { class: "change-title", text: "New file" }),
    diffBox(added),
  );
}

/** The views for every change in a split, in order. */
export function changeViews(split: ChangeSplit): HTMLElement[] {
  const views = split.edits.map((edit, index) =>
    editView(
      edit,
      split.edits.length > 1 ? `Edit ${index + 1} of ${split.edits.length}` : "Changes",
    ),
  );
  if (split.created !== null) {
    views.push(createdView(split.created));
  }
  return views;
}
