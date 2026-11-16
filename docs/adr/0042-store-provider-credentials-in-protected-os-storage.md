# 0042: Store provider credentials in protected OS storage

- Status: Accepted
- Date: 2026-11-15

## Context

Provider profiles (ADR-0038) record a provider kind and an authentication mode
but no credential. Every real API-key or subscription connection in Phase D
needs a credential service first. The
[provider authentication contract](../provider-authentication.md#credential-and-execution-boundaries)
requires durable credentials in protected OS storage, no silent plaintext
fallback, and no credential in model context, workspace files, task
environments, command arguments, transcripts, or diagnostics. The prototype's
`OMGCODE_LM_TOKEN` process-environment input does not meet that contract.

The user selected DPAPI-protected files on Windows and the freedesktop Secret
Service through the pure-Rust zbus stack on Linux.

## Decision

1. **Native stores in `omgcode-platform`:**
   - **Windows:** each credential is a DPAPI blob (`CryptProtectData`, current
     user scope, `CRYPTPROTECT_UI_FORBIDDEN`) whose optional entropy binds it
     to the OMGCode store label and the credential identifier. It is written
     create-only as one file in a `credentials-v1` directory of the existing
     verified-ACL private store (ADR-0023). A read verifies the file's exact
     private DACL before decryption. A swapped, edited, or truncated blob
     fails decryption and is reported as corrupt, never as absent.
   - **Linux:** each credential is one Secret Service item in the default
     collection, with attributes naming the application and the credential
     identifier, through the `secret-service` crate (5.x, zbus with tokio and
     the Rust crypto session) and direct zbus calls on the same connection.
     Secrets cross the bus only in an encrypted session. A locked collection
     is not unlocked by OMGCode; it is reported as locked. OMGCode never runs
     a Secret Service prompt: the crate's blocking `create_item` and
     `Item::delete` run a returned prompt and wait for it without a bound, so
     `CreateItem` and `Item.Delete` are called directly. A returned prompt is
     dismissed without being shown and reported as a prompt refusal. An item
     is created in three steps: a direct `CreateItem` in a short plain session
     makes a placeholder with an empty value and an `omgcode_state=pending`
     attribute; the crate's `SetSecret` sends the secret through its
     encrypted session; and an attribute update removes the marker. Neither
     later step can return a prompt. A placeholder loads as corrupt, never as
     its value, and a failed later step deletes it.
   - Any other platform, a missing session bus or service, or a missing
     default collection is reported as unavailable. Nothing falls back to a
     plaintext file.
2. **Immutable credentials:** a credential is identified by a fresh random
   identifier and is never overwritten. Rotation stores a new credential,
   binds it, and then deletes the old one. An interrupted rotation either
   leaves the old or the new binding valid, with at most one unbound item to
   remove, or tells the operator which identifier to reconcile. A binding
   never names an item that OMGCode deleted, and no half-written secret is
   loaded:
   - A failed store that may have left an item deletes it. If that fails,
     the error names the identifier.
   - A binding append refused before any write deletes the new item.
   - A binding append whose journal write or sync failed may be recorded.
     The new item is kept, and the error names it and any replaced
     identifier, so the operator can check the profile and keep or remove
     it.
3. **Binding through the control journal:** trusted CLI commands bind a stored
   credential to one provider profile whose authentication mode is `api_key`.
   The control journal records the profile name and the credential
   identifier, never the secret, under the settings revision rule. Removing a
   profile or its credential removes the binding first and then the native
   item.
4. **Input:** the secret is read from standard input, bounded, and never
   accepted as a command argument or environment variable. It is not echoed,
   logged, or written to a journal or diagnostic. Output reports only the
   profile, the opaque identifier, and the backend.
5. **In-memory handling:** loaded secrets are held in an owned buffer that is
   zeroed on drop and redacted from `Debug`. This reduces, but does not
   guarantee, the removal of copies made by libraries or the OS.
6. **Scope:** this increment stores, reports, and removes credentials. No
   provider adapter reads them yet; that is the next increment. A memory-only
   mode for a single invocation is part of the adapter increment, because a
   CLI process has no session to hold it across invocations.

## Alternatives considered

- **Windows Credential Manager.** Rejected for the primary store: a generic
  credential blob is limited to about 2.5 KB, which OAuth access and refresh
  tokens can exceed, and chunking adds partial-write states.
- **The `keyring` crate for both platforms.** Rejected by the user in favour
  of narrower, directly reviewed backends.
- **`libdbus` through `dbus-secret-service`.** Rejected: it links a C library;
  zbus is pure Rust.
- **Linux kernel keyrings.** Rejected for durable storage: they do not persist
  across reboots.
- **An encrypted file with an OMGCode-managed key.** Rejected: the key would
  need the same protected storage, and it would duplicate OS facilities.
- **Plaintext fallback when no store is available.** Rejected by the contract.

## Consequences

- `omgcode-platform` gains `secret-service`, zbus, and their dependencies on
  Linux, and new `windows-sys` features on Windows. `zeroize` is added for
  owned secret buffers. zbus is also a direct Linux dependency, pinned to the
  exact version `secret-service` resolves (5.19.0) with the features it
  already enables, so the direct prompt-refusing calls add no crate. Its
  `p2p` feature is enabled only for tests, which run a mock Secret Service
  over a socket pair to prove that a returned prompt is never run.
- The three-step Linux create leaves a placeholder marked pending if it is
  interrupted. It holds no secret, loads as corrupt, blocks reuse of its
  identifier, and is removed by the normal delete.
- A Secret Service configured to confirm item creation or deletion cannot be
  used: those commands fail with a prompt refusal rather than wait for a
  user.
- The crate's blocking API, used here, runs calls on a process-wide tokio
  runtime that zbus builds on first use (and panics if it cannot build).
  Credential calls must therefore not be made from inside an async runtime.
  gnome-keyring 46 does not retain an item's content type and reports
  `text/plain`; OMGCode does not rely on it.
- Linux tests and users need a running, unlocked Secret Service. CI installs
  and unlocks `gnome-keyring` in the existing user session bus; tests fail,
  rather than skip, without one.
- Any process running as the same user can read the user's Secret Service
  items or call DPAPI for the same user. This protects credentials at rest and
  from other users, not from same-user malware. Task processes remain
  confined by ADR-0028 and ADR-0029; Linux runs cannot reach the session bus
  socket unless `HostServices` is granted, and the trusted runner must not
  expose credentials in a run's environment.
- Windows provides no power-loss durability contract for these files; the
  create-only write and the immutable model keep an interrupted store visible
  as an orphan rather than a corrupt binding.

## References

- [Provider authentication](../provider-authentication.md)
- [Security coding rules](../security-coding-rules.md)
- [ADR-0023](0023-store-windows-checkpoints-with-verified-acls.md)
- [ADR-0038](0038-record-provider-profiles-without-credentials.md)
- [CryptProtectData](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)
- [Secret Service API](https://specifications.freedesktop.org/secret-service-spec/latest/)
