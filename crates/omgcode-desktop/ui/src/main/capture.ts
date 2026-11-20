/**
 * Paints one transcript entry into a PNG for the reply's **Copy image** button (ADR-0068).
 *
 * The webview has no screenshot interface, and the content security policy (`img-src 'self'`)
 * refuses the `data:` and `blob:` images that HTML-to-image libraries draw through. So this
 * module repaints the entry on a canvas from the layout the browser already computed: each
 * element's background and border from its computed style and box, and each run of text at the
 * position a `Range` reports for it. Nothing is fetched and no markup is parsed; the canvas only
 * ever holds pixels drawn here, so it is never tainted.
 *
 * It paints backgrounds (per line fragment for an inline element), uniform borders with one
 * corner radius, per-side borders without radius, overflow clipping, text with its computed
 * font and colour, and line-through, which also crosses the text of nested elements. It does
 * not paint shadows, gradients, images, outlines, or text ellipses; list markers, which are
 * pseudo-elements, are drawn from the list itself. Buttons and anything marked
 * `data-capture="skip"` are left out, so the image holds the reply without its controls.
 *
 * The work is bounded: the canvas is at most `MAX_CANVAS_SIDE` pixels on a side and
 * `MAX_CANVAS_AREA` pixels in all, the device scale is reduced to fit, and at most
 * `MAX_TEXT_RUNS` text runs are painted. An entry beyond those bounds is refused.
 */

export const MAX_CANVAS_SIDE = 8192;
export const MAX_CANVAS_AREA = 4096 * 4096;
export const MAX_TEXT_RUNS = 50_000;
/** Space around the entry, in CSS pixels, painted with the page background. */
export const CAPTURE_PADDING = 12;
const MAX_SCALE = 2;
const MIN_SCALE = 0.5;

/** Added to the entry while it is painted; the stylesheet then removes skipped parts from layout. */
export const CAPTURING_CLASS = "capturing";

export class CaptureError extends Error {}

interface Origin {
  x: number;
  y: number;
}

interface Budget {
  runs: number;
}

/**
 * Paints `element` and returns its PNG. The painting happens before this function returns, so it
 * reflects the layout at the click; only the PNG encoding is asynchronous. Rejects with a
 * `CaptureError` when the entry is empty, too large, or the canvas is unavailable.
 */
export function renderPng(element: HTMLElement): Promise<Blob> {
  try {
    // Take the skipped parts out of layout for the synchronous paint, so the image has no gap
    // where they were. No frame is drawn before the class is removed again.
    element.classList.add(CAPTURING_CLASS);
    let canvas: HTMLCanvasElement;
    try {
      canvas = paint(element);
    } finally {
      element.classList.remove(CAPTURING_CLASS);
    }
    return new Promise((resolve, reject) => {
      canvas.toBlob((blob) => {
        if (blob === null) {
          reject(new CaptureError("The image could not be encoded."));
        } else {
          resolve(blob);
        }
      }, "image/png");
    });
  } catch (reason) {
    return Promise.reject(reason);
  }
}

/** The device scale for a `width` by `height` CSS-pixel image within the canvas bounds. */
export function captureScale(width: number, height: number, deviceScale: number): number {
  let scale = Math.min(Math.max(deviceScale, 1), MAX_SCALE);
  scale = Math.min(scale, MAX_CANVAS_SIDE / width, MAX_CANVAS_SIDE / height);
  scale = Math.min(scale, Math.sqrt(MAX_CANVAS_AREA / (width * height)));
  if (!Number.isFinite(scale) || scale < MIN_SCALE) {
    throw new CaptureError("The reply is too large to capture as an image.");
  }
  return scale;
}

function paint(element: HTMLElement): HTMLCanvasElement {
  const box = element.getBoundingClientRect();
  if (box.width <= 0 || box.height <= 0) {
    throw new CaptureError("The reply has no visible area.");
  }
  const width = box.width + 2 * CAPTURE_PADDING;
  const height = box.height + 2 * CAPTURE_PADDING;
  const scale = captureScale(width, height, globalThis.devicePixelRatio ?? 1);

  const canvas = document.createElement("canvas");
  canvas.width = Math.ceil(width * scale);
  canvas.height = Math.ceil(height * scale);
  const context = canvas.getContext("2d");
  if (context === null) {
    throw new CaptureError("A canvas is not available.");
  }
  context.scale(scale, scale);
  context.fillStyle = backdrop(element);
  context.fillRect(0, 0, width, height);
  paintElement(
    context,
    element,
    { x: box.left - CAPTURE_PADDING, y: box.top - CAPTURE_PADDING },
    { runs: MAX_TEXT_RUNS },
  );
  return canvas;
}

/**
 * The colour behind `element` on screen, for the padding around it: the background of its nearest
 * ancestor that paints one, else black.
 */
export function backdrop(element: Element): string {
  for (let node = element.parentElement; node !== null; node = node.parentElement) {
    const colour = getComputedStyle(node).backgroundColor;
    if (visible(colour)) {
      return colour;
    }
  }
  return "#000000";
}

/**
 * Paints `element` and its descendants onto `context`, relative to `origin` in viewport
 * coordinates. `struck` is set inside an element with a line-through, which CSS draws through
 * the text of every descendant although the property is not inherited. Exported for tests,
 * which supply a recording context.
 */
export function paintElement(
  context: CanvasRenderingContext2D,
  element: Element,
  origin: Origin,
  budget: Budget,
  struck = false,
): void {
  if (element.tagName === "BUTTON" || element.getAttribute("data-capture") === "skip") {
    return;
  }
  const style = getComputedStyle(element);
  if (style.display === "none" || style.visibility === "hidden") {
    return;
  }
  const strike = struck || style.textDecorationLine.includes("line-through");
  const box = element.getBoundingClientRect();
  const x = box.left - origin.x;
  const y = box.top - origin.y;
  const inline = style.display === "inline";
  // An inline element that wraps has one box per line. Its bounding box would also cover the
  // text beside those lines, so each fragment is painted on its own.
  const fragments = inline ? Array.from(element.getClientRects()) : [box];
  for (const fragment of fragments) {
    paintBox(context, style, fragment, origin);
  }
  if (element.tagName === "LI") {
    paintListMarker(context, element, style, x, origin);
  }

  const radius = Math.min(px(style.borderTopLeftRadius), box.width / 2, box.height / 2);
  const clips = !inline && (style.overflowX !== "visible" || style.overflowY !== "visible");
  if (clips) {
    context.save();
    roundedPath(context, x, y, box.width, box.height, radius);
    context.clip();
  }
  for (const child of Array.from(element.childNodes)) {
    if (child.nodeType === Node.ELEMENT_NODE) {
      paintElement(context, child as Element, origin, budget, strike);
    } else if (child.nodeType === Node.TEXT_NODE) {
      paintText(context, child as Text, style, origin, budget, strike);
    }
  }
  if (clips) {
    context.restore();
  }
}

/**
 * A list marker is a pseudo-element that has no node to read, so it is drawn here: the item's
 * number for an ordered list, otherwise a disc, or a circle inside another list. It is
 * right-aligned before the item, on the baseline of the item's first text.
 */
function paintListMarker(
  context: CanvasRenderingContext2D,
  item: Element,
  style: CSSStyleDeclaration,
  x: number,
  origin: Origin,
): void {
  const list = item.parentElement;
  if (list === null || style.listStyleType === "none") {
    return;
  }
  let marker: string;
  if (list.tagName === "OL") {
    const start = Number.parseInt(list.getAttribute("start") ?? "1", 10);
    const index = Array.from(list.children).indexOf(item);
    marker = `${(Number.isFinite(start) ? start : 1) + index}.`;
  } else {
    marker = list.parentElement?.closest("li") === null ? "•" : "◦";
  }
  const first = firstTextRect(item);
  if (first === null) {
    return;
  }
  context.font = font(style);
  context.fillStyle = style.color;
  context.textBaseline = "alphabetic";
  context.textAlign = "right";
  const ascent = context.measureText("Mg").fontBoundingBoxAscent || px(style.fontSize) * 0.8;
  context.fillText(marker, x - 6, first.top - origin.y + ascent);
  context.textAlign = "left";
}

/** The box of the first visible character under `node`, or null when it holds no text. */
function firstTextRect(node: Element): DOMRect | null {
  const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
  const range = document.createRange();
  for (let current = walker.nextNode(); current !== null; current = walker.nextNode()) {
    const value = current.textContent ?? "";
    const at = value.search(/\S/);
    if (at !== -1) {
      range.setStart(current, at);
      range.setEnd(current, at + 1);
      return range.getClientRects()[0] ?? null;
    }
  }
  return null;
}

function font(style: CSSStyleDeclaration): string {
  return `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
}

/** Paints one box's background and borders. */
function paintBox(
  context: CanvasRenderingContext2D,
  style: CSSStyleDeclaration,
  box: DOMRectReadOnly,
  origin: Origin,
): void {
  const x = box.left - origin.x;
  const y = box.top - origin.y;
  const radius = Math.min(px(style.borderTopLeftRadius), box.width / 2, box.height / 2);
  if (visible(style.backgroundColor)) {
    context.fillStyle = style.backgroundColor;
    roundedPath(context, x, y, box.width, box.height, radius);
    context.fill();
  }
  paintBorders(context, style, x, y, box.width, box.height, radius);
}

function px(value: string): number {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

function visible(colour: string): boolean {
  return colour !== "" && colour !== "transparent" && !/^rgba\(.*,\s*0\)$/.test(colour);
}

function roundedPath(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): void {
  context.beginPath();
  if (radius <= 0) {
    context.rect(x, y, width, height);
    return;
  }
  context.moveTo(x + radius, y);
  context.arcTo(x + width, y, x + width, y + height, radius);
  context.arcTo(x + width, y + height, x, y + height, radius);
  context.arcTo(x, y + height, x, y, radius);
  context.arcTo(x, y, x + width, y, radius);
  context.closePath();
}

function paintBorders(
  context: CanvasRenderingContext2D,
  style: CSSStyleDeclaration,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): void {
  const sides = [
    { width: px(style.borderTopWidth), style: style.borderTopStyle, colour: style.borderTopColor },
    {
      width: px(style.borderRightWidth),
      style: style.borderRightStyle,
      colour: style.borderRightColor,
    },
    {
      width: px(style.borderBottomWidth),
      style: style.borderBottomStyle,
      colour: style.borderBottomColor,
    },
    {
      width: px(style.borderLeftWidth),
      style: style.borderLeftStyle,
      colour: style.borderLeftColor,
    },
  ];
  const drawn = sides.map(
    (side) => side.width > 0 && side.style !== "none" && visible(side.colour),
  );
  const [top, right, bottom, left] = sides;
  if (top === undefined || right === undefined || bottom === undefined || left === undefined) {
    return;
  }
  const uniform =
    drawn.every(Boolean) &&
    sides.every(
      (side) => side.width === top.width && side.colour === top.colour && side.style === top.style,
    );
  if (uniform) {
    const inset = top.width / 2;
    context.strokeStyle = top.colour;
    context.lineWidth = top.width;
    context.setLineDash(top.style === "dashed" ? [4, 3] : []);
    roundedPath(
      context,
      x + inset,
      y + inset,
      width - top.width,
      height - top.width,
      Math.max(0, radius - inset),
    );
    context.stroke();
    context.setLineDash([]);
    return;
  }
  const strips: Array<[boolean, string, number, number, number, number]> = [
    [drawn[0] === true, top.colour, x, y, width, top.width],
    [drawn[1] === true, right.colour, x + width - right.width, y, right.width, height],
    [drawn[2] === true, bottom.colour, x, y + height - bottom.width, width, bottom.width],
    [drawn[3] === true, left.colour, x, y, left.width, height],
  ];
  for (const [draw, colour, sx, sy, sw, sh] of strips) {
    if (draw) {
      context.fillStyle = colour;
      context.fillRect(sx, sy, sw, sh);
    }
  }
}

const NON_SPACE = /\S+/g;
const GRAPHEMES =
  typeof Intl === "object" && typeof Intl.Segmenter === "function"
    ? new Intl.Segmenter(undefined, { granularity: "grapheme" })
    : null;

/**
 * The user-perceived characters of `word`, so an emoji sequence or a letter with combining marks
 * is placed as one glyph. Without `Intl.Segmenter` it falls back to `codePointClusters`.
 */
export function graphemes(word: string): string[] {
  if (GRAPHEMES === null) {
    return codePointClusters(word);
  }
  return Array.from(GRAPHEMES.segment(word), (part) => part.segment);
}

const ZERO_WIDTH_JOINER = String.fromCharCode(0x200d);
const EXTENDS_CLUSTER = /^[\p{M}\p{Emoji_Modifier}\p{Variation_Selector}]$/u;
const REGIONAL_INDICATOR = /^\p{Regional_Indicator}$/u;

/**
 * An approximation of grapheme clusters for a webview without `Intl.Segmenter`: a code point
 * joins the one before it when it is a combining mark, an emoji modifier, a variation selector,
 * or a zero-width joiner, or follows a joiner, and two regional indicators form one flag. It does
 * not cover every rule, such as Hangul syllable sequences, but keeps the common joined glyphs
 * whole.
 */
export function codePointClusters(word: string): string[] {
  const out: string[] = [];
  let joined = false;
  let loneRegional = false;
  for (const point of word) {
    const regional = REGIONAL_INDICATOR.test(point);
    const extendsLast =
      joined ||
      point === ZERO_WIDTH_JOINER ||
      EXTENDS_CLUSTER.test(point) ||
      (regional && loneRegional);
    const last = out.length - 1;
    if (extendsLast && last >= 0) {
      out[last] += point;
      loneRegional = false;
    } else {
      out.push(point);
      loneRegional = regional;
    }
    joined = point === ZERO_WIDTH_JOINER;
  }
  return out;
}

function paintText(
  context: CanvasRenderingContext2D,
  node: Text,
  style: CSSStyleDeclaration,
  origin: Origin,
  budget: Budget,
  strike: boolean,
): void {
  const value = node.data;
  if (value.trim() === "") {
    return;
  }
  context.font = font(style);
  context.fillStyle = style.color;
  context.textBaseline = "alphabetic";
  context.textAlign = "left";
  const metrics = context.measureText("Mg");
  const ascent = metrics.fontBoundingBoxAscent || px(style.fontSize) * 0.8;
  const range = document.createRange();

  for (const match of value.matchAll(NON_SPACE)) {
    const start = match.index;
    const word = match[0];
    range.setStart(node, start);
    range.setEnd(node, start + word.length);
    const rects = range.getClientRects();
    if (rects.length <= 1) {
      const rect = rects[0];
      if (rect !== undefined) {
        paintRun(context, word, rect, origin, ascent, strike, budget);
      }
      continue;
    }
    // The word wraps inside itself, as overflow-wrap allows; place each character on its own.
    let offset = 0;
    for (const ch of graphemes(word)) {
      range.setStart(node, start + offset);
      range.setEnd(node, start + offset + ch.length);
      const rect = range.getClientRects()[0];
      if (rect !== undefined) {
        paintRun(context, ch, rect, origin, ascent, strike, budget);
      }
      offset += ch.length;
    }
  }
}

function paintRun(
  context: CanvasRenderingContext2D,
  run: string,
  rect: DOMRect,
  origin: Origin,
  ascent: number,
  strike: boolean,
  budget: Budget,
): void {
  budget.runs -= 1;
  if (budget.runs < 0) {
    throw new CaptureError("The reply holds too much text to capture as an image.");
  }
  const x = rect.left - origin.x;
  const y = rect.top - origin.y;
  context.fillText(run, x, y + ascent);
  if (strike) {
    context.fillRect(x, y + rect.height * 0.55, rect.width, 1);
  }
}
