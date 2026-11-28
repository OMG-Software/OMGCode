# Workspace identity and permission settings

## Implemented boundary

The 0.2.0 CLI stores a user-selected development root, stable workspace IDs,
permission preferences, a global settings revision, and audit events. This is
configuration infrastructure. It does not implement a file broker, runner,
permission settings page, Git tools, or process isolation.

Every diagnostic result has `execution_allowed: false`. A result with
`configured_allowed: true` means the current preferences allow that operation
class for that binding. It is not a grant to open a file or start a process.
Future tools must check the target actually opened and apply native controls.
Never pass these results to an unrestricted executor.

Since 0.16.0 the live `execute` preference gates the trusted CLI command
`omgcode run`. The command checks the preference itself on every run and
starts the program only under native confinement. It does not consume a
diagnostic result; see [trusted runs](execution.md#trusted-runs).

## Trusted settings and identity

Only the trusted local CLI and, since 0.41.0, the desktop `settings` window
can change these settings (ADR-0060). Neither is a model tool. The window
also adds and removes toolchain profiles, package-network profiles, and
container grants (ADR-0088), and manages skills (ADR-0082). The window gets
every folder from a native dialog and asks for a native confirmation before
any change that widens access. Prompts, conversation history, imported IDs, skills, and repository files
cannot set a development root, change permissions, or recreate a binding.

Select an existing development root explicitly. Register an existing workspace
beneath it, including the root itself. Each workspace gets a random UUIDv4.
Since 0.43.0 each preference except `model_tools` is `deny`, `prompt`, or
`allow`; see [deny, prompt, or allow](#deny-prompt-or-allow-adr-0063). A new
workspace starts with `read` allow and `write` prompt; process execution, task
network access, and host services are `deny`, and model tools are off. Provider
connections remain separate from task networking. No operation class executes
a tool by itself. The separate `model_tools` and `host_services` preferences
are described under [model tools](#model-readlist-and-command-tools) and
[host services](#host-services).

Registration stores the canonical native path, volume/filesystem ID, 128-bit
file ID, and directory creation timestamp. Canonicalization resolves a trusted
configuration selection; it does not authorize later I/O. Native path units are
preserved without Unicode replacement. Ancestor checks compare live directory
identities, with a maximum of 256 ancestors. Exhausting the bound is an error,
never proof that two directories are separate. They do not use string prefixes.
Directory creation-time support is required; unsupported filesystems fail.
After resolving a trusted selection, Unix opens the canonical path with
`O_DIRECTORY | O_NOFOLLOW`. A FIFO or a newly introduced final-component link
then fails at open. This does not protect parent-directory races.

Registering the same directory again reuses its ID without a revision change.
Aliases of that directory can reuse the same ID. A replacement at a registered
path cannot inherit the old ID or preferences. If a saved workspace path is
missing or now names a different directory, binding or registration fails with
`DirectoryChanged`. Renaming or moving a workspace does not migrate its
registration. While its saved path is missing or stale, registration at a new
path cannot reuse the old ID or preferences. Bindings recheck workspace identity and root
membership. The application data directory and development root must not
overlap in either direction; the saved application-data directory identity is
checked before the overlap check. Stale data-directory identity fails closed.

These metadata checks detect ordinary replacement and configuration mistakes.
They do not prevent path-swap races, hostile mount changes, file ID reuse with
forged metadata, or a malicious process running as the same user. File access
goes through the bounded native broker described in the
[file access contract](file-access.md); its limits are stated there.

## Revisions and bindings

Every mutation requires the expected global revision. A stale value fails
without changing state. Root changes, new registrations, and changed flags
increment the revision. Idempotent settings operations do not append an event.
Changing the root preserves saved registrations, but unavailable or out-of-root
workspaces cannot obtain a new binding. There is no automatic reassignment or
path migration. A renamed workspace with a missing saved path cannot be
re-registered at its new path by reusing the old record; an explicit future
migration flow would need to define that operation.

A process-local session binding identifies the control store, session,
workspace, and revision. It expires after 15 minutes on a monotonic clock. It
cannot be cloned or deserialized. Every configuration change makes all earlier
bindings stale. The core turn runtime opens the control store, obtains a fresh
binding, and releases the settings lock for every tool operation instead of
retaining one across model rounds; it never reconstructs a binding from journal
metadata. The read-only `file diff` preview also requires the read preference.
It is not a write grant. Trusted apply separately checks live read
and write preferences and the native replacement requirements. Evaluation
rejects store, session, workspace, revision, expiry,
availability, and flag mismatches before reporting configuration eligibility.
Checks append audit events without changing the settings revision.

Trusted CLI runs exist since 0.16.0. A run needs the live `execute`, `read`,
and `write` preferences, `host_services` when it selects host services, and,
since 0.18.0, `task_network` when it selects network access with
`--network`.
It checks them before it starts and again after every settings change while
it runs. A change that withdraws one stops the running tree, which is recorded
as `revoked`. Since 0.41.0 the run's watcher checks every recorded settings
state since it last read them, so turning the preference back on before it
reads does not keep the run going (ADR-0061). The `permissions` command reports success only after the runs it
affects have stopped; see [settings changes during a
run](execution.md#settings-changes-during-a-run). Per-target grants remain
required. Single-use approvals now exist for the model read, list, edit,
multi-edit, create, rename, delete, directory, command, and `web_fetch` tools:
the trusted CLI and the desktop approvals window decide them. The desktop
`settings` window changes the development root, registrations, the workspace
preferences, and package cache roots through the same store calls, and creates
an empty store on first use (ADR-0059 phase S2, ADR-0060).

## Persistence and audit

The source of truth is `<data-dir>/control-v1.jsonl`, outside the development
root. It combines settings changes and audit records in one append-only log.
This avoids a separate settings write succeeding without its audit event.
The process holds an exclusive writer lock only for the local operation and
releases it before a model request. It never holds a session lock while opening
the control store. Each acknowledged event is synced before state is published.

Every envelope has schema version 1, store/event UUIDv4 values, a contiguous
zero-based sequence, UTC timestamp, global revision, and tagged `action`:

| Action | Meaning |
| --- | --- |
| `created` | Initial store identity, sequence and revision zero |
| `root_set` | Selected directory identity; increment revision |
| `workspace_registered` | New ID, directory identity, default preferences; increment revision |
| `workspace_unregistered` | Remove one registration and the settings that name it, including its profile assignments; nothing on disk changes (ADR-0081); increment revision |
| `permissions_changed` | Existing ID and complete preference set; increment revision |
| `web_fetch_host_added` | Add one host pattern to a workspace's `web_fetch` allowlist (ADR-0098); increment revision |
| `web_fetch_host_removed` | Remove one host pattern from that allowlist; increment revision |
| `policy_checked` | Explicit diagnostic binding/request check and decision; preserve revision |
| `container_grant_requested` | Windows container grant intent; increment revision |
| `container_grant_applied` | Grant applied to the access list; preserve revision |
| `container_revoke_requested` | Revocation intent for a grant; preserve revision |
| `container_grant_revoked` | Grant removed from the access list; preserve revision |
| `profile_added` | New toolchain profile; increment revision |
| `profile_removed` | Remove a toolchain profile and every assignment of it; increment revision |
| `profile_assigned` | Assign a recorded toolchain profile to one workspace (ADR-0107); increment revision |
| `profile_unassigned` | End that assignment; increment revision |
| `package_profile_added` | New package-network profile; increment revision |
| `package_profile_removed` | Remove a package-network profile and any credential bindings it ends; increment revision |
| `package_credential_bound` | Bind a stored credential to a package-network profile; increment revision |
| `package_credential_removed` | End that binding; increment revision |
| `package_fetch_started` | Trusted package-fetch intent; preserve revision |
| `package_fetch_finished` | Trusted package-fetch outcome; preserve revision |
| `package_cache_root_set` | Select one workspace's package cache root; increment revision |
| `package_cache_root_cleared` | Clear one workspace's package cache root; increment revision |
| `workspace_store_bound` | Create or adopt a workspace's `.OMGCode` checkpoint folder (ADR-0070); preserve revision |
| `workspace_store_forgotten` | Abandon that folder, which is left in place; preserve revision |
| `provider_profile_added` | New provider profile; increment revision |
| `provider_profile_removed` | Remove a provider profile and any credential binding it ends, and clear the active connection if it was that profile; increment revision |
| `active_provider_selected` | Choose a local provider profile, or none for the default, as the desktop's active connection; increment revision |
| `provider_credential_bound` | Bind a stored API key to a provider profile; increment revision |
| `provider_credential_removed` | End a provider credential binding, including a ChatGPT sign-out; increment revision |
| `provider_subscription_bound` | Record a completed ChatGPT sign-in (ADR-0093); increment revision |
| `skill_added` | Import a skill into the private registry, disabled (ADR-0057); increment revision |
| `skill_removed` | Remove a skill; increment revision |
| `skill_enablement_set` | Enable or disable a skill for the application or one workspace; increment revision |
| `approval_requested` | Trusted approval request; preserve revision |
| `approval_decided` | Approval granted or denied; preserve revision |
| `approval_consumed` | Approval used once; preserve revision |

Operational file checks do not append `policy_checked`. They use current
settings and their separate [operation journal](file-access.md). This prevents
file traffic or a full operation log from consuming settings capacity.
Workspace registration and binding reject relative scopes beyond the broker's
name/depth bounds, with a workspace-specific error.

Limits are 8 MiB per journal, 4,096 events, 64 KiB per encoded event including
the newline, and 256 registered workspaces. Audit output returns 1 to 100 recent
events. Native paths have at most 32,768 units. Audit rows contain identifiers
and policy metadata, never prompts or dedicated credential fields. Native paths
can themselves contain secrets. Treat journal records and audit output as
private data.

Opening validates the complete bounded log. Empty files, incomplete tails,
duplicate JSON keys or event IDs, unknown fields/schema/events, invalid
transitions, and sequence/revision inconsistencies fail without repair. Only
an absent file permits initialization. Existing links, including dangling links,
are invalid state; they cannot select the legacy no-settings path. Write or sync failure poisons that
writer; no decision is returned if its audit append fails. A complete record
can still exist after an uncertain sync, so reopen validates without retrying
work. There is no rotation, automatic repair, deletion, or directory-rebind
command yet. Capacity exhaustion stops changes and checks; it never drops audit
events. Keep the original log for future reviewed migration/recovery.

New Unix directories request mode 0700 and files 0600. Existing permissions are
not changed. Windows inherits ACLs; explicit per-user ACL setup and power-loss
durability remain unqualified. Parent paths must be trusted. Native file checks
and locks do not protect against a hostile same-user application.

## Container grants

On Windows, `workspace container-grant` and `workspace container-revoke` give
and remove ACL grants for a workspace's AppContainer. A grant needs the expected
revision and increments it; revocation needs none and keeps it. `workspace
status` lists outstanding grants under `container_grants` and omits the field
when there are none, so settings without grants keep their exact output.
Version 0.14.0 refuses a settings log that records a grant. The
[execution guide](execution.md#container-grants) owns the rules. Since
[ADR-0088](adr/0088-change-execution-settings-in-the-settings-window.md) the
desktop `settings` window grants a folder and revokes a grant through the same
core calls, with native dialogs and confirmations; on other platforms it
returns `unsupported_platform`. It adds no new event. Since
[ADR-0104](adr/0104-grant-the-workspace-folder-read-write-without-a-folder-dialog.md)
a read-write grant there opens no folder dialog and uses the workspace's
recorded directory. Since
[ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)
the operator changes that need a grant make it: registering a workspace grants
its container read-write on its folder, and assigning a toolchain profile grants
read-execute on each of the profile's folders the container cannot read yet.
Unassigning or removing an assigned profile, and unregistering a workspace,
revoke the grants no longer needed. They use the same events.

## Unregistering a workspace

`workspace unregister <workspace-id> --revision <n>` removes one registration
([ADR-0081](adr/0081-unregister-a-workspace.md)). It needs the expected revision
and increments it, through one `workspace_unregistered` event that replay
applies. The event also drops what names the workspace: its preferences, its
package cache root selection, its skill workspace overrides, and its bound
`.OMGCode` checkpoint folder record (as `file forget-checkpoint-store` does). Nothing
on disk changes: the folder, its `.OMGCode` folder, cached packages, and the
sessions recorded in the workspace stay. Approval records, package-fetch
records, and policy-audit records stay as history. The revision change makes
every outstanding approval unusable, and a started run of the workspace is
withdrawn like any run whose workspace is no longer registered.

Since ADR-0107 the command first revokes every container grant of the
workspace, in any state, and lists them under `revoked`. It records
`workspace_unregistered` only when every revocation was confirmed. Every
grant is attempted, even after one fails. A failed revocation leaves the
workspace registered with the grants not yet revoked; the command names each
one, and `workspace container-revoke` retries it before the operator
unregisters again. When every grant was revoked but the event cannot be
recorded, the workspace also stays registered, and the command still waits
for the runs the revocations withdrew. The event itself is still refused while any
grant names the workspace (`grant refused`), so replay keeps ADR-0081's rule.
An unknown workspace and a stale revision are refused as for any other
settings change, before anything is revoked. Registering the same folder afterwards assigns a new
identifier and the default preferences; the old identifier is never reused.

The desktop `settings` window removes the workspace's saved custom
instructions file with the same event
([ADR-0080](adr/0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)),
and the CLI deletes it right after the event is recorded. Sessions recorded in
the removed workspace keep their journals and index entries. A turn in one
finds no registered workspace, so chat is text only with the reason
`workspace_unregistered`, as described under
[model tools](#model-readlist-and-command-tools), and sends no workspace
instructions. A control journal that records `workspace_unregistered` cannot be
read by 0.55.0 or earlier.

## Toolchain profiles

`workspace profile-add`, `profile-show`, and `profile-remove` manage the
operator-declared toolchain profiles that `omgcode run` uses. Adding and
removing need the expected revision and increment it, through the
`profile_added` and `profile_removed` events. `workspace status` lists
profiles under `profiles` and omits the field until a profile exists. Each
permission view now also includes `host_services`, so the output is not the
same as version 0.15.0's. Version 0.15.0 refuses a settings log
that records a profile. The
[execution guide](execution.md#toolchain-profiles) owns the rules. Profile
variables are stored here in plain text; never put secrets in them.
The desktop `settings` window adds and removes profiles through the same core
calls, with a native folder dialog for each root and `PATH` entry and a native
confirmation (ADR-0088); it records the same events and offers no edit.

A workspace runs only the profiles assigned to it
([ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)).
`workspace profile-assign <workspace> <name> --revision <n>` and
`profile-unassign` record `profile_assigned` and `profile_unassigned`, each
of which increments the revision. Replay refuses an unknown workspace or
profile, an assignment that exists already, and an unassignment of one that
does not; assigning also refuses a profile declared on another platform.
`workspace status` lists each workspace's assigned profile identifiers under
`assigned_profiles`, in assignment order, and omits the field when there are
none. Removing a profile ends every assignment of it, and so does
unregistering the workspace. On Windows assigning grants the folders the
workspace's container needs, and unassigning revokes those no other assigned
profile needs; the [execution guide](execution.md#toolchain-profiles) owns
those rules. The `settings` window assigns and unassigns with a switch per
profile on the workspace's page, after a native confirmation. Existing
profiles start assigned to no workspace. Version 0.69.0 refuses a settings
log that records an assignment.

## Package-network profiles

`workspace package-profile-add`, `package-profile-show`, and
`package-profile-remove` record operator-declared registry origins as
configuration only. Adding and removing need the expected revision and
increment it, through the `package_profile_added` and
`package_profile_removed` events. `workspace status` lists profiles under
`package_profiles` and omits the field until a profile exists. Each profile
has a name and 1 to 8 origins; there are at most 32 profiles, and one origin
text is at most 2,048 bytes. No run accepts a profile, and a profile never
authorizes network access. Version 0.19.0 refuses a settings log that records
one; a 0.20.0 control journal cannot be read by 0.19.0. The
[execution guide](execution.md#package-network-profile-contract-only) owns the
contract and the remaining enforcement gate. The desktop `settings` window adds
and removes profiles through the same core calls after a native confirmation
(ADR-0088); removal also unbinds the credentials bound to the profile's
origins. Binding a credential stays a trusted CLI action.

A trusted `packages fetch` appends `package_fetch_started` and
`package_fetch_finished` events. They preserve the settings revision. They
record the profile identity and revision, a SHA-256 of the request URL (never
the URL text), the cache name, limits, HTTP status, byte count, and content
digest. A failed fetch records a controlled reason; an abandoned fetch leaves
its start event. A control journal written by 0.21.0 cannot be read by 0.20.0.

A registry credential bound to one origin of a profile
([ADR-0056](adr/0056-send-registry-credentials-to-one-declared-origin.md))
appends `package_credential_bound` and `package_credential_removed` events,
each of which advances the settings revision. `package_profile_removed` then
lists the identifiers it unbinds, and `package_fetch_started` records the
`credential_id` of a credential it attached. A journal that holds these events
cannot be read by an earlier version. Journals record only opaque identifiers.

## Package cache roots

`workspace set-package-cache <workspace-id> <dir> --revision <n>` selects one
workspace's fetched-package cache directory. It must exist, must not overlap
application storage or the development root, and must not overlap another
workspace's cache root. `workspace clear-package-cache <workspace-id>
--revision <n>` removes the selection and leaves cached files in place. Both
use the expected revision and increment it through `package_cache_root_set`
and `package_cache_root_cleared`. `workspace status` lists selected roots under
`package_cache_roots` and omits the field until one exists. A control journal
written by 0.23.0 cannot be read by 0.22.0. The
[execution guide](execution.md#trusted-package-fetch) owns the fetch, exposure,
and grant rules.

## Provider profiles

`workspace provider-profile-add <name> --kind <kind> --auth <auth>
--revision <n>`, `provider-profile-show <name>`, and
`provider-profile-remove <name> --revision <n>` record provider connection
shapes. They store no credential or account. Adding and removing
use the expected revision and increment it through `provider_profile_added`
and `provider_profile_removed`; `workspace status` lists them under
`provider_profiles` and omits the field until one exists. The
[provider authentication guide](provider-authentication.md) owns the kind and
authentication matrix. A control journal written by 0.24.0 cannot be read by
0.23.0.

`provider-profile-add` accepts `--endpoint <url>`. An `lm_studio` profile
(since 0.31.0) and an `ollama_local` profile (since 0.44.0) take a loopback
address. Each named hosted kind accepts only its provider's one documented
endpoint: `ollama_cloud` since 0.50.0 (ADR-0071) and the others since 0.59.0
(ADR-0092). An `api_key` profile of a named hosted kind that omits
`--endpoint` records that endpoint. An `openai_compatible` profile needs an
`https://` base URL that core validates and refuses rather than rewrites. The
endpoint is validated when recorded and again on replay, and it cannot be
changed without removing the profile. `provider_profile_added` carries an `endpoint` field
only when one is recorded, so a journal without one is unchanged; a control
journal that records an endpoint cannot be read by 0.30.0. The
[provider authentication guide](provider-authentication.md#endpoint-binding-0310)
owns the rules.

A `claude_code` profile (0.72.0,
[ADR-0109](adr/0109-drive-the-signed-in-claude-code-cli-as-a-model-connection.md))
is recorded with `workspace claude-code-add` or, since ADR-0111, the desktop
`settings` window instead, which `provider-profile-add` refuses. Its `provider_profile_added` event carries a `claude_code` field with
the program and the identities of its folder and of the sign-in folder, and no
endpoint or credential. The connection's container grants are recorded as
`claude_code_grant_requested`, `claude_code_grant_applied`,
`claude_code_revoke_requested`, and `claude_code_grant_revoked`, with the same
grant reserve as workspace grants, and replay refuses a profile removal while
any of its grants remains. A control journal that records one cannot be read
by 0.71.0. The profile grants no workspace permission.

Since 0.44.0, `provider-profile-select <name> --revision <n>` records
`active_provider_selected` with the profile's identifier, and `--default`
records it without one. Only an `lm_studio` or `ollama_local` profile with an
endpoint can be selected, and an unchanged selection is refused. Replay
checks each selection again. `workspace status` shows `active_provider` only
while one is selected, so a journal without a selection is unchanged; a
control journal that records one cannot be read by 0.43.0. The selection is
read only by a desktop shell started without `--endpoint` or
`--provider-profile`
([ADR-0064](adr/0064-choose-the-model-connection-in-settings-and-add-ollama-local.md)).

Since 0.27.0, `provider-credential-set <name> --revision <n>` and
`provider-credential-remove <name> --revision <n>` bind and unbind a credential
stored in protected OS storage for an `api_key` profile. They record
`provider_credential_bound` and `provider_credential_removed` with the profile
and an opaque credential identifier, never the secret, and increment the
revision. A removed profile's `provider_profile_removed` event names any
credential it unbinds. Each listed profile reports `credential` as
`{"state":"none"}` or `{"state":"bound","id":...,"backend":...}`. A control
journal written by 0.27.0 cannot be read by 0.26.0. See the
[development commands](development.md#provider-credentials).

## Approval records

`approvals request`, `list`, `show`, `approve`, `deny`, and `consume` manage
trusted per-call approvals. A request records a session, workspace, operation,
exact request digest, summary, settings revision, and expiry. The events
`approval_requested`, `approval_decided`, and `approval_consumed` preserve the
settings revision. A decision or consumption is refused after expiry,
mismatch, reuse, or any settings change. The `workspace_run_command` model tool
consumes an approval; the CLI consume command marks a record used but executes
nothing. Since 0.43.0 a `prompt` model read records a `file_read` approval,
which needs no writers-stopped acknowledgement, and the desktop shell waits
in the turn for each decision (ADR-0063). The
[agent instructions guide](agent-instructions.md#approval-records) owns the
tool-side contract. See
[ADR-0039](adr/0039-gate-future-model-tools-with-bound-single-use-approval-records.md)
and [ADR-0040](adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md).

## Task network

`workspace permissions <id> --revision <n> --task-network true` lets a
trusted `run` of that workspace select outbound IP networking when the run
also passes `--network`. Both are required; the preference alone changes
nothing. It is off by default. Turning it off stops networked runs of the
workspace. Model connections never use it. A networked Linux run can reach
every destination, including loopback and the local network; see
[network access](execution.md#network-access).

## Host services

`workspace permissions <id> --revision <n> --host-services true` lets a
trusted Linux `run` of that workspace select host-service Unix-socket access
when the run also passes `--host-services`. Both are required. The preference
is off by default and appends `permissions_changed` like the other flags. It is
not an operation class, so `workspace check` does not mention it. Windows
refuses host services at run time whatever the preference says.

`workspace status` always reports `host_services`. Persisted settings omit the
field while it is `false`, so settings that never enable it keep their exact
bytes. Once a `true` value is written, version 0.15.0 refuses the settings
log. Host-service access exposes every Unix socket the user can reach,
including the Docker daemon; see
[Unix-domain sockets](execution.md#unix-domain-sockets).

## Web fetch (ADR-0098)

`workspace permissions <id> --revision <n> --web-fetch deny|prompt|allow`
sets whether chat offers the model `web_fetch` tool, which sends one HTTPS
`GET` to a public host from the OMGCode process and returns a slice of the
page text. It is separate from `task_network`, which no model tool uses. It
defaults to `deny`, needs `model_tools`, and does not depend on `read`,
`write`, or `execute`. Stored settings omit it while it is `deny`, so they
keep their bytes; a build before 0.64.0 refuses a settings log that holds
another state.

Each workspace also has a host allowlist of at most 100 patterns (32 before
0.73.0, ADR-0110; an older build refuses a settings log with more).
`workspace web-host-add <id> <pattern> --revision <n>` adds one,
`workspace web-host-remove <id> <pattern> --revision <n>` removes one, and
`workspace web-host-list <id>` prints the preference and the list. A pattern
is an ASCII DNS name, which matches that host only, or `*.` followed by a
name, which matches its proper subdomains and not the name itself. IP
addresses, local names, and non-ASCII names are refused; use punycode. Each
change appends an event and increments the revision, so any pending fetch
approval becomes stale. The desktop `settings` window edits both after a
native confirmation.

| State | Allowlisted host | Any other public host |
| --- | --- | --- |
| `deny` | Tool not offered; a call is refused | Tool not offered; a call is refused |
| `prompt` | Each fetch needs a `network` approval | Each fetch needs a `network` approval |
| `allow` | Runs without an approval | Each fetch needs a `network` approval |

A URL that the network policy refuses, such as `http`, an IP literal, or
`localhost`, is answered with `url_refused` and recorded as refused; it
records no approval and is never fetched. An approval binds the exact
normalised URL, offset, and size, needs no writers-stopped acknowledgement,
and shows the URL, its host, and whether the host is allowlisted now. The
[execution guide](execution.md#model-web-fetch) describes the network
policy, and [ADR-0098](adr/0098-add-a-model-web-fetch-tool-behind-a-host-allowlist.md)
records the design.

Since 0.73.0 the desktop approvals window can also decide a pending fetch with
**Allow host** (ADR-0110). It is offered only while `web_fetch` is `allow`, the
host is not listed, and the list has room. After a native confirmation that
names the host and the workspace folder, core approves the record and then
adds the URL's exact host, never a wildcard, in one hold of the settings
store. The host comes from core's review of the record, not from the window.
Adding it advances the revision, so the approved record is never consumed:
the waiting fetch is dispatched again and runs under the standing allow, and
other pending approvals become stale. The CLI has no combined decision.
`approvals approve` and `workspace web-host-add` are separate steps with
separate results. A waiting desktop turn can consume the plain approval
before the host is added, so that fetch runs once under the approval, and
only later fetches run under the standing allow. Adding the host first makes
the pending approval stale instead.
See [ADR-0110](adr/0110-allow-a-web-host-from-a-fetch-approval-and-list-100-hosts.md).

## Deny, prompt, or allow (ADR-0063)

Since 0.43.0, `read`, `write`, `execute`, `task_network`, and `host_services`
each hold one of three states. `model_tools` stays on or off.

| State | Trusted operator command | Model tool call |
| --- | --- | --- |
| `deny` | Refused | Tool not offered; a call is refused |
| `prompt` | Permitted | Each call needs a trusted single-use approval |
| `allow` | Permitted | Runs without an approval |

`workspace permissions <id> --revision <n>` takes `--read`, `--write`,
`--execute`, `--task-network`, and `--host-services` with `deny`, `prompt`, or
`allow`. The earlier `true` and `false` are still accepted: `false` is `deny`,
and `true` is `allow` for `--read` and `prompt` for the others, which is what
`true` meant before. The desktop `settings` window shows a selector for each
preference and confirms any change towards `allow` (ADR-0060).

**Per-tool write limits.** `--edit-tool`, `--multi-edit-tool`, `--create-tool`,
`--rename-tool`, `--delete-tool`, and `--create-directory-tool` set a limit for
one model file-write tool. Each defaults to `allow`. A limit can only narrow `write`: the tool's state is
the more restrictive of the two, and a `deny` limit removes the tool. For
example, `--write allow --delete-tool prompt` lets the model edit, create, and
rename files freely but still asks before each delete. `workspace status`
reports the limits as `write_tools`. In the desktop `settings` window,
**Set all** beside **File-write tool limits** sets all six limits to one state
in one change (ADR-0090); it does not change `write`.

**What a model tool call uses.** With `model_tools` off, every model tool is
denied. Otherwise:

- `workspace_read_file` and `workspace_list_directory` use `read`. Under
  `prompt` each call records a `file_read` approval for the exact path and
  limit, and nothing is read until it is approved. A read approval needs no
  writers-stopped acknowledgement.
- The six file-write tools use `write` and their own limit. They are denied
  unless `read` permits, because each reads the file first. A `prompt` read
  adds no prompt here: that read returns no file text to the model.
- `workspace_run_command` uses the most restrictive of `execute`, `read`, and
  `write`. The per-tool limits do not reach it: a permitted command can still
  create, change, or delete any file in the workspace.

- `web_fetch` uses its own preference and the workspace host allowlist; see
  [web fetch](#web-fetch-adr-0098).

`task_network` and `host_services` have no model tool. For them `prompt` and
`allow` mean the same today; `prompt` keeps any future model tool for them
gated.

**Waiting in the turn.** In the desktop shell, a call that needs an approval
waits in the turn. The operator decides in the approvals window, and an
approved call completes in the same turn. The call is dispatched again
first, so every live check and digest is repeated before the approval is
consumed. A denied call ends with `approval_denied`, an expired one with
`approval_expired`, and cancelling the turn ends the wait. A call waits at
most three times, and the wait does not count against the turn deadline. The
CLI `chat` command does not wait: it returns `approval_required`, and a later
identical call consumes the approved record.

**Stored form.** Settings keep their earlier bytes. A stored `false` is
`deny` and a stored `true` keeps its earlier meaning; a string is written only
for a state a boolean cannot express, such as `"allow"` for `write`.
`write_tools` is omitted while every limit is `allow`. A stored `write_tools`
without `create_directory` takes its `create` limit for directory creation
([ADR-0079](adr/0079-add-a-model-tool-that-creates-one-directory.md)). A settings log that uses
a new state cannot be read by version 0.42.0 or earlier. A non-canonical value,
such as the string `"deny"`, is refused.

## Model action journal

Every model tool call is recorded in `<data-dir>/model-actions-v1.jsonl`,
whether it ran or was refused (ADR-0063). The file operation, run, and
session journals keep their own records; this journal is the one place that
lists every call with its authorisation.

| Event | Written | Holds |
| --- | --- | --- |
| `action_started` | Before the call acts; synced first | Session, workspace, call ID, tool, targets, request digest, and authorisation |
| `action_finished` | After the call | Outcome (`succeeded`, `failed`, or `uncertain`), code, and the broker, checkpoint, or run record ID |
| `action_refused` | Instead of a start | The reason, and for a rejected call the SHA-256 of its received arguments |

The authorisation is `standing_allow` with the settings revision that held
`allow`, or `approval` with the consumed approval ID. A refusal reason is
`access_denied`, `approval_required`, `approval_denied`, `approval_expired`,
`invalid_request`, `policy_unavailable`, `cancelled`, `unsupported_tool`,
`arguments_too_large`, `invalid_arguments`, or `invalid_path`. The last four
record a call the runtime rejected and answered without running it
(ADR-0099); they have no target, and a call to a tool the runtime does not
have is recorded with the tool `unrecognized` and no name. A build before
0.63.0 reads none of these four reasons or `unrecognized`: it reports the
journal as corrupt, and each model tool call then ends the turn with
`audit_unavailable` until the operator moves `model-actions-v1.jsonl` and
every `model-actions-v1.<20-digit sequence>.jsonl` archive aside. A target is a
validated workspace-relative path, or for `web_fetch`, recorded with the
tool `network`, the normalised URL; a value that failed validation is stored
only as its SHA-256 and byte length.

**After web content (ADR-0098).** Every model action that a session records
after its first successful `web_fetch` carries `after_web_content: true`,
whatever its tool or outcome, including refused and rejected calls. The
runtime finds a session's first fetched content again from its journal at
the start of each turn, so the marker survives a restart and compaction;
other sessions are unaffected. The record names the tool and its targets.
A `workspace_run_command` record has no target: its request digest binds the
program and arguments, which the session and run journals hold, and nothing
names the files the program touched. The desktop's right-hand panel shows a
command's program and arguments from the session journal. The field is omitted while it is `false`, so earlier
records keep their bytes; a build before 0.64.0 reports a journal that holds
it as corrupt. `omgcode actions list --after-web-content [--session <id>]`
lists only these records, newest first, and prints `after_web_content` on
every record; the filters search the 1,000 newest records, so an older call
of a busy journal is not listed. A filtered listing prints `truncated`, true
when that window was full or more calls matched than `--limit`.

If the start cannot be recorded, the call does not run and the turn ends with
`audit_unavailable`. An approval consumed just before that failure is spent.
A finish can be written by a later writer, so the journal is not locked while
a command runs. An executed call without a finish, for example after a crash,
has an unknown outcome, not a failed one.

The journal rotates at 4 MiB or 16,384 events and keeps at most four
archives; a record is at most 32 KiB. `omgcode actions list [--limit N]` lists
the most recent calls, newest first, from 1 to 1,000. Like the other journals
it is private data: targets and codes can reveal workspace names. It is not
tamper-evident against another process running as the same user.

The desktop shell reads this journal, read-only, to list one session's command
calls and which of them were refused or never finished (ADR-0086). It reads
the 1,000 newest records of all sessions, so an older call of a busy journal
is not listed.

## Model read/list and command tools

The paragraphs below describe `write` and `execute` at `prompt`, the state
that `true` has always meant. Under `allow` the approval steps are skipped
and the call runs at once; under `deny` the tool is not offered. See
[deny, prompt, or allow](#deny-prompt-or-allow-adr-0063).

`workspace permissions <id> --revision <n> --model-tools true` lets `chat` offer
the local model two tools for that workspace: `workspace_read_file` and
`workspace_list_directory`. The preference is off by default, and there is no
per-run flag or per-call confirmation. It is a trusted settings change like the
other flags. It needs the expected revision, appends `permissions_changed`, and
increments the revision. It is not an operation class, so `workspace check` and
policy audit records do not mention it.

When `model_tools`, `read`, and `write` are all enabled, the runtime also
offers `workspace_edit_file`. Because `read` and `write` default to enabled,
enabling `model_tools` offers it unless `write` is disabled. It replaces one
exact text occurrence in an existing file, and every call needs an approval:
the first call records a pending `file_write` approval bound to the live
file's size and SHA-256 and writes nothing. After `approvals approve <id>
--writers-stopped`, a later identical call consumes the approval and applies
the edit through a checkpoint. A call made while `read` or `write` is disabled
is denied with `edit_disabled` and ends the turn. Disable `write` to remove
the tool. See [model edit route](file-access.md#model-edit-route-adr-0043) and
[ADR-0043](adr/0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md).

The same three preferences also offer `workspace_multi_edit_file`, which
applies up to 16 exact replacements to one existing file under one approval and
one checkpoint, and `workspace_create_file`, which creates one new file through
the exclusive create without a checkpoint. They share the edit tool's
`file_write` approvals, writers-stopped acknowledgement, per-call recheck, and
`edit_disabled` denial, and each has its own digest domain, so an approval for
one never authorizes another. Disabling `write` or `read` removes all three. See
[model multi-edit and create routes](file-access.md#model-multi-edit-and-create-routes-adr-0054)
and [ADR-0054](adr/0054-add-multi-edit-and-create-model-file-tools.md).

The same three preferences also offer `workspace_rename_file`, which moves one
file to a name that does not exist and never replaces an entry, and
`workspace_delete_file`, which removes one file after a checkpoint of its bytes.
Both refuse a source over 1 MiB. They share the edit tool's `file_write`
approvals, writers-stopped acknowledgement, per-call recheck, and
`edit_disabled` denial, and each has its own digest domain. Renaming is not
checkpointed; a deleted file is restored with the trusted
`file restore --removed`. Disabling `write` or `read` removes all six file-write
tools. See
[model rename and delete routes](file-access.md#model-rename-and-delete-routes-adr-0055)
and [ADR-0055](adr/0055-add-checkpointed-delete-and-no-replace-rename-model-tools.md).

The same three preferences also offer `workspace_create_directory`, which
creates one directory through the broker's exclusive single-directory creation
without a checkpoint. It never replaces an entry and creates no missing
parents: an existing path is `file_exists` and a missing parent is
`parent_not_found`, which `workspace_create_file` and the destination check of
`workspace_rename_file` now also report. It shares the edit tool's `file_write`
approvals, writers-stopped acknowledgement, per-call recheck, and
`edit_disabled` denial, and its digest domain is `omgcode-directory-create-v1`.
No dedicated model tool removes a directory; a command can, when its approved
profile exposes a program that does. See
[ADR-0079](adr/0079-add-a-model-tool-that-creates-one-directory.md).

When `model_tools`, `execute`, `read`, and `write` are all enabled and at
least one toolchain profile of the running platform is assigned to the
workspace, the runtime also offers `workspace_run_command`. Its `profile`
parameter lists those profiles, and a call that names another one is answered
with `profile_not_recorded` before any approval is requested
([ADR-0100](adr/0100-list-the-recorded-toolchain-profiles-in-the-command-tool.md),
[ADR-0107](adr/0107-assign-toolchain-profiles-to-workspaces-and-grant-on-assignment.md)).
Its first call records a pending approval
and does not run; after `approvals approve <id>`, a second identical call
consumes the approval and starts the trusted run. The model cannot approve.
The tool uses fixed limits and no network or host services. See
[model command tool](execution.md#model-command-tool) and
[ADR-0040](adr/0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md).
The tool schema version is 0.10.0, which lists only the toolchain profiles
assigned to the workspace in `workspace_run_command` (ADR-0107); 0.9.0 listed
the recorded profiles (ADR-0100); 0.8.0 added `web_fetch` (ADR-0098);
0.7.0 added the 32 KiB argument limit to the
create, edit, and multi-edit descriptions (ADR-0099); 0.6.0 added
`workspace_create_directory`; 0.5.0 added `workspace_rename_file` and `workspace_delete_file`; 0.4.0 added
`workspace_multi_edit_file` and
`workspace_create_file`, and 0.3.0 added `workspace_edit_file`. Each request
manifest records the version it advertised, and sessions that recorded 0.2.0,
0.3.0, 0.4.0, or 0.5.0 still replay and accept new turns.

`workspace status` always reports `model_tools`. Persisted settings omit the
field while it is `false`, so settings that never enable it keep their exact
bytes and remain readable by version 0.11.0. Once a `true` value is written,
version 0.11.0 refuses the settings log.

Before each turn, `chat` reads current settings and offers the tools only when:

- the build runs on Linux or Windows;
- a control store exists;
- the session's recorded workspace UUID matches a registered workspace;
- that workspace's `model_tools` preference is enabled;
- the session journal uses schema version 2 or later.

Otherwise chat stays text only and reports `model_tools.mode` as `disabled` with
a `reason`: `platform_unsupported`, `settings_absent`,
`workspace_unregistered`, `preference_disabled`, or `session_schema_v1`. Invalid settings fail with a
`control` error before any model request. The recorded UUID only selects a
registration. The workspace ID and preference used come from current settings.

Before every call, the core runtime opens the settings again and creates a
fresh binding. It denies the call with `model_tools_disabled` unless the
preference is still enabled. The broker then requires the live `read`
preference and denies the call with `permission_denied` otherwise. An edit
call also needs live `write` before any read. A denial
ends the turn. A change made during model I/O applies to the next call.

Earlier tool results stay in the journal. A turn sends them to the model again
only while the live `model_tools` and `read` preferences both allow it, checked
when the turn starts. Otherwise, including every text-only turn, each is
replaced by a fixed withheld notice and the output reports
`withheld_tool_results`. Revoking either preference therefore stops further
disclosure of earlier reads; it cannot recall what the model already received.

A model call never reads outside the session workspace, which is registered
beneath the development root. Parsing rejects absolute, drive-qualified, root,
current-directory, and parent paths before dispatch. The broker refuses links,
junctions, hard links, and names outside the workspace. Results are
source-labelled, untrusted user content; the compiled base prompt stays the
only system message. Limits are eight calls per response, 128 calls per turn
by default (configurable from 1 to 256), and 256 KiB of tool output per turn.

The journal selects the workspace. A crafted journal can therefore select
another registered workspace that has `model_tools` enabled. Calls remain inside
that workspace and need its live preferences, and the output names the workspace
ID used. See [ADR-0026](adr/0026-offer-read-list-tools-through-a-workspace-preference.md).

## Session compatibility

With a configured root, new CLI sessions register their selected workspace and
record its stable ID. Invalid settings or an out-of-root selection fail; there
is no fallback to a fresh legacy ID. The control lock is released before the
session journal is created or a model request starts. If session creation then
fails, its registration can remain as a valid independent settings change.

Without a configured root, text-only chat retains the earlier per-session
association ID. This compatibility path creates no development root or access
grant. Existing journals are not rewritten. Resuming a saved text conversation
does not infer workspace authority from its header. Policy bindings are never
restored from JSONL. Session storage and archive rules remain in the
[session contract](session-storage.md).

See the [development guide](development.md) for CLI examples and
[ADR-0012](adr/0012-separate-workspace-settings-from-execution-authority.md) for
the design rationale.
