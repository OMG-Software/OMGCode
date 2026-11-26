# 0072: Resize and collapse the main window panels and follow the transcript end

- Status: Accepted, amended by ADR-0083 and
  [ADR-0101](0101-make-every-section-divider-movable.md)
- Date: 2026-11-20
- Implemented: version 0.51.0

## Context

The operator asked for four changes to the desktop `main` window:

- Chat blocks were at most about 72 characters wide, so most of a wide window
  was empty. They should follow the window's width, and the operator's and the
  assistant's blocks should both be centred.
- A right-hand terminal panel that can be resized and collapsed.
- The sessions sidebar, fixed at 260 pixels, should be resizable and
  collapsible, and show only icons when collapsed.
- After a message is sent, the transcript should scroll to its end and stay
  there while content arrives, until the operator scrolls up. Scrolling back
  to the end should follow it again.

Constraints:

- The webview has no authority ([ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)).
  Nothing in the desktop shell calls the unconfined process runner, and the
  trusted `run` command starts one confined, non-interactive program at a
  time ([execution contract](../execution.md)). An interactive shell would be
  new authority for the `main` webview.
- The content security policy is `style-src 'self'`, and
  [ADR-0068](0068-name-replies-by-model-and-copy-them-as-markdown-text-or-image.md)
  states that the frontend may not set inline styles.
- The frontend may not use web storage
  ([ADR-0062](0062-send-the-desktop-message-on-enter-as-a-saved-chat-preference.md)),
  so a panel's width cannot be kept in the webview between launches.

## Decision

1. **Three columns.** The `main` window is a grid of the sessions panel, a
   separator, the conversation, a separator, and the terminal panel. The
   [desktop interface guide](../desktop-interface.md#appearance) owns the
   layout.
2. **Separators are window splitters.** Each is a focusable `separator` with
   `aria-controls`, `aria-valuenow`, `aria-valuemin`, and `aria-valuemax`.
   Dragging moves it; the arrow keys move it by 16 pixels, or 64 with Shift;
   Home and End go to the limits; Enter or a double click collapses or expands
   the panel. Dragging a panel more than 80 pixels below its minimum collapses
   it, and dragging a collapsed panel out expands it.
3. **Limits.** The sessions panel is 200 to 480 pixels and opens at 260. The
   terminal panel is 240 to 720 pixels, opens at 360 when expanded, and starts
   collapsed. Neither panel may take the conversation below 360 pixels. When
   the window narrows, the terminal panel gives way first, then the sessions
   panel. A panel dragged or keyed wider grows into the other panel, which
   narrows down to its own minimum. A panel that expands from its rail takes
   the free room first, and the other panel narrows only as far as the
   expanding panel's minimum needs. When both panels at their minimums would leave the conversation less
   than 360 pixels, as in an 800-pixel window, the other panel collapses
   instead; on narrowing, the terminal panel collapses first. A panel
   collapsed this way stays collapsed when the window widens.
4. **Collapsed panels are rails.** A collapsed panel is a 44-pixel rail of
   icon buttons. The sessions rail holds Expand, New session, Search, Sessions,
   Approvals with the waiting count, and Settings. Search and Sessions expand
   the panel and focus the search box or the open session. The terminal rail
   holds one button that expands it. Every icon button has an accessible name
   and a tooltip with the same fixed text. Icons are SVG elements built
   through the DOM from shapes fixed in `src/ui/icons.ts`.
5. **The terminal panel has no terminal.** It says that no terminal is
   available and that it starts no shell and runs no command. It calls no
   command and has no input. A terminal needs its own decision on what runs,
   with what authority, and which dependencies; this record does not make it.
6. **One inline style.** `src/main/layout.ts` sets `--left-width` and
   `--right-width` on the shell through the CSSOM. Each value is a whole
   number of pixels that the module computes and clamps from pointer
   coordinates, keys, and the shell's width. This narrows ADR-0068's rule: no
   other frontend code sets a style, and no text reaches one.
   `tests/security.test.ts` checks that the one write is the only one.
7. **Widths are not kept.** Panel widths and collapsed states last until the
   window closes.
8. **A centred column of entries.** Every transcript entry is centred and
   takes 90% of the transcript's width. The operator's bubble fits its text
   up to that width. [ADR-0074](0074-give-chat-bubbles-a-shared-width.md)
   replaces this sizing rule.
9. **Following the end.** Sending a message scrolls the transcript to its end
   and pins it there. Content that arrives while it is pinned, a turn's end,
   and a change in the transcript's size keep the end in view. A scroll event
   that moves up and leaves the end unpins it; one that reaches the end pins
   it again. Growth alone fires no scroll event, so it cannot unpin. Opening
   another session or a search hit starts unpinned, and it pins once it is at
   its end. **Load more** unpins when it is pressed, not when its page
   arrives, so the next page is read from where the operator is and a send
   made while the page loads still follows the end.

No command, IPC shape, permission, or Rust authority check changes.

## Alternatives considered

- **An interactive shell in the terminal panel now.** Deferred at the
  operator's choice. An unconfined pseudo-terminal would let the `main`
  webview run any command with the operator's rights. A confined shell would
  need interactive support in the one-shot runner. Either needs new
  dependencies and its own record.
- **Discrete widths as CSS classes.** Rejected. Every width step would need
  its own rule, and a drag would jump between steps.
- **Typed `attr()` for the width.** Rejected. Only recent Chromium supports
  it; WebKitGTK on Linux would ignore it and the panels could not resize.
- **A constructed stylesheet.** Rejected. It is no narrower than one CSSOM
  property, and it is harder to scan for.
- **The CSS `resize` property.** Rejected. Its grip sits in a corner of the
  panel, and the terminal panel would resize from the wrong edge.
- **Keeping widths across launches.** Not done. Web storage is forbidden, and
  saving them through the core would add a command for a convenience.
- **Following the end whenever the view is at the end before a render.**
  This was the earlier rule for the live preview. A send did not scroll to the
  end, so a send from higher up never followed the reply, and a turn's end
  jumped to the end even after the operator had scrolled up.

## Consequences

- Chat uses the window's width, and the operator chooses how much the panels
  take. Long lines of prose are harder to read in a very wide window.
- The frontend sets one inline style. The security scan keeps it to the two
  width properties in one module.
- A collapsed panel and its width are lost at each launch.
- The terminal panel is a placeholder until a later record decides what it
  runs.
- The tests run in jsdom, which computes no layout. They check the widths,
  states, and attributes the code sets and the scroll offsets it chooses, with
  stubbed sizes. The layout was checked in the Vite development page with the
  mock backend: dragging, keyboard resizing, the rails, the centred column,
  and following the end through a mock turn. The shell's size observer could
  not be seen there, because the browser pane was hidden and rendered no
  frames; a unit test drives it with a stubbed observer. The change was not
  run in the Tauri shell.

## References

- [Desktop interface guide](../desktop-interface.md)
- [ADR-0062: Send the desktop message on Enter as a saved chat preference](0062-send-the-desktop-message-on-enter-as-a-saved-chat-preference.md)
- [ADR-0065: Redesign the desktop windows with one dark theme and split layouts](0065-redesign-the-desktop-windows-with-one-dark-theme-and-split-layouts.md)
- [ADR-0068: Name replies by model and copy them as Markdown, text, or an image](0068-name-replies-by-model-and-copy-them-as-markdown-text-or-image.md)
- [WAI-ARIA window splitter pattern](https://www.w3.org/WAI/ARIA/apg/patterns/windowsplitter/)
