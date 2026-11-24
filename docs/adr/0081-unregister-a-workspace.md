# 0081: Unregister a workspace

- Status: Accepted
- Date: 2026-11-24
- Amends: [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md),
  [ADR-0080](0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)

## Context

An operator reported that there is no way to clear a workspace from the tool.
Registration was one-way: no core, CLI, or desktop path removed a registration.
[ADR-0057](0057-import-skills-into-a-private-registry-and-load-them-on-explicit-selection.md)
noted that the control journal had no workspace removal, so no skill override
could be dropped that way. [ADR-0080](0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)
required that any change adding unregistration also remove the workspace's
instructions file under the preferences lock, so no file is left behind and no
later turn sends its text.

A workspace identifier appears in several parts of the control state and
beside it, so removal has to decide what happens to each reference.

## Decision

Add the control-journal event `workspace_unregistered`, recorded by
`ControlStore::unregister_workspace(workspace_id, expected_revision)`. It is a
configuration change: it needs the expected revision and increments it, like
`register_workspace`. Replay validates it against the preceding state: the
workspace must be registered and no container grant may name it.

Each reference to the workspace identifier is handled as follows.

| Reference | Choice |
| --- | --- |
| Registration and its permission preferences | Dropped by the event. |
| Package cache root selection | Dropped. Cached files stay on disk, as after `clear-package-cache`. |
| Skill workspace overrides | Dropped. Each counts as an enablement change for its skill, so a turn that loaded the skill sees it withdrawn. |
| Bound `.OMGCode` checkpoint folder record | Dropped, as `forget_workspace_store` does. Nothing on disk changes: the folder, its ledger, and the checkpoints stay. |
| Container grants (any state) | Refused with a controlled `grant refused` error. The operator revokes them first with `workspace container-revoke`, so no ACL is stranded on a folder that no record names. |
| Approval records | Kept as history. An approval needs a live session binding to be consumed, a binding needs a registered workspace, and the revision change already invalidates every outstanding approval. The approvals window shows a request only when core reproduces its digest, which gives `NotFound` for an unregistered workspace. |
| Package fetch start and finish records, policy-check records | Kept as audit history. They never grant anything and replay does not need the registration. |
| Revocation counters (grants, `model_tools`) | Dropped with the workspace. They are derived on replay. A started run of the workspace is withdrawn by the existing rule "the workspace is no longer registered". |
| Toolchain, package-network, and provider profiles | Not workspace-scoped. Unchanged. |
| Session bindings | Process-local and tied to the revision. Invalid after the change. |
| Run, action, and file-audit journals; checkpoint ledgers | Separate files that record history. Unchanged. |
| Sessions recorded in the workspace | Journals and index entries stay, with their recorded `workspace_id`. |

The desktop and the CLI both remove the workspace's saved instructions file
(`desktop-v1/workspace-instructions/<id>.json`) after the event is recorded.
The `settings` window does it under the service's preferences lock, taken
before the settings store as every other save does, so the deletion never
overlaps a save or a turn's read. The lock lives in one process, so the CLI
deletes the file without it. To make "no later turn sends its text" hold
without cross-process locking, `chat_send` now reads a workspace's
instructions file only while current settings register that workspace. A file
that survives a failed deletion, or a save that raced with a CLI removal, is
never sent.

Registering the same folder again assigns a new identifier
(`WorkspaceId::fresh()`) and the default preferences. An identifier is never
reused. Sessions recorded under the old identifier therefore stay matched to no
registration for good.

Such a session stays readable. A later turn in it is not refused: the existing
tool-mode rule gives text-only chat with the reason `workspace_unregistered`
(`tool_mode::select`), the workspace's instructions are not sent, and the
desktop shows the reply as it shows any other. This is the behaviour that
existed for a session whose registration was missing, and this record does not
change it. The confirmation says that sessions "can chat without tools only";
it does not call them read-only.

Surfaces:

- Trusted CLI: `omgcode workspace unregister <workspace-id> --revision <n>`.
  After the event it stops withdrawn runs, deletes the instructions file, and
  prints the new snapshot. A deletion failure is reported as `preferences`
  after the change is recorded.
- Desktop: the `settings` window command `settings_unregister_workspace
  { workspace_id, revision }`, with `workspace_id` and `revision` only. Rust
  refuses an unknown workspace, a stale revision, a container grant, and a
  running chat turn in any session of the workspace, before any dialog. A
  native confirmation names the folder and states what is removed and what is
  not touched. The running-turn check is repeated after the dialog. A turn
  that starts after the second check still loses its tools, because each tool
  call reads live settings.
  The command is in the `settings` capability only. The shell then has 43
  commands, 15 of them for `settings`.
- Desktop UI: **Remove workspace…** at the end of the workspace block in the
  full Workspaces view. The scoped **Workspace settings** view has none.

A control journal that records `workspace_unregistered` cannot be read by
0.55.0 or earlier, which reject the unknown event name. No migration or
downgrade path exists.

## Alternatives considered

- **Tombstone the registration instead of removing it.** It keeps a
  registration that can never be used, and its identifier still counts against
  the workspace limit. Rejected.
- **Reuse the old identifier on re-registration.** It would silently re-attach
  old sessions, preferences history, and approvals to whatever folder is
  registered later. A new identifier keeps authority tied to one registration.
  Rejected.
- **Revoke container grants as part of the event.** Revocation changes ACLs
  through native calls and can fail halfway; the event must be a pure journal
  change. The operator already has an explicit revoke command. Rejected.
- **Refuse chat in sessions of an unregistered workspace.** It would strand the
  conversation history that the operator kept on purpose. Text-only chat grants
  nothing. Not changed here.
- **Take the preferences lock in the CLI.** The lock is an in-process mutex.
  A cross-process lock for one deletion is not justified when `chat_send`
  already ignores a file whose workspace is not registered.
- **Delete the instructions file before the event.** A stale revision or a
  grant refusal would then lose the text of a workspace that stays registered.
  Rejected; the event comes first.

## Consequences

- An operator can clear a workspace from the CLI and the desktop. Its folder,
  its `.OMGCode` folder, its fetched-package cache, and its sessions stay.
- A removal is refused while a chat turn runs in the workspace or a container
  grant names it.
- An older build cannot open the control journal after the event is recorded.
- `chat_send` opens the settings store once more per turn to check that the
  session's workspace is registered before it reads the instructions file.
- Approval, package-fetch, and policy records for the removed workspace stay
  in the journal and in `workspace status` history until the journal is
  compacted by a future change; this record adds no compaction.
- The flow was tested with the scripted prompt and loopback model server, the
  CLI through real processes, and the window under jsdom. It has not been
  checked in the running Tauri shell.

## References

- [Workspace settings](../workspace-permissions.md#unregistering-a-workspace)
- [Desktop interface](../desktop-interface.md)
- [Skills](../skills.md)
- [Session storage](../session-storage.md)
- [ADR-0057](0057-import-skills-into-a-private-registry-and-load-them-on-explicit-selection.md)
- [ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md)
- [ADR-0070](0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md)
- [ADR-0080](0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)
