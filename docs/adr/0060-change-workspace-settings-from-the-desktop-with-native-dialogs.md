# 0060: Change workspace settings from the desktop with native dialogs

- Status: Accepted
- Date: 2026-11-19
- Implemented: version 0.41.0 (ADR-0059 phase S2)

## Context

[ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
plans five phases for the desktop `settings` window. Phase S1 (0.40.0) made
the window read-only. In practice an operator who opened it saw "No settings
store exists yet. Run omgcode workspace status to create it." and had to use
the CLI for everything. The user reported both problems: the window should
change settings, and no command should be needed to create the store.

Phase S2 covers the development root, workspace registration, workspace
preferences, and package cache roots. ADR-0059 fixes the security shape: a
native folder dialog that Rust opens supplies every path, and a native
confirmation precedes every change that widens access. It names the Tauri
dialog plugin as the planned implementation and leaves the dependency to be
reviewed in this phase. Three choices remained:

1. How Rust opens the dialogs.
2. When the settings store is created.
3. Which workspace preferences the window changes. ADR-0059's table lists
   four; the CLI has six.

## Decision

1. **Open native dialogs with `rfd` from Rust, without a Tauri plugin.** The
   shell depends on `rfd` 0.17.2 (MIT) with default features off and the
   `gtk3` feature on. On Windows it uses `IFileOpenDialog` and a message box
   through `windows-sys`. On Linux it uses the GTK 3 that Tauri already links,
   not the XDG-portal backend and its D-Bus stack. The lockfile gains only the
   `rfd` package; its dependencies were already present. Its build script
   only checks the selected features.
   - No dialog command exists, so no webview can call one. The Tauri dialog
     plugin would register `plugin:dialog|open`, `|message`, and others. They
     would be refused only by the capability layer, and its `tauri-plugin-fs`
     dependency would add more commands to refuse.
   - A tested rule keeps every Tauri plugin out of the manifest and out of
     `lib.rs`, and allows only the `settings` handlers to open dialogs.
   - The service defines a `SettingsPrompt` trait: pick a folder, or confirm
     a composed message. The shell implements it with `rfd`, modal to the
     calling `settings` window. Tests implement it with a scripted prompt.
2. **Create an empty settings store on first read.** `settings_overview`
   opens the store with `ControlStore::open`, which creates it when missing,
   instead of `open_existing`. This is the same operation as the CLI's
   `workspace status`. The new store records its identity at revision zero:
   no root, workspace, or permission. The `settings` window loads hidden at
   launch and reads the overview at once, so the store exists from the first
   launch. The `store_initialized` field and the window's "no store" state
   are removed.
3. **The window changes all six preferences.** It changes `model_tools`,
   `read`, `write`, `execute`, `task_network`, and `host_services`, so the
   desktop matches the CLI's `workspace permissions`. A request carries the
   complete set, and every flag is required.
4. **Each change follows one order.**
   - Read the store. Check the caller's revision, and refuse a stale one with
     `stale_revision` before any dialog.
   - Release the store. No settings lock is held while an operator decides,
     so the CLI and run watchers are never blocked on a dialog.
   - Show the folder dialog where one is needed. Then show the confirmation
     where the change widens access. The service composes the confirmation
     text from fixed words and core's display-safe values. Each preference
     turned on is named with what it allows.
   - Reopen the store and apply the change with the same revision. If any
     change happened meanwhile, core refuses it, so what the operator
     confirmed is exactly what is applied.
   - Before releasing the store, list the pending runs that the new settings
     withdraw. Then stop and observe those runs and any others the settings
     withdraw. A change that restores access in between cannot hide them. A
     run not observed to stop fails the command with `runs_not_stopped`, and
     the change stays recorded.
   - Allow one change at a time per service. A second request while a dialog
     is open fails with `settings_change_running`.
5. **Five commands, for `settings` only.** They are `settings_choose_root`,
   `settings_register_workspace`, `settings_set_permissions`,
   `settings_choose_package_cache`, and `settings_clear_package_cache`. Each
   returns the new overview. None takes a path. Turning preferences off and
   clearing a cache root narrow access, so they show no dialog. Clearing
   leaves cached files in place, so it deletes nothing and needs no in-window
   confirmation either.
6. **The `main` window points to the settings window.** When no workspace is
   registered, the new-session dialog offers **Open settings**. That button
   calls the existing `settings_show_window` and no new command.

## Alternatives considered

- **The Tauri dialog plugin, as ADR-0059 planned.** Rejected. It registers
  dialog commands that only the capability layer would refuse. It pulls in
  `tauri-plugin-fs`. It offers nothing for Rust-only use that `rfd` does not
  already provide, since the plugin is built on `rfd`.
- **`rfd` with its default XDG-portal backend on Linux.** Rejected. It adds
  `ashpd` and a D-Bus stack, and Tauri already links GTK 3.
- **Create the store when the service opens.** This is equivalent in practice,
  because the settings window reads it at launch. It was not chosen because
  it would change every service test that relies on a missing store, and the
  settings window is the component that owns configuration.
- **Keep the "run `omgcode workspace status`" hint.** Rejected by the user. An
  empty store grants nothing, so there is no reason to make the operator
  create it.
- **Hold the settings store while a dialog is open.** Rejected. A slow
  operator would block the CLI, run watchers, and the revocation of
  withdrawn access. The revision check gives the same guarantee without the
  lock.
- **Change only the four preferences ADR-0059 lists.** Rejected. It would
  leave `task_network` and `host_services` CLI-only for no security gain.
  Turning either on needs the same confirmation as the others.

## Consequences

- The operator can configure the development root, workspaces, all workspace
  preferences, and package cache roots in the desktop application.
  Language-model connections (S3), execution setup (S4), and skills (S5)
  remain CLI-only.
- The shell has 28 commands: 19 for `main`, 3 for `approvals`, and 6 for
  `settings`.
- A compromised `settings` webview can turn preferences off, clear cache
  roots, and open dialogs repeatedly, one at a time. It cannot choose a
  folder, answer a dialog, or widen access, because every widening change
  waits for the operator's native answer.
- On Windows the confirmation's default button is Yes, because `rfd` offers
  no way to change it. A keyboard Enter confirms. The box is modal to the
  settings window and names the folder and each preference.
- The Linux dialog path is untested. `rfd` starts its own thread that
  initialises GTK and iterates the default GLib main context, and queues each
  dialog there. Tauri's event loop also initialises GTK and iterates that
  context on another thread, so a dialog may run on either. No window has yet
  been opened on Linux (ADR-0045).
- Display-safe text in a confirmation still does not detect look-alike
  letters or mixed scripts (ADR-0046).
- A dialog left open during shutdown is not waited for. The process ends
  with it, and no change is applied.

## References

- [ADR-0045](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
- [ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)
- [ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
- [Desktop interface](../desktop-interface.md)
- [Workspace settings](../workspace-permissions.md)
- [`rfd` documentation](https://docs.rs/rfd/0.17.2/rfd/)
