# 0101: Make every section divider movable

- Status: Accepted
- Date: 2026-11-26
- Implemented: version 0.65.0
- Amends: [ADR-0072](0072-resize-and-collapse-the-main-window-panels-and-follow-the-transcript-end.md)
  decision 6, for where the one inline style is written

## Context

The operator asked to resize the left-hand column of the `approvals` window,
which lists the approvals, by moving its divider, and asked that every section
divider in every window move, so that any section can be resized.

Facts established before the decision, on 2026-11-26:

- Only the `main` window's three columns resized (ADR-0072). The `approvals`
  queue was fixed at 300 pixels beside the review pane and, below 720 pixels
  wide, stacked above it at its content's height up to 40% of the window. The
  `settings` section list was fixed at 190 pixels. The `main` window's message
  box had the browser's own resize grip in its corner, capped at 40% of the
  window, and the right-hand panel's fixed footer, which holds the model box
  (ADR-0089), had its content's height.
- The content security policy is `style-src 'self'`. ADR-0072 decision 6 let
  `ui/src/main/layout.ts` write the two column widths through the CSSOM and no
  other code set a style; `ui/tests/security.test.ts` pinned that one write.
- The frontend may not use web storage
  ([ADR-0062](0062-send-the-desktop-message-on-enter-as-a-saved-chat-preference.md)).
- The other borders in the windows are headers, tab strips, toolbars, the
  status bar, the sidebar's two-button footer, and the review pane's sticky
  decision row. Each holds a fixed set of controls, not content that scrolls.

## Decision

1. **These dividers move.** Each is a one-pixel line between a section and a
   neighbour that gives way, down to the neighbour's least size:

   | Window | Section | Neighbour | Size | Starts at | Neighbour keeps |
   | --- | --- | --- | --- | --- | --- |
   | `approvals` | Queue (width) | Review pane | 200 to 640 | 300 | 280 |
   | `approvals`, under 720 pixels wide | Queue (height) | Review pane | 96 to 640 | 240 | 160 |
   | `settings` | Section list (width) | Content | 150 to 420 | 190 | 320 |
   | `main` | Message box (height) | Transcript | 64 to 1200 | three rows | 120 |
   | `main` | Side panel footer (height) | Tab content | 48 to 720 | its content | 96 |

   The `main` window's columns keep ADR-0072's separators, limits, and
   collapse. The `approvals` divider turns horizontal when its panes stack and
   keeps one size for each orientation. The `settings` divider hides with the
   section list in a window scoped to one workspace. The message box's divider
   is the composer's top line; it replaces the box's own resize grip, which
   would write an inline height of its own. A footer made shorter than the
   model box scrolls.
2. **Fixed controls do not get a divider.** Headers, tab strips, toolbars, the
   status bar, the sidebar's footer, and the sticky decision row stay as they
   are: making them taller or shorter would add empty space or hide a control.
3. **One splitter component.** `ui/src/ui/splitter.ts` builds each new divider.
   It follows the WAI-ARIA window splitter pattern with ADR-0072's keys: drag
   it, or focus it and use the arrow keys along its movement (16 pixels, 64
   with Shift), Home for the section's minimum, and End for the largest size
   the neighbour allows. A double click restores the starting size, or the
   stylesheet's own size where there is none. It never collapses a section, so
   Enter does nothing. It reports `aria-valuenow`, `aria-valuemin`,
   `aria-valuemax`, and `aria-valuetext`. A drag listens on the document, so it
   follows a pointer that leaves the line, but only the pointer that started
   it moves or ends it; it marks the page so the cursor stays and no text is
   selected. When the window or the neighbour shrinks, the section narrows
   until the neighbour keeps its minimum; it does not grow back by itself. A
   neighbour squeezed to nothing shows only part of the shortfall, so a fit
   writes the size and measures again, at most 32 times. A change in the
   neighbour's own size is fitted on the next frame, outside the
   size observer's callback, so narrowing cannot report an observer loop. A
   hidden section measures nothing and keeps its size for when it shows again.
4. **One module writes styles.** `ui/src/ui/size_property.ts` is now the only
   frontend code that sets a style. It sets one of a fixed list of custom
   properties to a whole number of pixels from 0 to 16 384, or removes one of
   them, and throws for anything else. `layout.ts` calls it for the column
   widths. The section's own stylesheet reads the property. The security test
   pins the module, its two writes, its checks, and the shape of every listed
   name. This moves ADR-0072 decision 6's write; its rule, that no text
   reaches a style, is unchanged.
5. **Sizes are not kept.** As in ADR-0072 decision 7, every size lasts until
   the window closes.

No command, IPC shape, permission, capability, or Rust code changes.

## Alternatives considered

- **The CSS `resize` property on each section.** Rejected for the reason
  ADR-0072 gave: its grip sits in a corner, and a section beside or below its
  neighbour would resize from the wrong edge. It also writes an inline size
  that the stylesheet and the limits cannot see.
- **Reuse `layout.ts` in each window.** Rejected. It owns two collapsible
  panels around a centre column. These dividers size one section against one
  neighbour and never collapse it; a collapsed queue or section list would
  hide the window's navigation.
- **Collapse on Enter or a double click, as the columns do.** Rejected. These
  sections have no rail to bring them back, so a collapse could only be undone
  by finding a one-pixel line.
- **A divider on every border.** Rejected by decision 2.
- **Keep sizes across launches.** Not done, for ADR-0072's reason: web storage
  is forbidden, and keeping them through the core needs a command for a
  convenience the operator did not ask for.
- **Set each property on the layout container.** Rejected. Setting it on the
  section keeps each property beside the rule that reads it. The `settings`
  window changes from a two-column grid to a flex row so its section list can
  read its own width.
- **A separate media-query listener for the `approvals` orientation.** Not
  added. Crossing 720 pixels resizes the window and the review pane, and both
  already make the splitter read the orientation again.

## Consequences

- The operator can widen the approval queue to read long summaries, widen
  the settings section list to read long workspace paths, give the message box
  more room, and give the model box more or less room.
- The message box loses its corner grip and its 40% cap; its divider allows
  any height that leaves the transcript 120 pixels.
- The stacked `approvals` queue starts at 240 pixels instead of its content's
  height.
- The list of size properties must be kept in step with the stylesheets. A
  listed name that no rule reads has no effect; a new name needs an entry in
  the list, whose shape the security test checks.
- Verification: Vitest covers the splitter in jsdom, which computes no layout,
  with stubbed sizes, and each window's divider through its app. In the Vite
  development page with the mock backend, every divider was dragged and the
  sizes read back: the queue from 300 to 420 pixels, the stacked queue from 240
  to 320, the section list from 190 to 321, the message box from 71 to 231 as
  the transcript went from 492 to 332, and the footer from 140 to 290 as the
  tab content gave up 150. End stopped the message box with the transcript at
  120 pixels. No text was selected and no console error was logged. The
  browser pane was hidden, so it delivered no resize events or size-observer
  callbacks; the `approvals` orientation was checked by loading the page at
  700 pixels wide and by dispatching a resize event. Nothing was run in the
  Tauri shell's WebView2 or WebKitGTK.

## References

- [Desktop interface guide](../desktop-interface.md#appearance)
- [ADR-0065: Redesign the desktop windows with one dark theme and split layouts](0065-redesign-the-desktop-windows-with-one-dark-theme-and-split-layouts.md)
- [ADR-0072: Resize and collapse the main window panels and follow the transcript end](0072-resize-and-collapse-the-main-window-panels-and-follow-the-transcript-end.md)
- [ADR-0089: Choose the provider and model in the side panel footer](0089-choose-the-provider-and-model-in-the-side-panel-footer.md)
- [WAI-ARIA window splitter pattern](https://www.w3.org/WAI/ARIA/apg/patterns/windowsplitter/)
