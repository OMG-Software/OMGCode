# 0073: Remove extra data folder grants after operator confirmation

- Status: Accepted
- Date: 2026-11-20
- Implemented: version 0.52.0

## Context

Adding an Ollama Cloud key from the desktop `settings` window failed with
`credential_unavailable`: "No protected credential store is available on
this computer." The operator's data folder,
`%LOCALAPPDATA%\OMG-Software\OMGCode\data`, inherited a read grant for a
local group named `CodexSandboxUsers`. Another tool on the same computer had
created that group. `AppData\Local` itself no longer had the grant, so
the folder kept an old inherited copy.

ADR-0023 lets the data root grant only the current user, SYSTEM, and
Administrators. `DpapiStore::open` therefore refused the folder with
"private root grants an untrusted principal". The desktop mapped every
`CredentialError::Unavailable` to the same fixed text, so the cause was
hidden. ADR-0023 also rejected requiring the exact rule on the data root
because "automatic ACL repair is not allowed".

The operator asked for two things: an error that names the cause, and a
dialog that explains the problem and offers to remove the extra
permissions.

## Decision

1. **A separate error category.** On Windows, a data folder that fails the
   private-root checks with `InvalidData` (an untrusted owner or grant, or
   not an ordinary folder) is `CredentialError::StorageNotPrivate`, not
   `Unavailable`. Its stable code is `credential_storage_not_private` in
   core, the CLI, and the desktop. The CLI keeps the native reason in its
   message. The desktop shows fixed text that names the cause and the
   accounts to allow. A missing folder or an unsupported filesystem stays
   `credential_unavailable`.
2. **Repair only after the operator confirms.** The `settings` window
   offers **Remove extra permissions…** with that error. The button sends
   `settings_repair_data_folder` with no arguments. Rust reads the folder,
   lists each allow entry for a principal other than the current user,
   SYSTEM, and Administrators, and shows the folder path and each account
   (name and SID, escaped) in a native Yes/No dialog. No webview can open,
   answer, or read that dialog. Nothing changes unless the operator answers
   Yes. After No, or `data_folder_changed`, the folder is as it was, so the
   window offers the button again without repeating the key change.
3. **Exactly what was confirmed.** The platform reopens the folder without
   following a final reparse point. It reads the grants again from that
   handle and refuses with `data_folder_changed` if the SIDs differ from
   the confirmed list. It then writes the new DACL through the same handle
   and checks the private-root rule again.
4. **Narrowing only.** The new DACL is protected, so the parent cannot pass
   the grant down again. It keeps every entry for a trusted principal and
   every deny entry, with inherited entries made explicit, and places deny
   entries first. It removes only the listed allow entries. The owner is
   never changed. The repair is refused, with `data_folder_not_repairable`,
   when the folder is a link or not an ordinary folder, is not on NTFS or
   ReFS, has an untrusted owner, or has an entry type the module does not
   decode. It is also refused when the new DACL would not give the user
   every right the store asks for on its root: listing, traversal, reading
   attributes and the descriptor, and adding files and folders. Two checks
   must both pass. First, the user's own entries on the folder, less the
   user's own deny entries, must grant those rights. This check does not
   depend on the token, so an elevated repair cannot approve a folder that
   an unelevated launch could not open. Second, Windows' own `AccessCheck`
   against the process token must grant them, so a kept deny entry for one
   of the user's groups counts. Access that comes only through a group is refused, which
   is stricter than Windows but never locks the store out.
5. **Children.** `SetSecurityInfo` passes the new inheritable entries to
   child folders whose DACL is not protected, such as `sessions`, so they
   lose the inherited grant too. The store's own children, such as
   `credentials-v1`, have protected owner-only DACLs and do not change.
6. **No journal event.** The repair changes a folder ACL, not a recorded
   setting, so the control journal is unchanged. Linux and other platforms
   report that the repair is available only on Windows; the Linux credential
   store does not use the data folder.

Automatic repair stays rejected. ADR-0023's rule for the data root does not
change: the store still refuses an untrusted grant on every open.

## Alternatives considered

- **Repair automatically on open:** rejected, as in ADR-0023. A silent ACL
  change would cut off another program without the operator's knowledge.
- **Reset inheritance (`icacls /reset`):** rejected. On the reporting host,
  `AppData\Local` passes down full control to an app-capability SID. A
  reset would replace one refused grant with another.
- **Let the webview send the list of SIDs to remove:** rejected. A
  compromised window could then name what changes. Rust lists the grants
  itself, and only the native dialog shows them.
- **Grant SYSTEM and Administrators explicitly:** rejected. The repair
  keeps the trusted entries that were already there and adds none, so it
  never widens access.
- **Only improve the message and leave the fix to `icacls`:** rejected by
  the operator, who asked for the offer in the window. The CLI keeps the
  improved message and has no repair command.

## Consequences

- An operator whose data folder another tool changed can fix it from the
  `settings` window in two clicks, and sees which accounts lose access.
- Programs that run as a removed account, such as another tool's sandbox,
  lose read access to OMGCode's data folder.
- `credential_storage_not_private` is a new stable code. Scripts that
  matched `credential_unavailable` for this case must also match the new
  code.
- The folder no longer inherits from `AppData\Local`. Later grants added
  there, by Windows or another tool, do not reach it.
- Children are not checked after the repair. `SetSecurityInfo` propagates
  the new entries to children whose DACL is not protected, and the tests
  show an inheriting child losing the grant. But the repair verifies only
  the root, and a child that kept an inherited grant, for example one that
  Windows skipped, would stay readable by the removed account while the
  root reports `Private`. The store itself needs only the root. Walking and
  checking every child, including session journals and links, is not
  decided here.
- The repair has run only in tests on Windows NTFS temporary folders. It has
  not run against the reporting host's real data folder or on a domain-joined
  computer, where account lookup can be slow or fail. An unresolved SID is
  shown without a name.
