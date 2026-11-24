/**
 * Line icons drawn as SVG elements through the DOM, never parsed from markup. Each icon is a
 * fixed list of shapes defined here; no caller-supplied value reaches an attribute. Icons are
 * decorative (`aria-hidden`): the button that holds one carries the accessible name.
 */

import { neutralise } from "../dom";

const SVG_NS = "http://www.w3.org/2000/svg";

type Shape = readonly [tag: "path" | "circle" | "rect", attrs: Readonly<Record<string, string>>];

const FRAME: Shape = ["rect", { x: "3", y: "4", width: "18", height: "16", rx: "2" }];

const ICONS = {
  /** A frame with a left column: the sessions panel. */
  panelLeft: [FRAME, ["path", { d: "M9 4v16" }]],
  /** A frame with a right column: the terminal panel. */
  panelRight: [FRAME, ["path", { d: "M15 4v16" }]],
  /** Two sliders: the provider and model choice. */
  model: [
    ["path", { d: "M4 8h9M19 8h1M4 16h1M11 16h9" }],
    ["circle", { cx: "16", cy: "8", r: "2.5" }],
    ["circle", { cx: "8", cy: "16", r: "2.5" }],
  ],
  plus: [["path", { d: "M12 5v14M5 12h14" }]],
  search: [
    ["circle", { cx: "11", cy: "11", r: "6.5" }],
    ["path", { d: "M16 16l4.5 4.5" }],
  ],
  /** A speech bubble: the session list. */
  sessions: [["path", { d: "M4 5h16v11H10l-6 4z" }]],
  /** A shield with a tick: the approvals window. */
  approvals: [
    ["path", { d: "M12 3l8 3v6c0 4.6-3.4 7.9-8 9-4.6-1.1-8-4.4-8-9V6z" }],
    ["path", { d: "M8.5 12l2.5 2.5 4.5-5" }],
  ],
  /** Three sliders: the settings window. */
  settings: [
    ["path", { d: "M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1" }],
    ["circle", { cx: "15", cy: "6", r: "2" }],
    ["circle", { cx: "9", cy: "12", r: "2" }],
    ["circle", { cx: "17", cy: "18", r: "2" }],
  ],
  /** An eight-tooth gear: the composer's chat settings menu. */
  gear: [
    [
      "path",
      {
        d: "M19.2 9.8L21.9 10.3L21.9 13.7L19.2 14.2L18.6 15.5L20.2 17.8L17.8 20.2L15.5 18.6L14.2 19.2L13.7 21.9L10.3 21.9L9.8 19.2L8.5 18.6L6.2 20.2L3.8 17.8L5.4 15.5L4.8 14.2L2.1 13.7L2.1 10.3L4.8 9.8L5.4 8.5L3.8 6.2L6.2 3.8L8.5 5.4L9.8 4.8L10.3 2.1L13.7 2.1L14.2 4.8L15.5 5.4L17.8 3.8L20.2 6.2L18.6 8.5z",
      },
    ],
    ["circle", { cx: "12", cy: "12", r: "3" }],
  ],
  /** A jigsaw piece with a top knob and two side notches: the composer's skills menu. */
  skills: [
    ["path", { d: "M4 6h6V4a2 2 0 0 1 4 0v2h6v4h-2a2 2 0 0 0 0 4h2v6H4v-6h2a2 2 0 0 0 0-4H4z" }],
  ],
  terminal: [FRAME, ["path", { d: "M7 9l3 3-3 3M13 15h4" }]],
  /** A play triangle in a circle: the session's subprocesses (ADR-0086). */
  processes: [
    ["circle", { cx: "12", cy: "12", r: "9" }],
    ["path", { d: "M10 8.5l5 3.5-5 3.5z" }],
  ],
  /** A frame with an M and a down arrow: copy the reply's Markdown source. */
  markdown: [
    ["rect", { x: "2", y: "5", width: "20", height: "14", rx: "2" }],
    ["path", { d: "M6 15V9l3 3 3-3v6M17 9v6M15 13l2 2 2-2" }],
  ],
  /** Two overlapping sheets: copy the reply as plain text. */
  copy: [
    ["rect", { x: "9", y: "9", width: "11", height: "11", rx: "2" }],
    ["path", { d: "M5 15V6a2 2 0 0 1 2-2h8" }],
  ],
  /** A picture frame: copy the reply as an image. */
  image: [FRAME, ["circle", { cx: "9", cy: "10", r: "1.5" }], ["path", { d: "M21 16l-5-5-9 9" }]],
  /** A folder: the open session's workspace settings. */
  folder: [
    ["path", { d: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" }],
  ],
  /** A lidded box with a down arrow: archive the open session. */
  archive: [
    ["rect", { x: "3", y: "4", width: "18", height: "4", rx: "1" }],
    ["path", { d: "M5 8v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8M12 11v6M9.5 14.5L12 17l2.5-2.5" }],
  ],
  /** A lidded box with an up arrow: restore the archived session. */
  restore: [
    ["rect", { x: "3", y: "4", width: "18", height: "4", rx: "1" }],
    ["path", { d: "M5 8v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8M12 17v-6M9.5 13.5L12 11l2.5 2.5" }],
  ],
  /** A page with a folded corner: a file in the Files tab. */
  file: [
    ["path", { d: "M6 3h8l4 4v14H6z" }],
    ["path", { d: "M14 3v4h4" }],
  ],
  /** Chevrons: a directory collapsed, then expanded. */
  chevronRight: [["path", { d: "M9 6l6 6-6 6" }]],
  chevronDown: [["path", { d: "M6 9l6 6 6-6" }]],
  /** A circular arrow: reload the file tree. */
  refresh: [["path", { d: "M20 12a8 8 0 1 1-2.3-5.6M20 4v4h-4" }]],
  /** A branch fork with a dot: the Git status tab. */
  gitStatus: [
    ["circle", { cx: "6", cy: "5.5", r: "2" }],
    ["circle", { cx: "6", cy: "18.5", r: "2" }],
    ["circle", { cx: "18", cy: "8.5", r: "2" }],
    ["path", { d: "M6 7.5v9M18 10.5c0 4-12 2.5-12 6" }],
  ],
  /** A tick: an action that succeeded. */
  check: [["path", { d: "M5 12.5l4.5 4.5L19 7" }]],
  /** A cross: an action that failed. */
  cross: [["path", { d: "M6 6l12 12M18 6L6 18" }]],
  /** A bar split into segments with a marker above it: the context breakdown tab. */
  context: [
    ["rect", { x: "3", y: "9", width: "18", height: "8", rx: "2" }],
    ["path", { d: "M9 9v8M14 9v8M18 4v5" }],
  ],
} satisfies Record<string, readonly Shape[]>;

export type IconName = keyof typeof ICONS;

/** Creates one decorative 16-pixel icon that draws in the current text colour. */
export function icon(name: IconName): SVGSVGElement {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", "16");
  svg.setAttribute("height", "16");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "2");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  svg.classList.add("icon");
  for (const [tag, attrs] of ICONS[name]) {
    const shape = document.createElementNS(SVG_NS, tag);
    for (const [key, value] of Object.entries(attrs)) {
      shape.setAttribute(key, value);
    }
    svg.appendChild(shape);
  }
  return svg;
}

/**
 * Creates a `<button type="button">` that shows only an icon. `label` is its accessible name
 * and its tooltip.
 */
export function iconButton(
  name: IconName,
  label: string,
  onClick: () => void,
  className = "icon-button",
): HTMLButtonElement {
  const node = document.createElement("button");
  node.type = "button";
  node.className = className;
  // An attribute is not a text node, so the label is neutralised here as `el` would.
  const shown = neutralise(label);
  node.setAttribute("aria-label", shown);
  node.title = shown;
  node.appendChild(icon(name));
  node.addEventListener("click", onClick);
  return node;
}

/** Shows `name` and `label` on an icon button made by `iconButton`, replacing its icon. */
export function setIconButton(node: HTMLButtonElement, name: IconName, label: string): void {
  const shown = neutralise(label);
  node.setAttribute("aria-label", shown);
  node.title = shown;
  node.replaceChildren(icon(name));
}
