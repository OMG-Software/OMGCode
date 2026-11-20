# 0068: Name replies by model and copy them as Markdown, text, or an image

- Status: Accepted
- Date: 2026-11-20

## Context

ADR-0067 renders a recorded assistant reply as inert Markdown in a response
block. The operator asked for three more things on each reply: a label naming
the model instead of "Assistant", and buttons that copy the reply's raw
Markdown, the reply as text without Markdown symbols, and an image of just
that reply. The operator chose copy buttons over view toggles, and asked for
no third general-purpose copy button.

The transcript wire format gives a message entry no model field. Core records
a `request.manifest` for every model request, holding the model id, and
`SessionJournal::record_manifest` accepts one only inside the active turn, so
a manifest always follows its turn's start and precedes the turn's replies.
Transcripts load from the oldest entry forward.

The webview has no screenshot interface. The content security policy is
`img-src 'self'`, so the `data:` and `blob:` image URLs that HTML-to-image
libraries draw an SVG `foreignObject` through are refused. The frontend may
not set inline styles or parse markup. The `main` window has no file-writing
command and opens no dialogs.

## Decision

1. **The model from the turn's manifest.** `entryContexts` walks the loaded
   entries in order: a request manifest sets the model, and a turn start
   clears it. A reply is labelled with that model, or "Assistant" when none
   is loaded, as in an older journal. The live entry is labelled with the
   model the send from this window asked for, else the loaded last turn's.
   The id is untrusted text from the model server, shown through text nodes
   with bidirectional overrides marked, cut with an ellipsis, and given its
   full text as a tooltip. It names the model that was asked; it is not an
   identity check.
2. **Copy Markdown** writes the recorded reply text with visible markers in
   place of bidirectional overrides, the same rule ADR-0067 applies to code
   blocks.
3. **Copy text** writes the rendered reply as plain text, read from the
   rendered elements by `plainText`: blocks separated by blank lines, also
   inside a list item, with a nested list on the line after its item, "•"
   bullets, ordered numbers, nested items indented, table cells separated by
   tabs, code exactly and without fences, and links with their shown
   destination. A reply shown as plain text, over the Markdown limit, is
   copied as shown.
4. **Copy image** paints the whole entry, label and time included, onto a
   canvas and writes one PNG through `navigator.clipboard.write`.
   `src/main/capture.ts` repaints from the computed layout: backgrounds and
   borders with one corner radius (for an inline element, one per line
   fragment, so a wrapped code span does not cover the text beside it),
   overflow clipping, text runs at the
   positions `Range.getClientRects` reports with their computed font and
   colour, line-through (also across nested elements, which do not inherit
   the property), and list markers drawn from the list. A word that wraps
   inside itself is placed one grapheme at a time, through `Intl.Segmenter`
   where it exists, and otherwise by joining combining marks, emoji
   modifiers, variation selectors, zero-width-joiner sequences, and flag
   pairs to the code point before them. The padding takes the background of the entry's nearest
   ancestor that paints one. Buttons and
   parts marked `data-capture="skip"` are left out; a `capturing` class
   removes them from layout for the synchronous paint, so the image has no
   gap, and lets a code block wider than the reply wrap instead of being cut
   at its scroll edge. It is removed before any frame is drawn. The canvas is at most 8,192
   pixels on a side and 4096 × 4096 in area, the device scale (at most 2) is
   reduced to fit, and at most 50,000 text runs are painted; a larger entry
   is refused. The PNG is passed to `ClipboardItem` as a pending blob, so the
   write starts inside the click, as WebKit requires.
5. **Clipboard only.** Every action writes to the clipboard on a click and
   nothing reads it. Each button shows "Copied" or "Copy failed" for 1.5
   seconds. The actions appear only on recorded assistant replies, not on the
   live preview, the operator's messages, tool messages, or summaries.

## Alternatives considered

- **An HTML-to-image library (`html2canvas`, `html-to-image`).** Rejected.
  `html-to-image` draws through an SVG `foreignObject` loaded as a `data:` or
  `blob:` image, which the policy refuses. `html2canvas` clones the document
  into an iframe and is a large dependency with no release since 2022. Both
  would join the supply chain for a single entry's repaint.
- **A native capture through a new Rust command.** Rejected for now. Tauri
  exposes no element capture; WebView2's `CapturePreview` captures the whole
  view and needs cropping, with a different path on WebKitGTK. It would add a
  `main` command for a cosmetic feature.
- **Saving the image to a file.** Not chosen. It needs a new `main` command
  and native save dialog, which the `main` window does not have, and would
  let a compromised `main` webview write chosen bytes wherever the operator
  confirms. The clipboard needs no new authority.
- **A model field on each message entry.** Rejected for now. It would change
  the core transcript wire type and journal replay for a value the manifest
  already records.
- **Clearing the model only at the next manifest.** Rejected. A turn with no
  loaded manifest would carry an earlier turn's model.
- **View toggles between rendered, Markdown, and text.** Not chosen by the
  operator.

## Consequences

- A reply names its model, and the operator can take it out as Markdown,
  text, or a picture.
- The image is a repaint, not a screenshot. Shadows, gradients, outlines,
  images, and text ellipses are not drawn, and glyph positions can differ
  slightly from the screen. It was compared with the screen in a Chromium
  browser pane against the mock backend, where a code line scrolled far past
  the reply's edge was also painted wrapped inside the image.
- A model server chooses the id shown as the label. A crafted id could read
  like another name; the reply's position and shading still mark it as a
  reply.
- Zero-width and other invisible characters are copied too, as ADR-0067
  records.
- Copying succeeded only under jsdom with stubbed clipboard, canvas, and
  layout. The Chromium browser pane refuses every clipboard write and shows
  "Copy failed". A successful copy of text or an image in the WebView2 or
  WebKitGTK shell has not been exercised, and WebKitGTK's support for
  `ClipboardItem` with a pending blob is unverified.

## References

- [Desktop interface](../desktop-interface.md)
- [ADR-0067: Render recorded replies as inert Markdown](0067-render-recorded-replies-as-inert-markdown.md)
- [Session storage](../session-storage.md)
- [Clipboard API `write`](https://developer.mozilla.org/en-US/docs/Web/API/Clipboard/write)
- [`ClipboardItem`](https://developer.mozilla.org/en-US/docs/Web/API/ClipboardItem)
- [`Range.getClientRects`](https://developer.mozilla.org/en-US/docs/Web/API/Range/getClientRects)
