# 0065: Redesign the desktop windows with one dark theme and split layouts

- Status: Accepted, amended by [ADR-0095](0095-use-the-omg-software-palette-and-omgcode-icons-in-the-desktop.md)
- Date: 2026-11-19
- Implemented: version 0.45.0

## Context

The desktop shell grew one feature at a time, and its three windows showed it.

- In the `approvals` window the DOM order was the pending list, then up to 50
  recently decided records, then the review panel. Every decided record pushed
  Approve and Deny further down, so after a busy session the operator scrolled
  past old decisions to reach the request they had just selected. Edits were
  shown as separate old and new text blocks, not as a difference.
- The `settings` window was one long page of eight sections. Each workspace
  card held eleven inline selectors that wrapped at uneven widths, with the same
  help paragraph on every card. The model connection appeared in three places.
  Four sections were read-only data that only the trusted CLI changes, shown
  with the same weight as the controls.
- The `main` window crowded six controls into the sidebar header, gave raw tool
  arguments the same weight as conversation, and stacked an approval banner,
  chat settings, skill checkboxes, and the status bar under the composer.
- The palette followed the system light or dark preference and had no contrast
  check.

The redesign must keep every security property of the shell. The three windows
load separate bundles, and the `main` bundle must never contain a decision or
settings command ([ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md),
[ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)).
Approve is enabled one second after a review renders and is never the default
focus. Untrusted text is rendered only as text nodes, with no inline style,
web storage, or remote resource.

## Decision

Redesign all three windows in one change, keeping their commands, IPC, and
authority unchanged. The [desktop interface guide](../desktop-interface.md)
owns the resulting layout contract.

- **One dark theme.** `tokens.css` defines a single dark palette of near-black
  greys with an orange accent and declares `color-scheme: dark`. The light
  palette is removed. `tests/tokens.test.ts` checks that every text colour
  reaches WCAG AA contrast (4.5:1) on every surface and that control borders
  and the focus colour reach 3:1. Rust opens each window with the same
  background colour, so a window does not flash white before the page paints.
- **Orange means action, not permission.** Orange marks primary actions,
  focus, selection, and "waiting for you". A permission at Allow is drawn solid
  neutral and Prompt outlined in orange, so the state that runs without asking
  never looks like the recommended choice. Each state also shows its word.
- **Shared DOM-only components.** `src/ui/` holds a segmented control with
  radio-group semantics, a tab strip, and a `<details>` popover. They import no
  IPC code, so any bundle can use them without breaking bundle isolation. The
  segmented control never changes itself; the window re-renders it from the
  state the core returns.
- **Approvals as a queue beside a fixed review.** Pending requests and decided
  history are separate tabs, and the review pane sits beside the queue with the
  decision row always visible. The next pending request opens after each
  decision. Each newly shown review starts with the writers-stopped box
  unticked and its own one-second Approve delay. Edits are shown as a bounded
  line difference.
- **Settings in three sections.** Model, Workspaces, and a read-only Advanced
  section, chosen from a sidebar. Each workspace permission is one row with a
  Deny, Prompt, or Allow segmented control. The labels stay those of the CLI
  and ADR-0063.
- **A quieter main window.** Search and a View menu replace the crowded sidebar
  header; the approval banner becomes a status-bar item; tool calls and notes
  are compact rows; skills and chat settings move into composer popovers.
- **Wider secondary windows.** The `approvals` and `settings` windows open at
  960 by 720 pixels, with a minimum of 600 by 480, to fit two columns.
- **CSS checks that read CSS.** Vitest now processes CSS (`css: true`), because
  it otherwise replaces each CSS module with an empty string. Before this change
  the remote-resource scan of the stylesheets read empty strings; it now
  asserts that each stylesheet is non-empty.

## Alternatives considered

- **Approval requests as a popup over the chat.** Rejected. The `main` bundle
  would have to contain `approval_decide` and `approval_detail`, which breaks
  the isolation ADR-0046 relies on. The status bar and the transcript can only
  say that a request is waiting and open the `approvals` window.
- **A single-key Approve shortcut.** Rejected. It would bypass the rule that
  Approve is never the default focus and only enables after a delay. Approve
  needs a click, or Tab to the button and Enter. No Deny shortcut was added
  either.
- **Keeping a light theme beside the dark one.** Rejected at the operator's
  request. A second palette would need its own contrast checks and doubles the
  styling surface of a prototype.
- **Renaming Prompt to Ask in the desktop shell.** Rejected. The stored values,
  the CLI, and ADR-0063 use `prompt`; two words for one state would confuse
  operators who use both surfaces.
- **One pull request per window.** Rejected at the operator's request in favour
  of one change made as a sequence of commits that each pass the checks.

## Consequences

- The redesign changes no command, IPC shape, permission, or Rust authority
  check. Its risk is in presentation and in the frontend state of each window,
  which the window tests cover.
- The `approvals` window now opens a review without a click: on opening and
  after the selected record stops being pending. For a file-write approval
  that review makes the same audited broker read as before, so reads now also
  happen at those moments, still one detail request per shown review (one
  read, or two for a rename). Each shown review also issues a review token,
  within core's limit of 64 outstanding.
- The operator can no longer choose a light appearance. Native folder and
  confirmation dialogs still follow the operating system's theme.
- The colour contrast test checks token pairs, not every rendered combination;
  it does not prove the whole interface meets WCAG.
- The layouts were checked against the mock backend in a browser and by the
  window tests. They were not driven by automation in the Tauri shell; see
  the desktop interface guide's known limits.

## References

- [Desktop interface](../desktop-interface.md)
- [ADR-0046: Decide model tool approvals in an isolated desktop window](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)
- [ADR-0059: Configure trusted settings in an isolated desktop settings window](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
- [ADR-0063: Deny, prompt, or allow each preference](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [WCAG 2.2 contrast (minimum)](https://www.w3.org/TR/WCAG22/#contrast-minimum)
