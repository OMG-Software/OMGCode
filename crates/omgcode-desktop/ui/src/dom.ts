/**
 * Minimal typed DOM builder. Text always goes through text nodes, so untrusted strings
 * can never become markup. Attributes that can execute or navigate are refused.
 */

export type Child = Node | string | null | undefined | false;

export interface ElOptions {
  class?: string;
  text?: string;
  attrs?: Readonly<Record<string, string>>;
}

const BIDI_CONTROLS = /[‪-‮⁦-⁩]/g;

/** Makes bidirectional override characters visible so they cannot reorder displayed text. */
export function neutralise(value: string): string {
  return value.replace(BIDI_CONTROLS, (c) => {
    const hex = c.charCodeAt(0).toString(16).toUpperCase().padStart(4, "0");
    return `[U+${hex}]`;
  });
}

function attributeAllowed(name: string): boolean {
  const lower = name.toLowerCase();
  const refused = lower === "style" || lower === "href" || lower === "src" || lower === "srcdoc";
  return !(refused || lower.startsWith("on"));
}

/** Creates a text node for untrusted text. */
export function text(value: string): Text {
  return document.createTextNode(neutralise(value));
}

/** Appends children; strings become text nodes. */
export function append(parent: Node, ...children: Child[]): void {
  for (const child of children) {
    if (child === null || child === undefined || child === false) {
      continue;
    }
    parent.appendChild(typeof child === "string" ? text(child) : child);
  }
}

/** Replaces all children of `parent`. */
export function replaceChildren(parent: Element, ...children: Child[]): void {
  parent.replaceChildren();
  append(parent, ...children);
}

/** Creates an element. Strings in `children` become text nodes. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  options: ElOptions = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (options.class !== undefined) {
    node.className = options.class;
  }
  if (options.text !== undefined) {
    node.textContent = neutralise(options.text);
  }
  for (const [name, value] of Object.entries(options.attrs ?? {})) {
    if (!attributeAllowed(name)) {
      throw new Error(`Attribute ${name} is not allowed.`);
    }
    node.setAttribute(name, value);
  }
  append(node, ...children);
  return node;
}

/** Creates a `<button type="button">` with a click handler. */
export function button(
  label: string,
  onClick: () => void,
  options: ElOptions = {},
): HTMLButtonElement {
  const node = el("button", { ...options, text: label });
  node.type = "button";
  node.addEventListener("click", onClick);
  return node;
}
