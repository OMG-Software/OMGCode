import { el } from "../dom";

/** A definition list of display-safe values. */
export function facts(pairs: Array<[string, string]>): HTMLElement {
  const list = el("dl", { class: "facts" });
  for (const [term, value] of pairs) {
    list.append(el("dt", { text: term }), el("dd", { text: value }));
  }
  return list;
}

/** The heading and one line of help that open a section. */
export function sectionHead(title: string, help: string): HTMLElement[] {
  return [el("h2", { text: title }), el("p", { class: "section-help", text: help })];
}

/** A one-line empty state. */
export function emptyLine(text: string): HTMLElement {
  return el("p", { class: "empty", text });
}
