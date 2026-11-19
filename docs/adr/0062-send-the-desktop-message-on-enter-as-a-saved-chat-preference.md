# 0062: Send the desktop message on Enter as a saved chat preference

- Status: Accepted
- Date: 2026-11-19

## Context

The desktop composer sent a message only on Ctrl+Enter (Cmd+Enter); Enter
added a newline. Most chat interfaces, including coding assistants, send on
Enter and add a newline on Shift+Enter. The operator asked for that behaviour
by default, with an **Enter to send** option that can turn it off.

The option is a keyboard choice of the `main` window. It changes no request,
permission, or turn. Two places could hold it:

- The isolated `settings` window owns trusted configuration: the development
  root, workspace registrations and preferences, and package cache roots
  ([ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md),
  [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)).
  Its changes are revisioned, audited, and some need a native confirmation.
- ADR-0059 decision 8 gives the `main` window saved, non-authority chat
  preferences in `<data_root>/desktop-v1/chat-preferences.json`, edited in the
  composer's settings panel and saved with **Save as defaults**.

The webview may not use `localStorage` or `sessionStorage`; the frontend
security scans refuse both.

## Decision

1. **Enter sends by default.** With **Enter to send** on, a plain Enter sends,
   and Shift+Enter or Alt+Enter keeps the textarea's newline. With it off,
   Enter adds a newline. Ctrl+Enter (Cmd+Enter) sends in both modes, so the
   old shortcut keeps working.
2. **An input-method Enter never sends.** A `keydown` with `isComposing`, or
   with `keyCode` 229, is ignored. WebKit can report the Enter that confirms
   a composition after the composition ended, with only `keyCode` 229 to show
   it. This is checked against synthetic events only; no input method was
   used.
3. **A send key never adds a newline.** It is consumed even when the message
   cannot be sent, and the composer shows why, as the Send button's check
   does.
4. **The choice is a chat preference.** `ChatPreferencesDto` gains
   `enter_to_send`. It is saved with the other chat preferences through the
   existing `chat_preferences_save` command, in the same file, with the same
   strict parsing and atomic replace. `chat_defaults` returns it as a
   top-level `enter_to_send` field, not in `settings`, because it is not a
   turn setting and `chat_send` never receives it.
5. **Missing means on.** A file or request without the field reads it as
   `true`, so a 0.41.0 file still loads. The file format version stays 1:
   the change only adds an optional field. An older build reads a newer file
   as unreadable (unknown field), shows its note, and uses its built-in
   values until a save replaces the file.
6. **The check box applies at once.** It is in the composer's settings panel,
   renamed from **Model settings** to **Chat settings**. **Save as defaults**
   keeps it for later runs, as for the other chat preferences.

No command, capability, window permission, or core rule changes.

## Alternatives considered

- **Put the option in the `settings` window.** Rejected. That window holds
  trusted, revisioned configuration and opens native confirmations. Adding a
  keyboard choice there would mix a preference that grants nothing into the
  trusted store, add a command and capability, and need a way to tell `main`
  that the choice changed.
- **A separate preferences file or command.** Rejected. The chat preferences
  file already has the needed bounds, strict parsing, and atomic replace. A
  second file would repeat that code for one boolean.
- **Keep it in the webview only.** Rejected. `localStorage` is forbidden by
  the frontend security rules, and an unsaved choice would reset each run.
- **Bump the file format to version 2.** Rejected for now. Readers of version
  1 would refuse version 2 in the same way as an unknown field, and accepting
  both versions adds code without changing what an older build can read.
- **Make Enter send and drop Ctrl+Enter.** Rejected. Ctrl+Enter is the
  documented shortcut until this change, and keeping it costs nothing.

## Consequences

- Enter sends in the desktop composer unless the operator turns it off.
- A preferences file saved by this version is unreadable to 0.41.0 and
  earlier. Those builds fall back to their built-in values and say so; they
  do not lose or partially apply the file until they save over it.
- The composition guard rests on `isComposing` and `keyCode` 229. It has not
  been tried with a real input method in WebView2 or WebKitGTK.
- The real shell's `main` window was not driven for this change. Unit, main
  window, and service tests, and the Vite development server with the mock
  backend, cover it.

## References

- [Desktop interface: chat](../desktop-interface.md#chat)
- [ADR-0059: Configure trusted settings in an isolated desktop settings window](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
- [UI Events: `KeyboardEvent.isComposing`](https://www.w3.org/TR/uievents/#dom-keyboardevent-iscomposing)
- [UI Events: key codes during composition](https://www.w3.org/TR/uievents/#determine-keydown-keyup-keyCode)
