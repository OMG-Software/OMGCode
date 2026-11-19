import { describe, expect, it } from "vitest";

const sheets = import.meta.glob<string>("../src/styles/tokens.css", {
  query: "?raw",
  import: "default",
  eager: true,
});
const tokens = Object.values(sheets)[0] ?? "";

/** Reads every `--name: #rrggbb;` declaration in the root block. */
function colours(css: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const match of css.matchAll(/^\s*--([a-z0-9-]+):\s*(#[0-9a-f]{6});/gim)) {
    const [, name, value] = match;
    if (name !== undefined && value !== undefined) {
      found.set(name, value.toLowerCase());
    }
  }
  return found;
}

function channel(hex: string, offset: number): number {
  const value = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
  return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

/** WCAG 2.x relative luminance. */
function luminance(hex: string): number {
  return 0.2126 * channel(hex, 1) + 0.7152 * channel(hex, 3) + 0.0722 * channel(hex, 5);
}

/** WCAG 2.x contrast ratio. */
function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

const palette = colours(tokens);

function token(name: string): string {
  const value = palette.get(name);
  if (value === undefined) {
    throw new Error(`tokens.css has no colour --${name}`);
  }
  return value;
}

const SURFACES = ["bg", "surface", "surface-2", "surface-3"];

/** Text tokens that must reach WCAG AA (4.5:1) on every surface. */
const TEXT = ["text", "muted", "accent-fg", "danger", "warn", "success"];

/** Text on a tinted or filled background, each at least 4.5:1. */
const PAIRS: Array<[string, string]> = [
  ["accent-text", "accent"],
  ["accent-text", "accent-hover"],
  ["accent-fg", "accent-soft"],
  ["text", "accent-soft"],
  ["danger", "danger-bg"],
  ["warn", "warn-bg"],
  ["text", "mark"],
  ["bg", "text"],
];

describe("theme tokens", () => {
  it("declares one dark scheme and no light override", () => {
    expect(tokens).toMatch(/color-scheme:\s*dark;/);
    expect(tokens).not.toMatch(/prefers-color-scheme/);
  });

  it.each(TEXT.flatMap((fg) => SURFACES.map((bg) => [fg, bg] as const)))(
    "%s on %s reaches 4.5:1",
    (fg, bg) => {
      expect(contrast(token(fg), token(bg))).toBeGreaterThanOrEqual(4.5);
    },
  );

  it.each(PAIRS)("%s on %s reaches 4.5:1", (fg, bg) => {
    expect(contrast(token(fg), token(bg))).toBeGreaterThanOrEqual(4.5);
  });

  it.each(["bg", "surface", "surface-2"])("control borders and focus reach 3:1 on %s", (bg) => {
    expect(contrast(token("control-border"), token(bg))).toBeGreaterThanOrEqual(3);
    expect(contrast(token("accent"), token(bg))).toBeGreaterThanOrEqual(3);
  });

  it("computes the reference contrast values correctly", () => {
    expect(contrast("#000000", "#ffffff")).toBeCloseTo(21, 5);
    expect(contrast("#777777", "#777777")).toBeCloseTo(1, 5);
  });
});
