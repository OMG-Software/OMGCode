# 0090: Set every file-write tool limit at once

- Status: Accepted
- Date: 2026-11-24

## Context

ADR-0063 gave each model file-write tool its own Deny, Prompt, or Allow limit,
and ADR-0079 added a sixth. The desktop `settings` window shows the six limits
in the **File-write tool limits** disclosure under Write, one control each. An
operator who wants every tool to ask first, or none to run, has to change six
controls, and each change is a separate request with its own revision and, when
it widens, its own native confirmation.

## Decision

1. A **Set all** control sits on the summary line of **File-write tool limits**.
   It is the same Deny, Prompt, or Allow segmented control as the rows, with
   the accessible name "All file-write tools".
2. It is placed beside the summary, not inside it. A `<summary>` has a button
   role, and nesting a radio group in it gives an invalid accessibility tree.
   Outside the summary, the control also works while the disclosure is closed.
   The summary keeps room on its right so its text wraps before reaching the
   control.
3. Choosing a state sends one `settings_set_permissions` request: the whole
   preference set, with all six limits at that state and every other
   preference, including `write`, as recorded. The frontend names all six keys
   in an object literal, so a seventh tool is a type error until it is added.
4. Rust is unchanged. It already compares the whole set and composes one native
   confirmation that names each widened preference with its old and new state.
   A bulk widening is therefore confirmed once, with every widened limit listed.
   Narrowing needs no confirmation, as before.
5. The control marks a state only while all six limits share it. While they
   differ, it marks none, and every choice sends a change. A choice equal to
   the shared state sends nothing, like any segmented control.
6. The shared segmented control accepts `null` as its value for that mixed
   state. It then marks no choice and gives the first choice the tab stop.

## Alternatives considered

- Put the control inside the `<summary>`: rejected for the nested interactive
  role, and because a click or key press there competes with the disclosure's
  toggle.
- Add a seventh row inside the disclosure: rejected; the operator asked for the
  control next to the heading, and a row inside is hidden while it is closed.
- Make **Set all** also change `write`: rejected. A tool limit can only narrow
  `write` (ADR-0063), and changing `write` from a tool-limit control would
  widen access the operator did not name. The rows still show "Limited by
  Write:" where a limit is wider than `write`.
- Send six requests, one per tool: rejected. Each would need its own revision
  and confirmation, and a refusal partway through would leave a mix the
  operator did not choose.
- Add a "Mixed" choice: rejected; it is not a state core accepts.

## Consequences

- Setting all six limits takes one choice and at most one confirmation.
- Core, the wire format, the stored settings, and the CLI are unchanged; the
  CLI still sets each limit with its own flag.
- The settings window has one more Deny, Prompt, or Allow group per shown
  workspace.

## References

- [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md)
- [ADR-0079](0079-add-a-model-tool-that-creates-one-directory.md)
