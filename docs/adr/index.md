# Architecture decision register

Record decisions whose rationale could otherwise be lost: a constraint,
a rejected plausible alternative, or a convention future code must follow.

Use the [template](template.md). Each record must include status and date.
Choose the next number above every existing record; never reuse a number,
renumber a record, or delete one to conceal a changed decision. Add each record
to this register in the same change.

Mark a changed decision superseded and link the replacing record. Update its
owning guide in the same change. A short-lived implementation detail with no
meaningful alternative does not need an ADR.

| Record | Status | Date |
| --- | --- | --- |
| [0000: Record architecture decisions](0000-record-architecture-decisions.md) | Accepted | 2026-11-08 |
| [0001: Adapt contributor rules to Rust](0001-adapt-contributor-rules-to-rust.md) | Accepted | 2026-11-08 |
| [0002: Require subscription authentication](0002-require-subscription-authentication.md) | Accepted | 2026-11-08 |
| [0003: Use scoped development toolchain profiles](0003-use-scoped-development-toolchain-profiles.md) | Accepted | 2026-11-08 |
| [0004: Require pull requests and repository analysis](0004-require-pull-requests-and-repository-analysis.md) | Accepted | 2026-11-08 |
| [0005: Keep agent context subordinate to runtime policy](0005-keep-agent-context-subordinate-to-runtime-policy.md) | Accepted | 2026-11-08 |
| [0006: Mediate MCP through core-owned clients](0006-mediate-mcp-through-core-owned-clients.md) | Accepted | 2026-11-08 |
| [0007: Store session journals as dated JSONL](0007-store-session-journals-as-dated-jsonl.md) | Accepted | 2026-11-08 |
| [0008: Compile the base prompt and separate user preferences](0008-compile-the-base-prompt-and-separate-user-preferences.md) | Accepted | 2026-11-08 |
| [0009: Require user-selected project and global memories](0009-require-user-selected-project-and-global-memories.md) | Accepted | 2026-11-08 |
| [0010: Build a local chat core before execution](0010-build-a-local-chat-core-before-execution.md) | Accepted | 2026-11-08 |
| [0011: Reserve terminal journal capacity](0011-reserve-terminal-journal-capacity.md) | Accepted | 2026-11-08 |
| [0012: Separate workspace settings from execution authority](0012-separate-workspace-settings-from-execution-authority.md) | Accepted | 2026-11-08 |
| [0013: Enforce file access with native handles and exclusive creation](0013-enforce-file-access-with-native-handles-and-exclusive-creation.md) | Accepted | 2026-11-08 |
| [0014: Use the renamed application namespace without automatic migration](0014-use-the-renamed-application-namespace-without-automatic-migration.md) | Superseded by ADR-0020 | 2026-11-09 |
| [0015: Archive operation logs without consuming settings capacity](0015-archive-operation-logs-without-consuming-settings-capacity.md) | Accepted | 2026-11-09 |
| [0016: Add a session-owned tool runtime with session schema v2](0016-add-a-session-owned-tool-runtime-with-schema-v2.md) | Accepted | 2026-11-09 |
| [0017: Preview edits before enabling replacement](0017-preview-edits-before-enabling-replacement.md) | Accepted | 2026-11-09 |
| [0018: Define checkpointed replacement before enabling overwrites](0018-define-checkpointed-replacement-before-enabling-overwrites.md) | Accepted | 2026-11-09 |
| [0019: Validate replacement records before native storage](0019-validate-replacement-records-before-native-storage.md) | Accepted | 2026-11-09 |
| [0020: Rename the project to OMGCode](0020-rename-the-project-to-omgcode.md) | Accepted | 2026-11-10 |
| [0021: Store checkpoints with native private handles](0021-store-checkpoints-with-native-private-handles.md) | Accepted | 2026-11-10 |
| [0022: Publish one verified stage on Linux ext4](0022-publish-one-verified-stage-on-linux-ext4.md) | Accepted; amended by ADR-0068 | 2026-11-10 |
| [0023: Store Windows checkpoints with verified ACLs](0023-store-windows-checkpoints-with-verified-acls.md) | Accepted; amended by ADR-0068 | 2026-11-11 |
| [0024: Publish one verified stage on Windows NTFS](0024-publish-one-verified-stage-on-windows-ntfs.md) | Accepted; amended by ADR-0068 | 2026-11-11 |
| [0025: Restore and delete checkpoints explicitly](0025-restore-and-delete-checkpoints-explicitly.md) | Accepted | 2026-11-11 |
| [0026: Offer read/list tools through a workspace preference](0026-offer-read-list-tools-through-a-workspace-preference.md) | Accepted | 2026-11-11 |
| [0027: Confine execution with native primitives, starting with a process-tree core](0027-confine-execution-with-native-primitives-starting-with-a-process-tree-core.md) | Accepted | 2026-11-11 |
| [0028: Confine Linux runs with Landlock and a socket filter](0028-confine-linux-runs-with-landlock-and-a-socket-filter.md) | Accepted | 2026-11-11 |
| [0029: Confine Windows runs in a workspace AppContainer with recorded grants](0029-confine-windows-runs-in-a-workspace-appcontainer-with-recorded-grants.md) | Accepted; amended by ADR-0068 | 2026-11-11 |
| [0030: Run trusted commands through recorded toolchain profiles](0030-run-trusted-commands-through-recorded-toolchain-profiles.md) | Accepted | 2026-11-11 |
| [0031: Limit whole run trees with job objects and delegated cgroups](0031-limit-whole-run-trees-with-job-objects-and-delegated-cgroups.md) | Accepted | 2026-11-12 |
| [0032: Allow outbound network for trusted runs through task_network](0032-allow-outbound-network-for-trusted-runs-through-task-network.md) | Accepted | 2026-11-13 |
| [0033: Bound run disk growth by sampling writable roots](0033-bound-run-disk-growth-by-sampling-writable-roots.md) | Accepted | 2026-11-13 |
| [0034: Gate package networking behind enforceable destination control](0034-gate-package-networking-behind-enforceable-destination-control.md) | Accepted | 2026-11-13 |
| [0035: Fetch packages through a trusted allowlisted cache](0035-fetch-packages-through-a-trusted-allowlisted-cache.md) | Accepted | 2026-11-13 |
| [0036: Expose the package fetch cache read-only to Linux runs](0036-expose-the-package-fetch-cache-read-only-to-linux-runs.md) | Superseded by ADR-0037 | 2026-11-14 |
| [0037: Use a per-workspace cache root and a scoped Windows grant](0037-use-a-per-workspace-cache-root-and-a-scoped-windows-grant.md) | Accepted | 2026-11-14 |
| [0038: Record provider profiles without credentials](0038-record-provider-profiles-without-credentials.md) | Accepted | 2026-11-14 |
| [0039: Gate future model tools with bound, single-use approval records](0039-gate-future-model-tools-with-bound-single-use-approval-records.md) | Accepted | 2026-11-14 |
| [0040: Gate the model command tool with approvals and the trusted runner](0040-gate-the-model-command-tool-with-approvals-and-the-trusted-runner.md) | Accepted | 2026-11-14 |
| [0041: Compact active context with readable summaries](0041-compact-active-context-with-readable-summaries.md) | Accepted | 2026-11-15 |
| [0042: Store provider credentials in protected OS storage](0042-store-provider-credentials-in-protected-os-storage.md) | Accepted | 2026-11-15 |
| [0043: Gate the model file edit tool with approvals and checkpointed apply](0043-gate-the-model-file-edit-tool-with-approvals-and-checkpointed-apply.md) | Accepted | 2026-11-16 |
| [0044: Observe journals without the writer lock and index them in SQLite](0044-observe-journals-without-the-writer-lock-and-index-them-in-sqlite.md) | Accepted | 2026-11-16 |
| [0045: Build the desktop shell on Tauri with a narrow command surface](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md) | Accepted | 2026-11-16 |
| [0046: Decide model tool approvals in an isolated desktop window](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md) | Accepted | 2026-11-16 |
| [0047: Read the local provider token from the credential store](0047-read-the-local-provider-token-from-the-credential-store.md) | Accepted; amended by ADR-0048 | 2026-11-16 |
| [0048: Bind a stored credential to its profile's endpoint](0048-bind-a-stored-credential-to-its-profile-endpoint.md) | Accepted | 2026-11-16 |
| [0049: Stream local model replies and push chat events to the main window](0049-stream-local-model-replies-and-push-chat-events.md) | Accepted | 2026-11-16 |
| [0050: Interrupt a streamed model request when a turn is cancelled](0050-interrupt-a-streamed-model-request-on-cancellation.md) | Accepted; amended by ADR-0051 | 2026-11-16 |
| [0051: Cancel every model request and let an interrupt cancel a CLI turn](0051-cancel-every-model-request-and-interrupt-cli-turns.md) | Accepted | 2026-11-17 |
| [0052: Publish rolling testing builds from main](0052-publish-rolling-testing-builds-from-main.md) | Accepted; amended by ADR-0058 | 2026-11-17 |
| [0053: Compact the checkpoint ledger in place](0053-compact-the-checkpoint-ledger-in-place.md) | Accepted | 2026-11-17 |
| [0054: Add multi-edit and create model file tools](0054-add-multi-edit-and-create-model-file-tools.md) | Accepted | 2026-11-17 |
| [0055: Add checkpointed delete and no-replace rename model tools](0055-add-checkpointed-delete-and-no-replace-rename-model-tools.md) | Accepted | 2026-11-17 |
| [0056: Send registry credentials to one declared origin](0056-send-registry-credentials-to-one-declared-origin.md) | Accepted | 2026-11-17 |
| [0057: Import skills into a private registry and load them on explicit selection](0057-import-skills-into-a-private-registry-and-load-them-on-explicit-selection.md) | Accepted | 2026-11-18 |
| [0058: Stage the latest release as a draft before the swap](0058-stage-the-latest-release-as-a-draft-before-the-swap.md) | Accepted | 2026-11-18 |
| [0059: Configure trusted settings in an isolated desktop settings window](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md) | Accepted | 2026-11-18 |
| [0060: Change workspace settings from the desktop with native dialogs](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md) | Accepted | 2026-11-19 |
| [0061: Check every recorded settings state in run watchers](0061-check-every-recorded-settings-state-in-run-watchers.md) | Accepted | 2026-11-19 |
| [0062: Send the desktop message on Enter as a saved chat preference](0062-send-the-desktop-message-on-enter-as-a-saved-chat-preference.md) | Accepted | 2026-11-19 |
| [0063: Deny, prompt, or allow each preference, wait for approvals in the turn, and journal model actions](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md) | Accepted | 2026-11-19 |
| [0064: Choose the model connection in settings and add Ollama Local](0064-choose-the-model-connection-in-settings-and-add-ollama-local.md) | Accepted | 2026-11-19 |
| [0065: Redesign the desktop windows with one dark theme and split layouts](0065-redesign-the-desktop-windows-with-one-dark-theme-and-split-layouts.md) | Accepted | 2026-11-19 |
| [0066: Choose the chat model from the local server's model list](0066-choose-the-chat-model-from-the-local-servers-model-list.md) | Accepted | 2026-11-19 |
| [0068: Allow ReFS, btrfs, XFS, tmpfs, and ZFS for trusted apply](0068-allow-refs-btrfs-xfs-tmpfs-and-zfs-for-trusted-apply.md) | Accepted | 2026-11-20 |
