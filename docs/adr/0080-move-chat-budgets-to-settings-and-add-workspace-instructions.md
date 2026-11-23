# 0080: Move chat budgets to settings and add workspace instructions

- Status: Accepted
- Date: 2026-11-23
- Implemented: version 0.54.0
- Amends: [ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md) decision 8,
  [ADR-0068](0068-name-replies-by-model-and-copy-them-as-markdown-text-or-image.md),
  [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md),
  [ADR-0076](0076-configure-the-tool-call-budget-per-turn.md), and
  [ADR-0077](0077-configure-the-tool-enabled-turn-timeout.md)

## Context

The composer's **Chat settings** menu held every chat value: context size,
output reserve, tool-call limit, turn timeout, custom instructions, the
compaction threshold, auto-compact, and the send key. Most of these describe
the model connection, not one message, and the operator asked to configure
them next to the model connection. One instruction text could not say
anything specific to a workspace. The compaction threshold was a free number
field. The reply copy buttons and **Workspace settings** were text buttons
that crowded the toolbar.

## Decision

1. The `settings` window's **Model** section has **Chat defaults** between
   the active connection and the connection list: context tokens, max output
   tokens, tool calls per turn, the tool-enabled turn timeout, and **Global
   custom instructions**. `settings_save_chat_defaults` saves them. It shows
   no native dialog and names no settings revision, because none of them is a
   permission.
2. Each workspace in the **Workspaces** section has **Custom instructions**,
   saved by `settings_set_workspace_instructions` for a registered workspace
   only, in `<data_root>/desktop-v1/workspace-instructions/<id>.json`. Empty
   text removes the file. Each save keeps the global text and every
   workspace's text sendable together within 16 KiB, the core's bound on
   custom instructions, with the headings of decision 3: the workspace save
   checks against the saved global text, and the global save against the
   longest saved workspace text, both under the preferences lock. Empty text
   needs no room and skips the check. A damaged other text counts as empty;
   one that cannot be read now makes the room unknown, and a save of
   non-empty text is then refused. The overview reports each remaining room
   by the same rule, `null` when unknown, which the window's counters show,
   so text the window accepts never makes a turn fail.
3. `chat_send` composes the turn's custom instructions in Rust. The window
   sends the global text; Rust reads the session's recorded workspace's text.
   With no workspace text the global text is sent unchanged, so manifests keep
   their earlier shape. Otherwise the message reads `Global instructions:`,
   the global text, a blank line, `Workspace instructions:`, and the workspace
   text, with an empty part left out. A composition over 16 KiB is refused
   with `invalid_request`, and an unreadable workspace file with
   `instructions_unreadable`, so a turn never goes out silently without the
   operator's text. Both texts stay one labelled user-level message of
   untrusted origin. They grant nothing.
4. The composer keeps **Auto-compact**, **Enter to send**, and **Save as
   defaults**. The compaction threshold is a slider from 128,000 to 1,000,000
   tokens in steps of 8,000. `chat_preferences_save` accepts only these three
   fields and refuses a threshold outside the range. The composer reads the
   other values from `chat_defaults` when it starts, when the window gains
   focus, and again before each send, and sends them in `chat_send`, whose
   wire shape is unchanged.
5. Both windows write `chat-preferences.json`. Each save loads the file,
   changes only its own fields, and writes it back while holding one service
   lock, so neither window's save overwrites the other's fields. Neither saves
   the model name; the model is chosen per session (ADR-0071).
6. **Copy Markdown**, **Copy text**, **Copy image**, and **Workspace
   settings** are icon buttons. The name is the accessible name and the
   native tooltip. After a copy, a tick or a cross and "Copied" or "Copy
   failed" replace the icon and name for 1.5 seconds. **Workspace settings**
   sits beside **Send**.

## Alternatives considered

- **Rust reads every budget from storage in `chat_send`.** This makes the
  saved file the only source, but it changes the `chat_send` wire shape and
  every test and caller that sends settings. Reading the defaults just
  before each send gives the same result for this window. Rust still
  validates every value.
- **Keep the workspace instructions in the settings store.** That store holds
  permissions with revisions and native confirmations. Instructions grant
  nothing, so they stay in desktop-only files beside the chat preferences.
- **Send the workspace instructions from the window.** The window would have
  to resolve the session's workspace, which only Rust does reliably
  (ADR-0071). Rust reads them at send time instead.
- **A logarithmic slider or free entry for the threshold.** Free entry allowed
  values that compact on every turn. A linear slider with 8,000-token steps
  reaches the 200,000 default exactly and is enough at this range.
- **Bound each text alone, less the headings.** It keeps one text from
  failing alone, but a long global text and a long workspace text still fail
  together on every turn. Checking the pair on each save keeps every accepted
  combination sendable.
- **Text buttons with shorter labels.** Icons with tooltips keep the reply
  row and the toolbar compact and still name each action for assistive
  technology.

## Consequences

- A saved threshold below 128,000, such as an older default, is shown and
  sent at 128,000. Compaction still triggers at the model's safe input when
  that is smaller, so a small model compacts as before. A larger model
  compacts later than with an older low threshold.
- Global instructions saved before this change may be up to 64 KiB. They load,
  but a turn refuses them, as the core did before. A new save accepts at most
  16 KiB.
- A workspace file that cannot be read blocks that workspace's sessions from
  sending until the operator saves its instructions again.
- No desktop, CLI, or core path unregisters a workspace, and a session is
  created only in a registered one, so `chat_send` reads a session's
  workspace file by its recorded identifier. A change that adds
  unregistration must also remove that workspace's instructions file under
  the preferences lock, so no file is left behind and no later turn sends
  its text. Any new writer of these files must take the same lock and the
  same pair check (decision 2).
- The settings window gained two commands (42 in all, 14 for `settings`).
- The composition was tested with a scripted loopback model server, and the
  windows under jsdom. It has not been checked with a real model.

## References

- [Desktop interface](../desktop-interface.md)
- [Agent instructions](../agent-instructions.md)
- [ADR-0062](0062-send-the-desktop-message-on-enter-as-a-saved-chat-preference.md)
