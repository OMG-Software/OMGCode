# 0067: Render recorded replies as inert Markdown

- Status: Accepted
- Date: 2026-11-20

## Context

Local models answer in Markdown. The desktop `main` window showed every reply
as one pre-wrapped text node, so a fenced command such as

````text
```bash
python3 server.py
```
````

appeared with its fence characters, in the body font, and had to be selected
by hand. ADR-0065 also left replies without a box, which made a long reply
hard to tell from the notes around it.

A reply is untrusted model output, and prompt injection can shape it. The
frontend rules require that the document is built only from elements and text
nodes, and `tests/security.test.ts` scans `src/` for `innerHTML`,
`DOMParser`, and other markup-injecting interfaces. `src/dom.ts` refuses
`href`, `src`, `style`, and event attributes. The security rules allow
"safe text or sanitized Markdown" and ask that clipboard effects be
restricted. Product priorities are secure operation, prompt injection
defence, and efficiency.

## Decision

1. **A Markdown subset parsed in the frontend.** `src/main/markdown.ts`
   parses a recorded assistant reply into paragraphs, ATX headings, thematic
   breaks, block quotes, bullet and ordered lists nested by indentation,
   fenced code blocks, GitHub-style tables, inline code, strong, emphasis,
   strikethrough, and backslash escapes. Anything else stays text. Line breaks
   inside a paragraph are kept, as replies are written for a chat view.
2. **Built through the existing DOM helpers.** Every element comes from `el`
   and every string from `text`, so raw HTML in a reply is literal text and
   bidirectional overrides become visible markers as before. No dependency is
   added.
3. **Links and images are inert.** A link shows its text and then its
   destination in a muted span. An image shows `[image: description]` and its
   source. No element gets an `href` or `src`, nothing navigates, and nothing
   loads. Showing the destination also stops a label such as a bank address
   from posing as a different target.
4. **Bounded work.** A reply over 100,000 UTF-16 code units is shown as plain
   text. Recorded replies are already at most 16 KiB, so this bound is defence
   in depth. Block quotes and list items nest at most six deep and emphasis
   and link text at most eight; deeper source is shown as text. A table has
   at most 32 columns. Closing-delimiter searches are cached per delimiter, so
   a reply full of unmatched delimiters is parsed in close to linear time, and
   the line patterns cannot backtrack across a line more than once. Tests
   parse pathological replies under a time bound.
5. **Copy writes what is shown.** A fenced code block has a language label
   and a **Copy** button. The click handler writes the code element's text
   content through `navigator.clipboard.writeText`. That text is the
   displayed text, with visible markers in place of bidirectional overrides,
   so the clipboard cannot hold a line that reads differently from the
   screen. Nothing reads the clipboard. The button shows "Copied" or
   "Copy failed" for 1.5 seconds.
6. **Only recorded assistant replies.** The operator's messages, tool
   messages, compaction summaries, and the live preview stay plain text. The
   live preview updates one text node per animation frame; it is provisional,
   and Markdown appears when the recorded reply replaces it.
7. **A response block.** A reply sits on the `--surface` shade with a border
   and rounded corners, one step lighter than the transcript. A code block is
   darker, on `--bg`, with a `--surface-2` header. This replaces the boxless
   reply from ADR-0065. No colour token is added.

## Alternatives considered

- **A Markdown library with a sanitizer (`marked` or `markdown-it` with
  DOMPurify).** Rejected. They produce an HTML string that must be assigned
  through `innerHTML`, which the frontend rules and the source scan forbid,
  and they would add two frontend dependencies to the supply chain. The
  sanitizer would become the security boundary instead of the DOM builder.
- **A library that emits a syntax tree (`mdast` from `micromark`).**
  Rejected for now. It avoids `innerHTML` but adds a larger dependency tree
  for a subset this small, and still needs a tree-to-element step written
  here.
- **Live links that open in the system browser.** Rejected. The navigation
  guard denies other origins, `href` is refused by `src/dom.ts`, and a
  model-chosen link is a prompt-injection lure. The operator can copy a
  destination by hand.
- **Rendering Markdown in the live preview.** Deferred. Rebuilding the tree on
  every frame costs more than updating one text node, and a partial reply
  often holds an unclosed fence or emphasis, so the preview would flicker
  between layouts.
- **Copying the recorded text, override characters included.** Rejected. A
  bidirectional override can make a pasted command differ from what the
  operator read.
- **A Tauri clipboard plugin.** Rejected. The manifest has no plugins, and the
  web clipboard API needs no new command or capability.

## Consequences

- Replies are easier to read, and a command can be copied with one click.
- The model chooses the formatting. A reply can make its text look like a
  heading, a quote, or a code block the operator did not write. The copy
  button copies whatever command the model wrote; the operator still decides
  whether to run it, and running it is outside OMGCode.
- Zero-width and other invisible characters are neither marked nor removed,
  so they are copied too. The display-safe escaping limits in the desktop
  contract still apply.
- The parser is a subset. Setext headings, indented code blocks, reference
  links, HTML blocks, autolinks, and task-list checkboxes are not recognised
  and stay text. A link destination ends at the first `)`, so a destination
  that itself holds parentheses is cut short and the rest shown as text.
- Copying has been checked under jsdom with a stubbed clipboard, and as a
  refused write in a Chromium browser pane, which shows "Copy failed". A
  successful copy in the WebView2 or WebKitGTK shell has not been exercised.
  `http://tauri.localhost` is expected to be a secure context in WebView2;
  whether WebKitGTK treats `tauri://localhost` as one is unverified.

## References

- [Desktop interface](../desktop-interface.md)
- [Security coding rules](../security-coding-rules.md)
- [ADR-0049: Stream local model replies and push chat events](0049-stream-local-model-replies-and-push-chat-events.md)
- [ADR-0065: Redesign the desktop windows with one dark theme and split layouts](0065-redesign-the-desktop-windows-with-one-dark-theme-and-split-layouts.md)
- [CommonMark specification](https://spec.commonmark.org/)
- [GitHub Flavored Markdown tables](https://github.github.com/gfm/#tables-extension-)
- [Clipboard API `writeText`](https://developer.mozilla.org/en-US/docs/Web/API/Clipboard/writeText)
