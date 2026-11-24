# 0095: Use the OMG Software palette and OMGCode icons in the desktop

- Status: Accepted
- Date: 2026-11-24

## Context

ADR-0065 gave every desktop window one dark theme of near-black, slightly blue
greys with an orange accent (`#ff7a1a`). The app icons were placeholders.
OMG Software now has an outlined-toggle brand package: switch orange `#FF761F`
and charcoal `#151515`, a standalone knob symbol for small sizes, and charcoal
on orange for text on the accent. The OMGCode project icon set draws the
toggle on charcoal with a `>_` prompt cut out of the knob, and includes a
`.ico` and PNGs from 16 to 1024 pixels.

## Decision

- `ui/src/styles/tokens.css` uses neutral greys around the brand charcoal:
  `--bg` `#111111`, `--surface` `#151515`, then `#1e1e1e` and `#282828`, with
  neutral borders and text. `--accent` is switch orange `#ff761f`, and text on
  it is charcoal `#151515`. The accent's meaning from ADR-0065 is unchanged:
  it marks actions, focus, selection, and "waiting for you", never a
  permission at Allow.
- The Rust window background matches the new `--bg`, and a unit test reads
  `tokens.css` to keep the two equal.
- The `main` sessions panel, the `settings` navigation, and the `approvals`
  header lead their heading with the OMGCode knob: an 18-pixel SVG built
  through the DOM from one fixed path, filled with the accent, and hidden from
  assistive technology because the heading text names the window.
- `crates/omgcode-desktop/icons/` holds the OMGCode project icons: the `.ico`
  and the 32, 128, 256, and 512 pixel PNGs.
- The sessions panel's **New session** label no longer wraps, and the title
  row wraps its actions under the heading when the panel is too narrow.

## Alternatives considered

- Use the full toggle mark beside each heading: rejected. At the sessions
  panel's default 260-pixel width the row did not fit, and the brand guide
  recommends the standalone knob at small sizes.
- Load the brand SVG files as images: rejected. The content policy allows only
  `'self'` images, and building the fixed path through the DOM keeps the
  no-markup-parsing rule for icons.
- Make `--bg` charcoal `#151515` itself: rejected. The panels need a step
  between page and surface, so the page is one step darker, as in the brand
  preview.

## Consequences

- The palette still passes the contrast checks in `ui/tests/tokens.test.ts`.
- The bundled icons change the taskbar, window, and executable icons on the
  next build. No installer exists, so no installed icon is updated.
- The look was checked in the browser mock only, not in a packaged build on
  Windows or Linux.

## References

- [ADR-0065](0065-redesign-the-desktop-windows-with-one-dark-theme-and-split-layouts.md)
- [Desktop interface: Appearance](../desktop-interface.md#appearance)
