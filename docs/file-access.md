# Controlled workspace file access

## Increment scope

The 0.3.0 broker provides bounded file reads, directory listings, exclusive
new-file creation, and exclusive single-directory creation. Trusted CLI input
exercises these APIs. The 0.4.0 core turn runtime can also dispatch read and
list through a caller-supplied model interface. Since 0.12.0, chat runs that
runtime with the local LM Studio adapter when the workspace's
[`model_tools` preference](workspace-permissions.md#model-readlist-and-command-tools) is
enabled; otherwise chat advertises no tools. The 0.5.0 CLI adds
a read-only `file diff` preview that reads through the broker and never writes,
checkpoints, or replaces a workspace entry. The process runner,
terminal, Git, MCP, and graphical panels are separate increments. This broker is
not process containment or a release qualification.

The CLI session UUID selects an operator trace scope. These manual commands do
not dispatch work from saved conversations. The core turn runtime checks
archive and active-turn state before starting and obtains a fresh binding before
each call; it does not infer a live binding from journal metadata. Tool-enabled
chat preserves those checks and also requires the live `model_tools`
preference before each call.

Trusted apply can replace an existing file on a Linux ext4, btrfs, XFS, tmpfs,
or ZFS mount, or on a Windows NTFS or ReFS volume, under
the [replacement contract](#replacement-contract). Trusted restore writes a
retained checkpoint's original bytes back through that same apply path, and
trusted checkpoint deletion removes one resolved private entry; see
[Phase A4](#implemented-restore-and-checkpoint-deletion-phase-a4). A create
operation still refuses every existing entry. Workspace file deletion,
restore-as-create, recursive directory creation, and action replay are
unavailable. A content check followed by rename cannot guarantee no
lost updates against independent writers. See
[ADR-0013](adr/0013-enforce-file-access-with-native-handles-and-exclusive-creation.md).

## Authorization

Each operation requires a process-local session binding and explicit request
session/workspace IDs. The broker checks store incarnation, session, workspace,
revision, expiry, current permission preferences, root membership, and private
application-data separation. Reads and listings require read permission;
creation requires write permission. A serialized ID, journal header, summary,
or diagnostic policy decision cannot restore authority.

The core then opens native root/workspace directory handles and compares their
volume, file ID, and creation time with current registrations. The turn runtime
opens the control store and obtains a fresh process-local binding immediately
before each call, then releases the settings lock before the next model
request. It does not retain a binding across model rounds or reconstruct one
from journal metadata. Relative paths
are opened through these handles. String-prefix checks derive no authority.
The diagnostic `execution_allowed` field remains false; it is not an executor
permit. The actual broker performs the additional native checks on each call.
Operational checks do not append to the settings journal. Admitted target
operations have their own intent/outcome log, so repeated file requests cannot
consume the capacity needed for settings changes. The explicit `workspace check`
diagnostic still records `policy_checked` in the settings journal.
File admission also stops if settings lack event space or a conservative
permission-change byte budget. A previously full settings log cannot leave
usable file access that the operator cannot revoke.

The control writer remains locked throughout a synchronous bounded operation.
Other OMGCode settings writers cannot change its policy midway. This does
not isolate the application from hostile code already running as the same
user, mutable filesystem implementations, or a host process moving an opened
directory outside its earlier ancestry. A confined process runner remains a
separate requirement. Do not describe handle checks as whole-user isolation.

## Native paths and objects

Accept at most 4,096 native path units, 32 components, and 255 units per component.
Registration and binding apply the same bounds to the workspace name beneath
the development root. An unusable scope is rejected before file operations.
Invalid target names return a request error; unsupported native controls remain
a separate failure. Names retain Unix bytes or Windows UTF-16 without lossy conversion. Refuse roots,
prefixes, empty components, dot/parent components, and NUL. The empty path denotes
the workspace for listing only. Windows also rejects alternate streams, device
aliases, invalid characters, and trailing-dot/space spellings.

The name `.OMGCode` is reserved for checkpoint storage at any depth and in
any ASCII letter case. It is refused as a path component with
`PermissionDenied` (`the .OMGCode folder is reserved for checkpoint
storage`), and a listing shows it as blocked. Windows also compares each
opened directory's normalized long name, which refuses a short-name alias
such as `OMGCOD~1` ([ADR-0070](adr/0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md)).

Linux uses `openat2` with `BENEATH`, `NO_SYMLINKS`, `NO_MAGICLINKS`, and `NO_XDEV`
for untrusted traversal. Mount crossings, including bind mounts, are refused.
The required syscall/flags must be available; there is no weaker fallback.
An ordinary file is first pinned with `O_PATH` and checked before opening for
data. The generated `/proc/self/fd/<owned-fd>` path reopens only that retained
regular inode. A second identity check guards this reopen. Procfs availability
is required; caller-supplied paths are never inserted into that generated path.

Windows opens one validated name relative to a retained directory handle with
native no-reparse controls and verifies the opened object's attributes, volume,
type, and link count. Directory enumeration also uses the retained handle.
Unsupported native flags or metadata fail closed. No ambient child-path reopen
or path-based directory enumeration is a fallback.

Windows directory flag compatibility is an explicit release gate. The current
opens and creates combine `FILE_DIRECTORY_FILE` with `FILE_OPEN_REPARSE_POINT`.
[Microsoft's NtCreateFile reference](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile)
omits the latter from its compatible-directory flag list. Passing native tests
shows acceptance on that host; it does not resolve this public-contract gap.
Establish a supported configuration and verify the target Windows version,
filesystem, and filter matrix before release. Do not remove no-reparse controls
or add a weaker fallback to make an incompatible system proceed.

Reads accept regular files with one hard link. Links, reparse points, devices,
and other special files cannot supply file data. Listed names and kinds are
observations only; every later access is reopened and checked. Listings can
reflect concurrent directory changes and are not consistent snapshots.

## Bounds and creation results

| Resource | Bound |
| --- | --- |
| Read or new-file payload | 1 MiB |
| One listing | 512 entries and 64 Ki native name units |
| Operation journal | 8 MiB, 4,096 events, 64 KiB per encoded event |
| Recent audit view | 1–100 entries |

Reads check size and enforce the byte limit during reading. Creation uses
exclusive create semantics, so a collision never truncates an existing file.
Parent directories must already exist. New Unix files request mode 0600 and
new directories mode 0700. Windows inherits the workspace ACL.

Tool-runtime limits are separate from the broker limits: four model rounds,
eight calls per round, sixteen calls per turn, 16 KiB per source-labelled tool
message, 256 KiB aggregate tool text, and a 30-second cooperative deadline by
default. See the [session contract](session-storage.md) and
[development guide](development.md).

A new file is visible while bytes are written. Interrupted writes can leave a
partial new entry. Post-create failure is an uncertain mutation, not a clean
refusal. The broker never automatically deletes that name, because another
actor may have changed it. Inspect the path and operation journal before any
explicit retry. File flushes and journal syncs are used, but cross-platform
power-loss durability, particularly Windows directory entries, remains
unqualified.

## Read-only edit previews

The v0.5.0 `file diff` command generates a reviewable replacement preview
without changing the workspace. It requires the same live session binding and
read preference as a broker read, reads the current regular single-link file
through the broker (creating the normal read operation record), and reads the
complete proposed content from a trusted operator-selected local regular file.
The proposed source is opened read-only without following a final link; Windows
reparse points, devices, and non-regular handles are rejected. The preview also
rejects non-UTF-8 content and either side above 1 MiB. When a diff is needed it
rejects more than 65,536 lines per side; identical content may exceed that line
count if it fits the byte bound.

The result contains the lossless target, exact current and proposed SHA-256
digests and byte counts, a changed flag, and a bounded single-hunk diff with
current and proposed line ranges. The hunk is built around the common line
prefix and suffix; it is not a minimal diff. The diff is limited to 64 KiB and carries a truncated flag. A missing
final newline is marked with the standard `\ No newline at end of file` line.
Previewing does not replace, delete, checkpoint, or otherwise mutate any
workspace entry, and it creates no proposal or checkpoint record. It is not a
write grant or a replacement contract.

The current-side size limit is enforced by the broker and reports the broker's
`file_io` error category. The proposed-side limit reports `edit_limit`.

The preview records the bytes read at that moment. An independent editor can
change the target after the read. Apply has a separate identity, content,
checkpoint, and recovery protocol. A preview grants no authority. See
[ADR-0017](adr/0017-preview-edits-before-enabling-replacement.md).

## Replacement contract

This contract was accepted through PR #10 and
[ADR-0018](adr/0018-define-checkpointed-replacement-before-enabling-overwrites.md).
Phase A2a implements read-only validation of caller-supplied record bytes.
Phase A2b adds Linux private checkpoint storage and existing-only recovery.
Phase A2c adds trusted native apply on Linux ext4. Phase A3b adds Windows NTFS
storage and apply. Phase A4 adds explicit restore and checkpoint deletion on both.
Since 0.28.0 the approval-gated model edit tool reaches the same apply; see
[Model edit route](#model-edit-route-adr-0043). Since 0.36.0 the model
multi-edit tool reaches it too, and the model create tool reaches the exclusive
create; see [Model multi-edit and create routes](#model-multi-edit-and-create-routes-adr-0054).
Since 0.38.0 the model delete tool reaches a checkpointed removal and the
model rename tool a no-replace rename; see
[Model rename and delete routes](#model-rename-and-delete-routes-adr-0055). No
model tool restores or manages checkpoints.

### Implemented record inspection (Phase A2a)

The v0.6.0 core `replacement` module provides `inspect_initialization` and
`inspect_ledger`. Both accept byte slices and expected store IDs. They return
recorded evidence only. They open no file, initialize no directory, append no
event, check no live policy, and execute no action. The A2b storage layer now
uses them when opening and appending journals. Alone, they cannot distinguish
missing storage from an unused installation because they do not observe disk.
An empty byte slice or incomplete final record is invalid input, rather than
an unused state.

Every record uses schema version 1 and has `control_store_id`,
`replacement_store_id`, `event_id`, `sequence`, `timestamp`, and `action`.
IDs must be non-nil RFC 4122 version-4 UUIDs. Sequence starts at zero and is
contiguous. Timestamps must be RFC 3339 UTC. Event IDs cannot repeat. Each action
has a `type` and a typed `payload` when needed. Unknown fields/variants and
duplicate keys at any depth are rejected. Complete LF or CRLF records are
required; encoded delimiters count toward the bounds.

Initialization input is at most 4 KiB and two records: `initializing`, followed
optionally by `ready`, with unchanged identities. The first record alone returns
an incomplete initialization snapshot. A `ready` record is a recorded claim,
not evidence that files or directory entries are durable.

Ledger input has the 64 MiB, 65,536-record, and 64 KiB-per-record bounds
below; 0.34.0 and earlier used 8 MiB and 4,096 records.
It retains at most 64 operation intents, including failed and acknowledged
intents that might have no artifact directory yet. This conservative count
does not infer freed capacity from absent files. The first record is `created`.
The remaining action sequence is:

| Action type | Payload and replay rule |
| --- | --- |
| `replacement_started` | A unique operation ID, valid session/workspace IDs, nondecreasing policy `revision`, `target`, `expected`, `proposed`, and `artifacts`, and optionally `kind`: `replace` (the default, never written) or `remove`. A `remove` proposes exactly the empty artifact. No other operation may be unresolved. |
| `checkpoint_prepared` | Matching `operation_id` and exact `manifest` descriptor. Requires the intent phase. |
| `stage_prepared` | Matching `operation_id`, native `identity`, and `content` descriptor equal to the intent's proposed bytes/digest. Requires checkpoint preparation. Invalid for a `remove`, which has no stage. |
| `commit_intended` | Matching `operation_id`. Requires stage preparation, or for a `remove` the prepared checkpoint. |
| `replacement_finished` | Matching `operation_id`, `outcome`, and bounded non-success `code`. Success requires commit intent. Clean failure/conflict is accepted only before commit intent. Uncertainty remains unresolved. |
| `replacement_acknowledged` | Matching unresolved `operation_id`. Clears only the recorded blocker; retains progress and outcome and never creates a success result. |
| `checkpoint_deletion_started`, `checkpoint_deleted` | One resolved retained entry's deletion; see [Phase A4](#implemented-restore-and-checkpoint-deletion-phase-a4). |
| `compacted` | Only as the second record. The replaced ledger's `previous` descriptor (nonempty, at most 64 MiB), `previous_records` (1 to 65,536), and the optional `revision_floor` that later starts must not go below. |
| `checkpoint_carried` | Only directly after `compacted` or another carried record. One retained entry's whole recorded state, including its `kind` (never a `remove` with a stage identity). Replay rebuilds it through the ordinary transitions above, so only a reachable state is accepted, and it must be resolved. Carried revisions are nondecreasing and at most the floor. See [ledger compaction](#implemented-ledger-compaction-phase-a5). |

Artifact descriptors have `bytes` and lowercase hexadecimal `sha256`. File
payload claims are at most 1 MiB; a manifest claim is nonempty and at most
64 KiB. Stage identity has `volume` and a 16-byte `file_id`, using the existing
native identity representation. Those fields do not prove an opened object.
The fixed artifact-name plan is `original`, `proposed`, `manifest.json`, and
`stage` beneath the operation UUID entry. Other names are rejected.

Targets use this OS's native path encoding and the existing broker's relative
path syntax and bounds. Raw Unix bytes and unpaired Windows UTF-16 units remain
lossless. Foreign-OS path encodings are refused; cross-platform code does not
mean cross-OS archive conversion. Non-success codes use 1–128 lowercase ASCII
letters, digits, or underscores. Their syntax is not a confidentiality check.

Replay never reads a referenced manifest or payload, compares actual file bytes,
verifies private ACLs, or enforces aggregate disk quota. End-of-input preserves
every unfinished or uncertain operation. After commit intent, this first format
accepts only success, uncertainty, or acknowledgement; a later backend-specific
clean-failure case needs a reviewed format extension. See
[ADR-0019](adr/0019-validate-replacement-records-before-native-storage.md).

### Implemented checkpoint storage (Phase A2b)

The v0.8.0 core `FileBroker::checkpoint_prepare` API accepts a live binding,
a relative target, the expected current digest and size, and complete proposed
UTF-8 bytes. It checks fresh read and write preferences. The native broker
captures the source bytes, file identity, owner, group, and mode through the
same pinned file. It refuses extended attributes and special mode bits for
checkpoint capture; ordinary reads keep their existing behavior. Stale expected
bytes, invalid UTF-8, oversized input, and unchanged content create no checkpoint.
The capture is not an atomic filesystem snapshot.

On Linux, private directories must belong to the
current effective user and have mode 0700. Files must be regular, single-link,
owned by that user, and mode 0600. The implementation checks opened objects,
not only requested creation modes. It retains directory handles, checks the
registered identity and creation time of the store root (a workspace's bound
`.OMGCode` folder, or the data directory for the legacy store), and uses
`openat2` without symlink or magic-link traversal. Selecting the trusted
data root can cross a mount; child traversal cannot. Generated procfs reopening requires procfs and
verifies the retained file identity again. There is no weaker path fallback.
Windows uses the private store from
[ADR-0023](adr/0023-store-windows-checkpoints-with-verified-acls.md), with exact
protected per-user DACLs, on an NTFS or ReFS volume that reports persistent
ACLs, POSIX unlink and rename, case-preserved Unicode names, and extended
attributes ([ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)). No automatic chmod or ACL repair occurs.

The source and private root must have the same device identifier before
initialization. This check does not qualify a later rename across mount aliases.
Native publication, security-metadata preservation, filesystem support, and
power-loss durability still need A2c evidence. Successful sync calls and injected
failure tests do not establish physical power-loss behavior.

First use writes the bounded initialization anchor and ledger in the order
specified below. Every checkpoint has an exclusive UUID directory with
`original`, `proposed`, and `manifest.json`. The strict manifest binds store,
operation, session, workspace, revision, lossless target, source identity,
supported security metadata, and exact payload sizes and digests. Payloads and
manifest are synced, reopened, and verified before `checkpoint_prepared` is
appended and synced. The ledger writer validates the candidate history before
append and checks that existing bytes still match. A write or sync error poisons
that writer and reports uncertainty. It does not retry or remove artifacts.

Admission reserves worst-case payload, manifest, staging, and recovery-ledger
capacity before creating operation artifacts. It counts every retained intent,
including acknowledged attempts, and all managed file lengths. A bounded
inventory rejects unknown entries, unsafe objects, missing required files,
changed manifests, and corrupt payloads. It revalidates existing payload bytes
on every ready-store admission and inspection. This can read up to the 128 MiB
store bound per request; this deliberate cost avoids trusting cached evidence.

Standalone preparation stops at `checkpoint_prepared`. It returns
`applied: false`, leaves the target unchanged, and keeps the operation unresolved.
The CLI has no standalone preparation command. Apply creates its own checkpoint
and continues in the same authorized operation. `file diff` remains a read-only
preview. Export and eviction are unavailable. Restore and deletion are explicit
commands; see [Phase A4](#implemented-restore-and-checkpoint-deletion-phase-a4).

`FileBroker::checkpoint_inspect` and `checkpoint_acknowledge` are operator
recovery APIs. Their trusted CLI routes are:

```text
omgcode --data-dir /private/omgcode-data file checkpoints --workspace WORKSPACE_UUID
omgcode --data-dir /private/omgcode-data file acknowledge-checkpoint OPERATION_UUID \
  --workspace WORKSPACE_UUID
```

`--workspace` selects that workspace's `.OMGCode` folder. Without it, these
and the other checkpoint commands use the legacy store in the data
directory, which versions before 0.49.0 wrote
([ADR-0070](adr/0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md)).

These routes require an existing control store. They never provision checkpoint
storage or require a live workspace binding. Inspection returns metadata and
recorded progress, never payload contents. Acknowledgement clears only the
matching recorded blocker. It does not report success, change permissions,
repair storage, or replay work. Settings changes remain available at admission
caps. Legacy operation-log rotation does not remove checkpoint evidence.

When both checkpoint paths are absent, ordinary file operations remain usable
without provisioning storage, including an older mode-0755 data directory.
Preparation requires the private root checks above. Once either path exists,
file admission validates the state before opening or extending the legacy
operation log. Incomplete initialization or a pending operation blocks new file
operations. Missing, foreign, corrupt, or unsupported state fails closed.
Recovery opens existing objects only. A complete record visible after a sync
error can be validated on reopen, without asserting that the original sync
succeeded. A partial record is invalid and is never automatically truncated or
repaired. The downgrade and complete-state-erasure limits below still apply.
See [ADR-0021](adr/0021-store-checkpoints-with-native-private-handles.md).

### Implemented native apply (Phase A2c)

The v0.9.0 `file apply` command accepts an existing registered workspace and
session, a lossless relative target, an exact current SHA-256 and byte count,
and a trusted local file with the complete proposed content:

```text
omgcode --data-dir /private/omgcode-data file apply WORKSPACE_UUID notes.txt --session SESSION_UUID --content-file /private/proposed.txt --expected-sha256 CURRENT_SHA256 --expected-bytes CURRENT_BYTES --writers-stopped
```

Stop other writers before running it. The required flag acknowledges this
operating assumption. It does not lock editors or make the rename conditional
on the expected bytes. An edit, swap, or removal after the final check can go
undetected. A target removed in that window can be recreated by the rename.
The original checkpoint preserves the bytes captured by OMGCode; it cannot
guarantee that they were the last bytes at the target. Do not use this route
when independent writers cannot be stopped.

The CLI captures the proposed file once using the preview's regular-file,
no-final-link input check. Both byte sequences must be valid UTF-8 and at most
1 MiB. The core checks a live binding and both permission preferences. Missing,
linked, or unsafe targets are refused. A stale digest or size conflicts before
checkpoint creation. Equal bytes return `changed: false`, `applied: false`,
and no operation ID. A source read can still create its ordinary read audit.

Native apply requires Linux with `openat2`, procfs, and a positive
`statx` mount ID (available since Linux 5.8). The retained workspace parent and
private storage must be on the same mount, and its type must be ext4, btrfs, XFS,
tmpfs, or ZFS ([ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)). Check the mount ID against a
bounded kernel mountinfo record and probe directory sync before private work.
Equal device numbers alone do not establish this. Mount aliases, other mount
types, missing controls, and unsupported platforms fail closed. The refusal
reads `workspace mount uses nfs4; file writes require ext4, btrfs, XFS, tmpfs,
or ZFS`. The inode-attribute policy comes from the open file's `statfs` magic.
ext4 may carry only `FS_EXTENT_FL`. The other filesystems may carry no
`FS_IOC_GETFLAGS` flag. `FS_IOC_FSGETXATTR` may report no settable flag, project
ID, or reserved byte, so btrfs NOCOW or compression and XFS allocation hints
or project inheritance are refused. Its read-only attribute-fork bit
(`FS_XFLAG_HASATTR`) and an extent or CoW size without its hint flag, a
filesystem default, are not attributes; extended attributes are refused
separately. Only tmpfs may lack the attribute ioctls. overlayfs, NFS, SMB, FUSE,
FAT, and exFAT stay refused. tmpfs is not persistent. Existing
read/list/create and standalone checkpoint preparation retain their earlier
requirements.

The source must be owned by the effective user. Apply preserves owner, group,
and ordinary permission bits. It refuses extended attributes, including access
ACLs, labels, and capabilities, special mode bits, and unpreserved inode flags,
project IDs, or extent hints. Kernel allocation layout, inode ID, and times can
change. An attribute query failure prevents publication. There is no fallback
that copies default permissions onto the target.

Apply retains the target and both parent handles across the bounded operation.
It syncs the original and proposed checkpoint payloads, manifest, and prepared
record before staging. The exclusive `stage` file remains under a mode-0700
operation directory even after its permissions match the target. Write, set
metadata, sync, and verify the stage before recording `stage_prepared`. Reserve
its maximum bytes and the whole remaining ledger tail before artifacts.

Compare the live parent, target identity, original bytes, and required metadata
with the retained evidence. Also verify the stage's identity, bytes, and
metadata. A detected target conflict before commit intent records a conflict
and leaves the independent change intact. The private files remain retained.
After syncing `commit_intended`, recheck the native objects again and check live
authority immediately before one handle-relative `renameat2` call. Verify the
published object and absence of the stage name. Sync the published file and
both changed parents, then sync a success outcome. The receipt contains exact
descriptors and an operation ID, with `applied: true` only after those steps.

An error after intent can leave the target unchanged or published. The writer
returns uncertainty with the operation ID and preserves the visible history.
It does not retry or restore. Inspect the actual target out of band and run
`file checkpoints` to inspect retained metadata. It never opens the workspace
from historical records. While an operation is unresolved, new broker work is
blocked. Acknowledgement clears only the matching blocker. A complete outcome
can be visible after a sync failure; that does not turn the earlier caller
result into success. Partial or corrupt records still fail closed.

Native verification covers the ordered sync calls and fault/reopen behavior on
Ubuntu 24.04 under WSL, kernel 6.6.87.2, on ext4 with `data=ordered`.
This does not qualify physical power-loss behavior or a release filesystem
matrix. See [ADR-0022](adr/0022-publish-one-verified-stage-on-linux-ext4.md).
[ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md) widened the accepted mounts beyond ext4 and makes no power-loss claim
for any of them.

### Admission and conflict checks

The first apply interface was trusted CLI input only. Since 0.28.0 the second
is the approval-gated model edit tool described under
[Model edit route](#model-edit-route-adr-0043). Its expected values come from
a live trusted broker read, never from the model or a saved conversation. The
trusted CLI interface must accept a
lossless workspace-relative target, complete proposed UTF-8 content, and the
expected current SHA-256 and byte count. Both current and proposed payloads
must be valid UTF-8 and fit the existing 1 MiB file bound. Open an
operator-selected proposed source with the same no-final-link and regular-file
checks as the preview, and capture its bounded bytes once. Hash the exact
bytes, including line endings and the final newline. Do not accept a truncated diff as proposed
content or infer expected values from a saved conversation.

Require a fresh process-local binding and both read and write preferences.
Reading and retaining the original bytes is part of the operation. Reuse the
current store/session/workspace/revision/expiry and native scope checks. Hold
the control writer for the bounded synchronous operation. A preview, checkpoint
ID, historical grant, or recovery acknowledgement cannot supply authority.

Open and retain the target's parent and current regular file through native
handles. Refuse missing targets, links, reparse points, multiple hard links,
special files, mount crossings, invalid paths, and unavailable metadata. Retain
the native file identity as well as the expected digest and size. A different
identity at the final check is a conflict even when its bytes match. The current
preview does not expose an identity token, so this does not detect a same-bytes
entry replacement made before apply opens the target. Parent traversal must
meet the existing rooted-open contract.

Capture at most 1 MiB of current bytes and calculate their digest and size.
They must match the request. Use those same captured bytes for the checkpoint;
do not copy the target again by pathname. Recheck the current entry's identity,
digest, size, and required metadata immediately before the native replacement.
A target mismatch observed before recording commit intent returns a controlled
conflict without replacing the target. Private preparation artifacts remain
recorded. A mismatch in the check after commit intent stops the native attempt
and leaves the operation unresolved under the version-1 ledger contract.
Identical current and proposed bytes need no replacement or checkpoint.

These checks are **not atomic compare-and-swap**. An independent editor can
change a file or directory entry after the final check. It can also write
during capture, so a read is not a filesystem snapshot. Repeated equal hashes
and advisory locks do not prove an exclusive edit. A write from A to B and back
to A before the final check can go undetected when the checked identity and
metadata also match. A checkpoint preserves the bytes captured by OMGCode;
it cannot promise to preserve every concurrent
edit or the exact bytes displaced by the rename. The first interface must
explain this limit and require the operator to stop other writers before
applying. That requirement is an operating assumption, not enforced isolation.
If a caller requires a no-lost-update guarantee against independent writers,
the operation must refuse that requirement. Do not label this mode conflict-free.

### Model edit route (ADR-0043)

`workspace_edit_file` replaces exactly one occurrence of a non-empty
`old_text` with `new_text` in an existing UTF-8 regular file. The chat runtime
advertises and dispatches it only while the workspace's live `model_tools`,
`read`, and `write` preferences are all enabled. The settings never authorize
the write by themselves. Since 0.43.0 (ADR-0063) the approval described below
applies when the tool's effective state is `prompt`. When `write` and the
tool's limit are both `allow`, the call skips the approval and the
writers-stopped acknowledgement and goes straight to the same live read,
digest check, and checkpointed apply. The operator who chooses `allow`
accepts that no one confirms concurrent writers are stopped.

For each call the trusted handler holds one control-store writer, obtains a
fresh binding, and reads the current file through the audited broker read,
bounded to 1 MiB. Missing, repeated (counting overlaps), or unchanged
`old_text` returns a recoverable `edit_no_match`, `edit_ambiguous`, or
`edit_unchanged` result; a non-UTF-8 file returns `non_utf8`, and a result over
1 MiB returns `edit_too_large`. The handler then computes the proposed bytes
and an approval digest under the domain tag `omgcode-file-edit-v1`. The digest
covers length-prefixed fields for the session, the workspace, the normalized
path, and the size and SHA-256 of both the current and the proposed bytes.

Without a matching record the handler requests a `file_write` approval whose
summary has the path, both sizes, and 12-character digest prefixes, never file
text, and returns `approval_required`. A pending record returns
`approval_required`, and a denied one returns `approval_denied`. An approved
record must carry the operator's `--writers-stopped` acknowledgement; core
consumption refuses one without it. The handler consumes the record and then
calls `apply_replacement` with the captured size and SHA-256 as the expected
value, so every apply gate above still applies. A file that changed after the
request has a different digest and needs a new approval. A conflict, pending,
busy, or unsupported checkpoint store, and the other admission outcomes, return
recoverable results with the checkpoint's stable code; the approval stays
consumed. Corrupt or uncertain private evidence, storage I/O failures, and a
permission denial end the turn. A success returns the normalized path, the new
size and SHA-256, and the checkpoint operation ID, never file contents.

The operator reviews the exact `old_text` and `new_text` in the recorded tool
request with `session inspect` before approving. This route does not detect a
concurrent writer any more than the CLI route does; the acknowledgement records
the operating assumption only. The desktop approvals window shows the same
`old_text` and `new_text` only when core reproduces the approval's digest from a
recorded request and a live read of the file; otherwise it offers Deny only. See
the [desktop interface guide](desktop-interface.md#approvals). Restore and
checkpoint management stay trusted CLI commands.

### Model multi-edit and create routes (ADR-0054)

Both tools are advertised and dispatched only under the same live
`model_tools`, `read`, and `write` preferences as `workspace_edit_file`, with
the same per-call recheck and `edit_disabled` denial. Both use
`file_write` approvals with the 900-second lifetime and the writers-stopped
acknowledgement, and the same lookup filters (session, workspace, operation,
exact digest, current settings revision). Each has its own digest domain tag,
so a record for one tool, including `workspace_edit_file`, can never be
consumed by another.

`workspace_multi_edit_file` takes a path and one to 16 `old_text`/`new_text`
pairs. It reads the live file through the audited broker (at most 1 MiB, UTF-8)
and matches every `old_text` against that original content: each must occur
exactly once (`edit_no_match`, `edit_ambiguous`, naming the 1-based edit index),
and the matched ranges must not overlap (`edit_overlap`). It applies the
replacements in ascending offset order into one proposed buffer, which must
differ from the file (`edit_unchanged`) and fit 1 MiB (`edit_too_large`). The
digest, under `omgcode-file-multi-edit-v1`, covers the session, workspace,
normalized path, and the size and SHA-256 of the current and proposed bytes.
After approval, the handler checks the success report fits, consumes the
record, and calls `apply_replacement` with the captured size and digest, like
the single edit. One approval and one checkpoint cover the whole call.

`workspace_create_file` takes a path and complete UTF-8 content (empty
allowed), bounded by the 4 KiB argument limit. Before recording any approval, a
live audited read must find the path absent (an existing entry returns
`file_exists`) and a one-entry listing must find its parent directory (a missing
parent returns `not_found`); nothing is recorded in either case. The digest,
under `omgcode-file-create-v1`, covers the session, workspace, normalized path,
and the size and SHA-256 of the content. After approval, the handler checks the
success report fits, consumes the record, and calls the broker's exclusive
`create_file`. The write is not checkpointed: nothing existed before, and the
exclusive create refuses an existing entry. If another writer creates the path
between the check and the create, the create returns `file_exists`, replaces
nothing, and the approval stays spent. An uncertain outcome is a terminal
failure that leaves the file-operation journal's pending record for the trusted
acknowledgement; nothing is retried or deleted. A success reports the path,
size, SHA-256, and operation ID, never the content.

Neither tool detects a concurrent writer, and neither is power-loss qualified.
The desktop approvals window reproduces the digest from a recorded request of
any of the file-write tools before it shows fields. A create request also
becomes stale once its path exists.

### Model rename and delete routes (ADR-0055)

Both tools are advertised and dispatched only under the same live
`model_tools`, `read`, and `write` preferences as `workspace_edit_file`, with
the same per-call recheck and `edit_disabled` denial, in the order read, list,
edit, multi-edit, create, rename, delete, command (eight names, the existing
cap). Both use `file_write` approvals with the 900-second lifetime and the
writers-stopped acknowledgement, and the same lookup filters. Each has its own
digest domain tag (`omgcode-file-rename-v1`, `omgcode-file-delete-v1`), so a
record for one tool, including the three earlier file-write tools, can never be
consumed by another. Both read the live source through the audited broker
first, at most 1 MiB; a larger source is the recoverable `edit_too_large`, so
the model cannot rename or delete a file over 1 MiB. The bytes are not
interpreted, so a non-UTF-8 source is accepted.

`workspace_rename_file` takes `from` and `to`. The names must differ after
normalization (`invalid_request`). Before an approval is recorded, `from` must
read (`not_found`), `to` must be absent (`file_exists`), and its parent must
list (`not_found`). The digest covers the session, workspace, both normalized
names, and the source's size and SHA-256, and the summary is `rename {from} ->
{to}: {n} bytes, sha256 {12}`. After approval, the handler checks the success
report fits, consumes the record, and calls `FileBroker::rename_file`. That
call requires both preferences from the live binding and no unresolved
checkpoint operation, records a `rename_file` intent that names the source and
the destination in the file-operation journal, and calls the platform's
`rename_no_replace`. A changed source is `checkpoint_conflict`, an existing
destination is `file_exists`, and an error after the native call may have
happened is uncertain: the audit record stays pending for the trusted
acknowledgement, and nothing is retried. Rename is not checkpointed: no content
is destroyed, and renaming back reverses it. A success reports both names, the
size, the SHA-256, and the operation ID.

The native rename, on both platforms, opens both parents handle-relative
beneath the workspace without links or mount crossings, pins the source as a
regular single-link file, reads it through the pinned handle, and requires the
size and SHA-256 to match. Directories, links, junctions, reparse points,
hard-linked files, devices, and a source on another filesystem are refused before
any rename. Linux (a source owned by the effective user) re-stats the source name
against the pinned inode immediately before `renameat2` with
`RENAME_NOREPLACE`, requires the destination name to resolve to the pinned inode
afterwards, and flushes both parents. Windows opens the source with DELETE access
and sharing that denies every other writer and deleter, so the name cannot be
swapped or removed while the handle is held, and renames with
`FileRenameInformationEx` using POSIX semantics and no replace flag, so an
existing name fails with a collision status. It reopens the destination to
compare file IDs and flushes both parents; the flush carries the ADR-0023
limitation. There is no filesystem qualification gate beyond the broker's
existing checks, as for create.

`workspace_delete_file` takes `path`. The digest covers the session, workspace,
normalized path, and the file's size and SHA-256, and the summary is `delete
{path}: {n} bytes, sha256 {12}`. After approval, the handler checks the success
report fits, consumes the record, and calls `FileBroker::apply_removal`. It
runs the checkpointed apply's sequence without a stage: the live binding, both
preferences, checkpoint admission, the accepted filesystem (Linux ext4,
btrfs, XFS, tmpfs, or ZFS; Windows NTFS or ReFS), a capture whose size and SHA-256 must match, a durable checkpoint
of the original bytes under a `remove` record, a recheck that the target is
still the captured object, commit intent, one native removal attempt, and a
success record. A change before commit intent is `checkpoint_conflict` and
records a conflict. Any later error is uncertain: the operation stays
unresolved for the trusted acknowledgement, and nothing is retried, restored,
or cleaned up. The success message names the checkpoint's operation ID.

The native removal is a sibling of the replacement commit. On Linux it
rechecks that the name still resolves to the pinned inode, then calls
`unlinkat` by name and requires the pinned inode's link count to be zero. The
call is by name, so a swap between the recheck and the call cannot be
prevented: a different file may then have been removed, and the check reports
it as uncertain. On Windows it opens the name with DELETE access, verifies that
same handle (file ID, volume, exact name, security, and every byte), and marks
that handle for POSIX-semantics deletion, so exactly the verified file is
deleted. It then requires the name to stop resolving and flushes the parent
(ADR-0023 limitation).

**Ledger.** Each operation record has a `kind` of `replace` or `remove`. It is
omitted for `replace`, so every earlier ledger, carried record, and manifest
stays byte-identical and replays unchanged. For `remove`, the `proposed`
artifact must be exactly the empty artifact (0 bytes, the SHA-256 of the empty
string) and the empty `proposed` file keeps the fixed entry layout; there is no
stage, so a `stage_prepared` record is invalid and `commit_intended` follows
`checkpoint_prepared` directly; success still requires commit intent. A carried
`remove` with a stage identity or the stage step is refused, and compaction
carries `kind`. A `remove` entry holding a `stage` file fails inspection. A
`remove` entry's original need not be UTF-8, because nothing interprets it;
a `replace` entry's must be. Older binaries refuse a ledger that holds a
`remove` record, because they reject the unknown `kind` field. Acknowledgement
and checkpoint deletion work on `remove` entries unchanged.

**Restore.** A `remove` checkpoint is restored by `FileBroker::restore_removed_file`
and the trusted `file restore --removed`, not by the apply path. The target must
not exist: the retained original is reread and compared with its record, then
written through the audited exclusive create, so an existing entry is
`checkpoint_conflict` and is never replaced. No operation is recorded, because
nothing is destroyed. Mode, owner, access list, and timestamps are not restored:
the new file gets the broker's default creation metadata. The checkpoint must be
for the same workspace and exact target. `file restore` of a replacement
checkpoint and `--removed` of a removal checkpoint are each refused as
`checkpoint_unavailable`.

Neither tool detects a concurrent writer, and neither primitive is power-loss
qualified. The desktop approvals window reproduces the digest from a recorded
request of any of the five file-write tools (at most eight distinct candidates,
newest first) before it shows fields: `From`, `To`, `Size`, and `SHA-256` for
a rename, and `File`, `Size`, and `SHA-256` for a delete. A rename review is
stale once its source changed or its destination exists, and a delete review is
stale once the file changed or is gone.

### Private checkpoints, quota, and retention

Use `replacement-v1/` in the workspace's `.OMGCode` folder
([ADR-0070](adr/0070-keep-checkpoints-in-a-protected-per-workspace-omgcode-folder.md)). Before 0.49.0 it was `<data-dir>/replacement-v1/`,
which remains readable as the legacy store. The store must be on the
target's filesystem; refuse a file on another mount or subvolume before
preparation rather than relocating storage or copying during commit.
Keep this store under the control-store identity. Every admitted replacement
gets an exclusive UUID-named entry containing the captured original bytes,
proposed bytes, and a bounded manifest. The manifest binds the control store,
operation, session, workspace, lossless target, native identity, required
security metadata, expected and proposed sizes/digests, and format version.
Names and metadata are untrusted when read back. Never overwrite or silently
reuse an existing entry.

Request mode 0700 for new Unix directories and 0600 for files; verify existing
storage is private. On Windows, require a verified per-user ACL for this new
store rather than assuming inherited ACLs are private. Require native directory
objects for the store and operation entries, and regular single-link files for
payloads, manifests, and the ledger. Refuse links/reparse points, foreign-store
objects, and unavailable controls. Native parent/entry checks must protect both
creation and reopening. Existing journal helpers alone do not establish that
protection.

Replacement storage has at most 64 operation entries and 128 MiB of managed
file bytes, including manifests, the replacement ledger, and the initialization
journal described below. The bound was 64 MiB before 0.35.0; it grew by the
64 MiB that the ledger may now use. Each manifest is at most 64 KiB. Reserve
the worst-case original, proposed, and staging payloads (1 MiB each), manifest,
and terminal ledger space before preparation. Count
incomplete entries and staging data inside this store against the limits.
Bound enumeration and refuse an unknown layout; do not recursively scan an
arbitrary tree or ignore unknown files to make capacity available. Only one
replacement can be active per store.

Sync the original and proposed files, their manifest, and the required parent
directory entries before recording the checkpoint as prepared. Reopen and
verify the exact checkpoint bytes against the manifest before use. A checkpoint
is durable only when the backend's reviewed persistence requirements succeed.
Any preparation, quota, validation, or sync failure must prevent replacement.
Partial preparation remains evidence and can require explicit acknowledgement.

Keep completed, conflicted, and uncertain entries. No checkpoint entry is
expired, evicted, exported, or pruned automatically. At the quota, refuse
further replacements while preserving inspection and permission changes. Only an
explicit trusted deletion of one resolved entry frees capacity; see
[Phase A4](#implemented-restore-and-checkpoint-deletion-phase-a4). Ledger
compaction drops the history of deleted entries only; see
[Phase A5](#implemented-ledger-compaction-phase-a5). Export needs a
later reviewed lifecycle; journal rotation must not discard referenced
checkpoints. Checkpoints contain source bytes and can
contain secrets. They are private recovery data, never ordinary model context,
tool output, or conversation events.

### Workspace checkpoint folder (ADR-0070)

The first checkpoint in a workspace creates `<workspace>/.OMGCode` under the
control lock, after the write permission check. On Windows it has the
protected current-user DACL of ADR-0023; on Linux it is mode 0700 and owned
by the effective user. An existing folder is adopted only when it is empty
and has exactly that shape: on Windows a protected DACL with one
non-inheritable full-access entry for the current user, which an inherited
DACL never has even when it names only trusted principals. Any other entry
at that name is refused with `checkpoint_store_changed`. The
control journal records `workspace_store_bound` with the folder's identity
and creation time; it does not change the settings revision. The folder
contains a `.gitignore` with `*`, so Git ignores it. The operator's
`.gitignore` is never edited.

The data directory keeps one head record per workspace store in
`checkpoint-heads-v1/<workspace-id>.json`. It records the store ID and the
committed ledger's size and SHA-256, plus the next one while a ledger
change is in progress. Initialization, each append, and compaction
announce the next head before writing and commit it after the ledger
syncs. Opening the store requires the replayed ledger to match the
committed or the announced head.

- A missing folder is `checkpoint_store_missing`.
- A replaced folder, changed access (on Windows any DACL but the exact
  protected owner-only one, even one naming only trusted principals; on
  Linux any owner or mode but the user's 0700), another store's ledger, a
  rewritten, extended, or rolled-back ledger, removed storage, or a missing
  or partial head record is `checkpoint_store_changed` or, for a partial
  head, a refusal to parse it.
- Either error blocks every broker operation in the workspace, reads
  included, as an unresolved operation does. Nothing is repaired.

Recover with the trusted CLI:

```text
omgcode --data-dir DATA file forget-checkpoint-store --workspace WORKSPACE_UUID --abandon
```

It records `workspace_store_forgotten` and prints `forgotten` and
`folder_changed: false`; the folder stays where it is and no command reads
it again. The next checkpoint needs the old folder moved, deleted, or
emptied, and then binds a new one. The desktop has no equivalent.

Admission checks both the workspace store and the legacy store, so a
pending legacy operation still blocks the workspace. Restore reads a
legacy checkpoint when the workspace store does not hold its ID; the
restore itself is checkpointed in the workspace store.

On Windows the folder's protected DACL grants only the current user and
does not inherit a workspace AppContainer grant. On Linux, Landlock grants
the workspace as a hierarchy, so a contained run with write access can
change the folder. The identity binding and head record detect such a
change before the next use; they do not prevent it.

### Replacement sequence and audit compatibility

An upgraded installation initially has neither the replacement store nor its
initialization journal. That unused state must allow existing read/list/create
operations without provisioning replacement storage. Ordinary file admission
must not create a replacement ledger just because it is absent.

The library preparation API or trusted apply may provision
storage under the control writer lock,
after authorization, private-storage, same-filesystem, and capacity checks.
Use a separate `replacement-init-v1.jsonl` anchor in the store root, outside
the replacement directory.
It is a strict, private, single-link journal of at most 4 KiB and two records,
bound to the control-store UUID and a new replacement-store UUID. Its two states
are `initializing` and `ready`, in that order. It is part of the aggregate
replacement quota and must receive the same native storage protection.

Persist and sync the exclusive `initializing` record and its parent entry
before creating the store. Exclusively create the store and its header-only ledger;
sync their header, files, and parent entries. Append and sync `ready` only after
that initialization succeeds. No replacement intent or workspace mutation is
allowed before `ready` is durable. Reopen strictly validates both records and
their store/ledger identities. Provisioning must reserve its full bounded space
before starting and must not consume settings-journal capacity.

Once the anchor exists, a missing ledger/store is not first use and must never
be recreated automatically. An incomplete initialization, a store without its
anchor, corrupt records, or inconsistent identities require existing-only
inspection and fail closed for new file operations. Settings changes remain
available. There is no automatic initialization repair or reset. A `ready`
anchor with missing storage reports missing evidence, not an empty store.
Deleting both the anchor and all store evidence cannot be detected by this
layout; it provides no protection against a same-user actor erasing all private
state. Downgrade and manual reset are not supported recovery procedures.

Use a separate versioned replacement ledger in the private store. Do not add
replacement variants to strict `file-operations-v1.jsonl` or rewrite control-v1
settings or session journals. The new ledger must strictly validate bounded
records, store identity, unique operation IDs, contiguous sequence numbers,
state transitions, and exact manifest references. Limit it to 65,536 events,
64 MiB total, and 64 KiB per event, also within the store's aggregate quota.
Reserve terminal outcome and acknowledgement capacity before admission.

Required ordering:

1. Check binding, policy, native objects, expected bytes, both operation logs,
   and capacity, including same-filesystem staging support. Persist a replacement
   intent, including the exclusive artifact names, before creating artifacts.
2. Prepare and verify the durable private checkpoint. Record its prepared state
   durably before staging or replacement.
3. Exclusively create a bounded staging file inside the private operation
   entry, through its retained directory handle. Write and sync the exact
   proposed bytes. Apply and verify required metadata before publication. Keep
   the original and proposed checkpoint files separate from this stage. Record
   the staging identity before the native commit attempt; an interrupted
   staging creation is unresolved, not permission to delete that name.
4. Recheck the expected current entry and stage, including single-link status,
   identity, bytes, and required metadata. Record commit intent durably. Repeat
   the native checks and check live authority before one reviewed native
   replacement. Never truncate the current inode.
5. Verify the published object and bytes, sync the required file/directory
   state, and persist the terminal outcome before reporting success. A later
   external edit is still possible. Success describes the completed operation,
   not permanent ownership of the target.

Preparation writes only private application storage. A detected conflict leaves
workspace entries unchanged. Publication moves the private stage to the target;
it does not consume either checkpoint payload. Sync both changed parent
directories where required. A failed or interrupted publication must preserve
enough recorded names and identities to inspect both locations.

Once replacement storage is initialized, new file-operation admission must
inspect unresolved state in both the existing operation log and the replacement
ledger. A missing, corrupt, foreign, or unsupported required artifact must fail
closed. Inspection, acknowledgement, and settings changes must remain possible
at the ordinary admission caps.
Never recreate a missing referenced checkpoint as an empty file. Older binaries
do not inspect the new ledger and cannot enforce this admission rule. Downgrade
is unsupported while replacement state exists; no old-binary recovery guarantee
is made. Replay and archive/restore must never execute a recorded replacement.

### Native backend gates

Linux ext4, btrfs, XFS, tmpfs, and ZFS and Windows NTFS and ReFS are
implemented prototypes ([ADR-0069](adr/0069-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md)). Windows is unqualified
for release, and no filesystem has a power-loss claim.
[ADR-0023](adr/0023-store-windows-checkpoints-with-verified-acls.md) accepts a
directory-handle flush for persistence, for which Microsoft gives no contract,
and records observed rename behaviour.
[ADR-0024](adr/0024-publish-one-verified-stage-on-windows-ntfs.md) records the publication decisions:

| Backend | Required primitive and review gate |
| --- | --- |
| Linux | Require one mount of an accepted type (ext4, btrfs, XFS, tmpfs, or ZFS) and retained rooted parent handles. Use one `renameat2` replacement from the private stage. Sync checkpoint files, checkpoint directories, the staged/published file, and both rename parents in the required order. The rename changes the namespace atomically; it cannot check expected content. |
| Windows | Require NTFS or ReFS with the required volume capabilities, one volume, and retained handles. Use one `FileRenameInformationEx` replacement from the private stage to the retained target parent, with replace-if-exists and POSIX semantics. Flush checkpoint files, directories, the published file, and both parents, and refuse a failed flush. The flush has no Microsoft persistence contract, and the directory-option contract gap remains open. |

The [Linux rename contract](https://man7.org/linux/man-pages/man2/rename.2.html)
defines replacement publication, and its
[fsync contract](https://man7.org/linux/man-pages/man2/fsync.2.html) requires
separate parent-directory synchronization for directory entries. Microsoft's
[rename information contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)
provides destination-directory and replacement fields; this is not evidence
that an OMGCode wrapper or a filesystem meets all requirements above.

Preserve and verify the target's security metadata before publication: Unix
owner/group, mode, and access ACL; Windows owner and access-control/protection
state. Refuse unsupported metadata, alternate data streams, or file attributes
whose preservation has not been defined and tested. Do not broaden access by
replacing a restricted file with a staged file's default permissions. Define
other preserved metadata explicitly in the backend review. A new inode/file ID
and changed modification time are expected; whole-file metadata equivalence is
not implied.

No copy/delete, in-place truncate/write, weaker traversal, or ambient-path
fallback is allowed. Do not substitute `RENAME_EXCHANGE` plus automatic rollback.
Do not use path-based `ReplaceFileW` as a shortcut: its
[documented errors](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew)
include partial name/metadata changes, and its write-through flag is unsupported.
Backend unavailability must be reported before workspace mutation. Windows
directory durability remains unqualified and is not a current guarantee.

### Implemented Windows apply (Phase A3b)

The v0.10.0 `file apply` command uses the same arguments and record flow on
Windows NTFS. ADR-0069 later added ReFS. The same limits apply: stop other writers first, and a swap or
removal after the final check can go undetected. The retained source handle
blocks new writers while the operation runs, so an in-place edit cannot happen
during it. A writer that already has the file open refuses the capture. A
reader that does not share delete access refuses apply, but not a checkpoint.

Apply preserves the owner, group, DACL with its protected and auto-inherited
state, and the content-indexing opt-out. It refuses other file attributes,
including read-only and hidden, alternate data streams, extended attributes,
mandatory labels, resource attributes, scoped policy entries, an owner other
than the current user or the token default owner, a name that differs from the
stored name by case or short-name alias, hard links, reparse points, a different
volume, filesystems other than NTFS and ReFS, and files with the ReFS
integrity-stream or no-scrub attribute. An audit SACL cannot be read without
elevation, so it is neither checked nor preserved. The file ID and modification
time can change.

Windows grants bypass traverse checking to everyone by default, so private
parent directories do not hide a file whose own DACL admits other principals.
The stage therefore keeps the private store's owner-only DACL until the commit
attempt. After commit intent and the final check, apply copies the source
security to the stage, verifies it, checks authority, and renames. A failure
before the rename attempts to restore the owner-only DACL and reports when that
restore fails. A crash, or a failed restore, in that short window can leave a
retained stage with the source DACL. Principals that DACL admits may then read
unpublished bytes. Recovery accepts either state only after commit intent and
does not repair it.

The data root must meet the
[ADR-0023](adr/0023-store-windows-checkpoints-with-verified-acls.md) rule. Any
grant beyond the current user, SYSTEM, and Administrators is refused, including
an inherited one. OMGCode never repairs it on its own; the desktop `settings`
window removes such grants only after the operator confirms each account
([ADR-0072](adr/0072-remove-extra-data-folder-grants-after-operator-confirmation.md)).
Windows makes no power-loss claim. See
[ADR-0024](adr/0024-publish-one-verified-stage-on-windows-ntfs.md).

### Uncertain outcomes and recovery

| Last durable state | Required behaviour after reopen |
| --- | --- |
| Intent only, or incomplete preparation | Report unresolved preparation; preserve any artifacts; do not stage or replace. |
| Prepared checkpoint, no commit intent | Report unresolved preparation/staging; inspect the target and recorded stage; do not infer that every artifact was fully written. |
| Commit intent, missing/uncertain outcome | Report that replacement may have occurred. Inspect target, stage, and checkpoint identities and digests. Do not infer success solely from matching proposed bytes. |
| Terminal outcome | Validate it and show its evidence; do not retry. A sync failure can leave a complete visible outcome despite an uncertain caller result. |

An error after the commit syscall starts must be uncertain unless the reviewed
backend proves no mutation for that error. Failure in post-checks, required
syncs, or outcome acknowledgement is uncertain. Poison the active writer after
a write/sync failure and require reopen for inspection. Preserve checkpoints
and staging evidence. Never retry, unlink a stage, swap back, or restore
automatically after an ambiguous result. A known pre-commit conflict does not
authorize cleanup of an entry another actor may have changed.

Recovery inspection must be existing-only and must withhold file contents from
ordinary audit output. A trusted acknowledgement records operator inspection;
it does not assert success, repair files, or grant access. Inspection and
acknowledgement stay read-only; they never restore, delete, or change an ACL.
Explicit restore is a separate command. It uses the same replacement path with a
fresh live binding, current read/write preferences, and an explicit expected
digest/size for the *current* target. Checkpoint IDs do not supply those
expected values. Restore detects later user changes, checkpoints the current
bytes again, and never overwrites them merely because an older checkpoint
exists. Missing targets and restore-as-create are outside this contract.
Workspace file deletion and recursive operations remain unsupported.

### Implemented restore and checkpoint deletion (Phase A4)

Version 0.11.0 adds two trusted CLI routes on Linux ext4 and Windows NTFS. Both
are operator commands, not model tool routes. See
[ADR-0025](adr/0025-restore-and-delete-checkpoints-explicitly.md).

```text
omgcode --data-dir DATA file restore WORKSPACE_UUID PATH --session SESSION_UUID \
  --checkpoint OPERATION_UUID --expected-sha256 CURRENT_SHA256 \
  --expected-bytes CURRENT_BYTES --writers-stopped
omgcode --data-dir DATA file delete-checkpoint OPERATION_UUID \
  --workspace WORKSPACE_UUID --permanent
```

`file restore` reads the named checkpoint's `original` payload, rereads it
through the private store, and compares it with the recorded digest and size.
The checkpoint must be retained, must have a recorded manifest, and must have
been recorded for the same workspace and the exact native target path. Its
outcome can be success, failure, conflict, or acknowledged uncertainty, because
the original bytes were captured before any of those. The store then closes,
and the bytes go through `file apply` unchanged. The expected digest and size
describe the current file. A later edit causes a conflict without mutation. A
changed restore creates a new operation whose checkpoint holds the bytes it
replaced, so a restore can itself be restored. Identical bytes change nothing
and create no operation. A missing target is refused. The output adds
`restored_from` to the apply receipt. The ledger does not link the two
operations, so a restore stays readable by 0.10.0.

Since 0.38.0, `file restore --removed` (without the current digest and size)
restores the checkpoint of a file that the model's approved deletion removed.
The target must not exist, and the retained original is written through the
broker's audited exclusive create instead of apply; an existing entry is
`checkpoint_conflict`. No operation is recorded, mode and access list are not
restored, and the output has `restored_from` and the creation receipt. See
[Model rename and delete routes](#model-rename-and-delete-routes-adr-0055).

`file delete-checkpoint` removes one resolved entry: an unresolved operation
must be acknowledged first. It needs no workspace binding and opens no
workspace file. It appends a synced `checkpoint_deletion_started` record. Then
it removes the fixed names `stage`, `proposed`, `original`, and
`manifest.json` that exist, flushes the entry, removes the empty entry
directory, and flushes the store directory. Last, it appends
`checkpoint_deleted`. The intent reserves space for that completion record.
The ledger keeps the operation's progress, outcome, and acknowledgement, and
marks its `retention` as `deleted`. A deleted entry no longer counts toward the
64-entry or 128 MiB caps.

Removal never recurses. Linux pins each name with `openat2`, checks a local
regular single-link file, calls `unlinkat`, and then requires the pinned inode
to have no links. Windows opens each name relative to its parent handle without
following a reparse point. It checks the object, then marks that handle for
POSIX-semantics deletion, ignoring a read-only attribute. A retained stage can
carry its source DACL; the private parent's delete-child right permits its
removal. A file whose DACL denies the owner attribute writes while it is
read-only is refused. Apply refuses read-only sources, so only an out-of-band
change can create one. A directory removal fails while any name remains, and an
unknown name fails inspection before removal.

An interrupted deletion stays pending as `deleting`. `file checkpoints` reports
it in `pending_deletion`. Until it finishes, file operations, apply, restore,
acknowledgement, and other deletions are refused. Repeating the same
`delete-checkpoint` command is the only accepted action. It removes whichever
fixed names remain, flushes the store directory, and records completion. This is
an explicit operator request, not an automatic retry. A partial deletion record
fails inspection closed, as other partial ledger records do.

New deletion records make a ledger unreadable to 0.10.0, which refuses it as an
unknown record. A successful apply uses five records and its deletion two.
Before 0.35.0 history was never evicted, so the 4,096-record cap bounded an
installation's lifetime operation count. Deletion is not power-loss qualified on
either platform. Windows directory flushes keep the ADR-0023 limitation.

## Implemented ledger compaction (Phase A5)

Version 0.35.0 raises the ledger bounds to 65,536 records and 64 MiB, raises the
store's aggregate bound to 128 MiB, and compacts the ledger in place. See
[ADR-0053](adr/0053-compact-the-checkpoint-ledger-in-place.md).

A compacted ledger is the original `created` record byte for byte, one
`compacted` record, and one `checkpoint_carried` record for each retained entry,
in their recorded order. The `compacted` record holds the replaced ledger's size,
SHA-256, and record count, and the revision of the last started operation. Every
record of deleted entries' history is dropped; nothing else keeps it, and the
digest alone cannot reproduce or verify it. `file checkpoints` reports the claim
as `compacted_from`. A later compaction replaces the claim with one for the
ledger it replaced, so the digests form a chain only if an operator kept each
replaced ledger.

Compaction runs in two ways:

- `file compact-checkpoints --discard-history` is a trusted operator command. It
  needs no workspace binding and opens no workspace file or checkpoint entry.
  `--workspace WORKSPACE_UUID` selects that workspace's store.
- Apply, restore, and deletion compact first when the ledger lacks room for
  their records: six for an apply or restore and two for a deletion. They do
  not compact for the aggregate byte quota.

Both refuse while an operation is unresolved or a deletion is unfinished; there
is nothing to carry for those states. Both write nothing when the ledger is
already its header, one compaction record, and one record per retained entry,
or when the rewrite would be larger.

The store builds the candidate in memory and replays it. It must hold exactly
the retained entries' recorded state before anything is written. It writes the
candidate to `replacement-v1/ledger.compact` with an exclusive private create,
syncs it, reads it back, checks that the open ledger still holds the validated
bytes, and closes that handle. Windows refuses to replace a name while a handle
that denies delete sharing is open. One native rename then replaces
`ledger.jsonl`: `renameat` on Linux, and a handle-relative POSIX-semantics
replace-if-exists rename on Windows. Both check that the name refers to the
source afterwards. The store flushes the directory, reopens the ledger, and
compares its bytes with the candidate. The anchor lock is held throughout.

A failure after the candidate is created poisons the writer and is uncertain.
Either ledger reopens with the same retained state, because the rename is
atomic and both ledgers describe the same entries. A left `ledger.compact` is a
derived copy, never evidence. Inventory counts its bytes against the quota and
otherwise ignores it; ordinary work continues. Only the next compaction removes
it, before writing a new candidate. Nothing is retried automatically.

Replay is incremental. An append validates its one record against the replayed
state instead of parsing the whole ledger again, and the result equals a full
replay. Operation IDs are indexed, so replay of a full ledger is linear in its
records. A full open still reads and parses the whole ledger, which can be
64 MiB.

Limits:

- Deleted entries' history is gone after compaction. An operator who needs it
  must copy `ledger.jsonl` before compacting.
- Operation-ID reuse is checked only against the history the ledger still holds.
  IDs are random version-4 UUIDs created by the store.
- Readers older than 0.35.0 refuse a compacted ledger as an unknown record.
- Neither the rename nor the directory flush is power-loss qualified. On
  Windows the flush keeps the ADR-0023 limitation.

## Private operation journal and recovery

`<data-dir>/file-operations-v1.jsonl` records operation intents and outcomes.
It links its header to the existing control store UUID. The settings journal
format stays unchanged, so 0.2.0 settings readers need no migration. Older
binaries provide no file broker and cannot execute operation history.

The file-operation journal uses exclusive writer ownership, strict bounded
JSONL replay, unique IDs, contiguous sequences, UTC timestamps, and synced
acknowledgement. Invalid, empty, linked, incomplete, or foreign-store files
are never reset automatically. File contents are not logged. Relative path names
retain their native form and may themselves contain credentials or other
sensitive data. Protect the journal and audit output as private user data; path
names are not redacted.

An intent is durable before mutation. A read result is released only after its
outcome is durable. Every start reserves two terminal event slots and 4 KiB
for finish and acknowledgement. An audit write/sync failure poisons its writer.
If creation succeeded but its result could not be journaled, the caller receives
an uncertain result with its operation ID.

A complete outcome may still be visible after a sync error. Reopen validates
that record without retrying the operation. Inspect the reported journal state;
an uncertainty returned to the caller does not guarantee an unfinished record.

Replay exposes an unfinished or uncertain operation and starts no work. New
file operations stop until the user explicitly acknowledges that operation
through the trusted operator interface. Acknowledgement records inspection; it
does not assert success, repair a file, retry a write, or grant permissions.
The user must inspect and resolve any partial entry separately. Automatic
repair and a graphical recovery flow are future work.

## Explicit operation-journal archive

When the active log reaches its byte or event cap, use the trusted
`file rotate --archive-dir PATH` command. The directory must already exist and
be private, outside both application storage and the current development root.
It must be on the same filesystem as the active log. This is an explicit export
to operator-managed storage, not a model tool or an automatic retention policy.

The command strictly replays the existing active log and refuses unfinished,
uncertain, invalid, or missing logs. It flushes the file, closes its writer, and
makes one native atomic no-replace move to `file-operations-<UUID>.jsonl`.
It then syncs destination and source directories where supported. The receipt
contains the lossless archive path, exact raw-byte SHA-256, and event count.
No archive is overwritten, deleted, scanned automatically, or replayed as work.

Each export is at most 8 MiB and 4,096 events. Active application storage keeps
its existing bound. Repeated explicit exports consume operator-selected disk
space; OMGCode does not impose an aggregate quota or manage their retention.
Keep exports private and outside present and future development roots. Move
them elsewhere through the operator's normal file-management process when
needed. Do not assume an export remains private after broadening a development
root to contain it.

The next ordinary operation initializes a fresh active log, preserving the
control store and its current permissions. `file audit --archive PATH` validates
an existing archive for that same store and returns bounded metadata without
appending. Missing archives are not created. Inspection currently requires the
existing read/write exclusive-lock helper, so read-only media are not supported.

A move or directory-sync error can return `file_rotation_uncertain` with the
lossless destination candidate. Inspect both the active path and candidate
before any explicit retry. The command does not reset, retry, or repair either
file. Windows directory power-loss durability remains unqualified.
See [ADR-0015](adr/0015-archive-operation-logs-without-consuming-settings-capacity.md).

See [workspace settings](workspace-permissions.md) for binding/configuration
contracts and [security rules](security-coding-rules.md) for the full boundary.
