/**
 * The frontend's only inline style (ADR-0072, ADR-0101): the size of a window section, written
 * through the CSSOM as a custom property that the section's stylesheet reads. The name comes from
 * the fixed list below and the value is a whole number of pixels its caller computed and clamped;
 * no other value, and no text, reaches a style. `tests/security.test.ts` checks that this module
 * holds every style write.
 */

/** Every custom property a section size may be written to. */
export const SIZE_PROPERTIES = [
  // The main window's columns (ADR-0072), on the shell.
  "--left-width",
  "--right-width",
  // The approvals window's queue, beside the review pane or above it in a narrow window.
  "--queue-width",
  "--queue-height",
  // The settings window's section list.
  "--nav-width",
  // The main window's message box, and the side panel's fixed footer.
  "--message-height",
  "--side-footer-height",
] as const;

export type SizeProperty = (typeof SIZE_PROPERTIES)[number];

/** No window section is larger than this many pixels. */
export const MAX_SECTION_SIZE = 16_384;

function check(name: SizeProperty): void {
  // The type already allows only listed names; this also holds for a value cast past the type.
  if (!SIZE_PROPERTIES.includes(name)) {
    throw new RangeError("Unknown section size property.");
  }
}

/**
 * Sets `name` on `element` to `pixels`.
 *
 * @throws RangeError when `name` is not listed or `pixels` is not a whole number from 0 to
 *   {@link MAX_SECTION_SIZE}. Callers clamp first, so this reports a defect.
 */
export function setSizeProperty(element: HTMLElement, name: SizeProperty, pixels: number): void {
  check(name);
  if (!Number.isSafeInteger(pixels) || pixels < 0 || pixels > MAX_SECTION_SIZE) {
    throw new RangeError("A section size must be a whole number of pixels.");
  }
  element.style.setProperty(name, `${pixels}px`);
}

/** Removes `name` from `element`, so the stylesheet's own size applies again. */
export function clearSizeProperty(element: HTMLElement, name: SizeProperty): void {
  check(name);
  element.style.removeProperty(name);
}
