/**
 * Line icons drawn as SVG elements through the DOM, never parsed from markup. Each icon is a
 * fixed list of shapes defined here; no caller-supplied value reaches an attribute. Icons are
 * decorative (`aria-hidden`): the button that holds one carries the accessible name.
 */

const SVG_NS = "http://www.w3.org/2000/svg";

type Shape = readonly [tag: "path" | "circle" | "rect", attrs: Readonly<Record<string, string>>];

const FRAME: Shape = ["rect", { x: "3", y: "4", width: "18", height: "16", rx: "2" }];

const ICONS = {
  /** A frame with a left column: the sessions panel. */
  panelLeft: [FRAME, ["path", { d: "M9 4v16" }]],
  /** A frame with a right column: the terminal panel. */
  panelRight: [FRAME, ["path", { d: "M15 4v16" }]],
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
  terminal: [FRAME, ["path", { d: "M7 9l3 3-3 3M13 15h4" }]],
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
  /** A tick: an action that succeeded. */
  check: [["path", { d: "M5 12.5l4.5 4.5L19 7" }]],
  /** A cross: an action that failed. */
  cross: [["path", { d: "M6 6l12 12M18 6L6 18" }]],
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
  node.setAttribute("aria-label", label);
  node.title = label;
  node.appendChild(icon(name));
  node.addEventListener("click", onClick);
  return node;
}

/** Shows `name` and `label` on an icon button made by `iconButton`, replacing its icon. */
export function setIconButton(node: HTMLButtonElement, name: IconName, label: string): void {
  node.setAttribute("aria-label", label);
  node.title = label;
  node.replaceChildren(icon(name));
}
