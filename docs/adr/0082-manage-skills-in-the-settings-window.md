# 0082: Manage skills in the settings window

- Status: Accepted
- Date: 2026-11-23

## Context

[ADR-0057](0057-import-skills-into-a-private-registry-and-load-them-on-explicit-selection.md)
made skill import and the registry trusted CLI operations, and
[ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
and [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
kept skills, toolchains, package-network profiles, and container grants out
of the desktop `settings` window, so the **Advanced** section only showed
them. An operator reported that nothing there can be added, edited, or
removed, and that they should at least be able to install skills.

Enabling a skill is consent to make its text loadable into a chat turn as a
labelled untrusted message (`docs/skills.md`). That grants no permission, but
it decides what reaches the model, so it needs the same native confirmation
the window uses for other widening changes. Importing copies a validated
folder and registers it disabled, and nothing in it runs.

## Decision

1. **The settings window imports, enables, disables, and removes skills.** It
   uses three commands, all `settings`-only, each taking the revision the
   window read: `settings_import_skill`, `settings_set_skill_state`, and
   `settings_remove_skill`. Each calls the same `ControlStore` operation as the
   CLI (`import_skill`, `set_skill_enablement`, `remove_skill_and_package`)
   from the application service, so there is one validation and one audit
   path. The window adds no second rule.
2. **No path crosses the webview.** Import takes only the revision. Rust
   opens the native folder dialog (a new `FolderPurpose::Skill`) and passes
   the chosen folder to core. A skill is addressed by its validated name, a
   workspace by its registered identifier.
3. **Import is not confirmed after the folder choice.** The skill arrives
   disabled, nothing is run, and choosing the folder is the operator's act.
   Core's refusals return as controlled errors with core's stable code. The
   message is core's static text or fixed text; a message never carries a path
   or a file or folder name from the source, because those come from disk.
4. **Enabling is confirmed natively; disabling is not.** Rust composes the
   confirmation from fixed text, the validated name, the scope (every
   workspace without its own setting, or one workspace's folder), and the
   description as a bounded display-safe line. It asks before any change that
   leaves the skill enabled at the scope and did not before: `enabled`, and an
   `inherit` whose result is enabled because the application setting is. It
   skips the question when the skill was already enabled at that scope, and for
   every disable. The check and the dialog use the revision read before the
   dialog, and the change applies with it, so a change made meanwhile is
   refused as `stale_revision`.
5. **Removing is confirmed natively and says what happens to the files.**
   Core records the removal first and then deletes the stored copy under
   `<data_root>/skills-v1/packages/<skill_id>/`; the source folder is never
   touched. The confirmation says so. When the copy cannot be fully deleted,
   the removal stands, the copy is inert, and the command fails with
   `package_orphaned` rather than reporting success; the window reads the
   settings again and shows the skill gone.
6. **The other three groups stay CLI-only for now.** Toolchain profiles,
   package-network profiles, and container grants configure what a run may
   read, execute, fetch, or be granted on the host. Each needs its own design
   for typed paths, origins, and native confirmations, and none is a
   consequence of importing text. They remain read-only in the window, and its
   help text says so, until a separate decision covers them.
7. **The overview is unchanged.** The window shows a skill's name and its
   settings. A description or body is not returned, so no untrusted package
   text is rendered in the webview; the confirmation shows a bounded,
   display-safe description natively.

## Alternatives considered

- **Take a folder path in the webview.** It would let a compromised window
  name any folder. Rejected for the reason in ADR-0059 decision 4.
- **Confirm every import.** The import grants nothing and starts disabled, so
  a second dialog after the folder dialog adds friction without a decision.
  The confirmation sits where the text becomes loadable.
- **Confirm only an explicit Enable.** An `inherit` that ends enabled has the
  same effect, so it is confirmed too.
- **One command with a union of actions.** Three commands keep each argument
  list typed, each permission named, and the layer tests exact.
- **Open the other three groups in the same change.** Rejected for scope and
  risk (decision 6).
- **Report a removal with an undeleted copy as success with a warning.** The
  overview has no warning field, and a silent success hides an orphaned copy.
  An error after a recorded change is the pattern the credential commands
  already use.

## Consequences

- The `settings` window gains three commands; the capability file, the
  policy enum, the shared name list, and the layer tests were updated
  together. With ADR-0081 to ADR-0089 merged in version 0.56.0, the shell has
  56 commands, 24 of them in the `settings` window; the "Commands" table in
  the desktop interface guide is the current count.
- A compromised settings webview can disable or remove skills, and can ask for
  an import or an enable, but it cannot choose a folder or answer the native
  dialog.
- Enabling still needs the `main` window's per-turn selection before any skill
  text reaches a model, and the turn rechecks the registry (ADR-0057).
- Skill updates, Git sources, and a view of a skill's description and body
  are still not implemented.
- The native dialogs and the real shell window were not driven by automation;
  the service tests use a scripted prompt, and nothing ran on Linux.

## References

- [Skills](../skills.md)
- [Desktop interface](../desktop-interface.md)
- ADR-0057, ADR-0059, ADR-0060
