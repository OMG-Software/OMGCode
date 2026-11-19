# 0059: Configure trusted settings in an isolated desktop settings window

- Status: Accepted
- Date: 2026-11-18
- Implemented: phase S1 in version 0.40.0; phase S2 in version 0.41.0
  ([ADR-0060](0060-change-workspace-settings-from-the-desktop-with-native-dialogs.md),
  which uses `rfd` instead of the dialog plugin named in decision 4); the
  active connection and local provider profiles of phase S3 in version
  0.42.0 ([ADR-0062](0062-choose-the-model-connection-in-settings-and-add-ollama-local.md),
  which extends decision 7 to `ollama_local` profiles and lets the window
  supply a new profile's loopback endpoint). API-key entry, the rest of S3,
  and phases S4 and S5 are not implemented.

## Context

The user requires that every setting the trusted CLI can configure is also
configurable in the desktop application. Examples given: the development root
("code root"), connections to language models, and general preferences.

Today the desktop shell (ADR-0045) has no settings interface. Workspace
settings, provider profiles, credentials, toolchain profiles, container grants,
package profiles, and skills change only through the trusted CLI. Chat options
are typed for each run in the `main` window and are not saved. The provider
profile and endpoint are fixed by launch arguments (ADR-0047, ADR-0048).

The CLI settings fall into these groups:

| Group | CLI commands | Effect |
| --- | --- | --- |
| Development root and workspaces | `workspace set-root`, `register`, `set-package-cache`, `clear-package-cache` | Takes host paths. Widens what tools can reach |
| Workspace permissions | `workspace permissions` (`model_tools`, `read`, `write`, `execute`) | Enables model writes and command execution |
| Language-model connections | `provider-profile-add`, `-show`, `-remove`, `provider-credential-set`, `-remove`; chat `--provider-profile` | Records endpoints and stores secrets |
| Execution setup | `profile-add`, `-show`, `-remove`, `container-grant`, `container-revoke`, `package-profile-*`, `package-credential-*` | Host paths, sandbox grants, secrets |
| Skills | `skills add`, `list`, `show`, `enable`, `disable`, `inherit`, `remove` | `add` takes a host path. A skill grants no permission |
| Chat preferences | chat `--model`, `--context-tokens`, `--max-output-tokens`, `--instructions`, `--compact-threshold`, `--auto-compact`, `--stream`, `--skill` | No authority |

Commands that perform an operation rather than change a setting stay out of
scope: `run`, `packages fetch`, `file`, `approvals request` and `consume`,
`runs`, `session` recovery, and `workspace check` and `audit`.

These constraints shape the design:

- **The `main` webview renders hostile text.** Transcripts, tool arguments, and
  session titles can be model- or repository-controlled. The security rules
  keep permission-setting controls separate from rendered untrusted content.
- **No command accepts a host path.** The rules forbid a webview from naming a
  path, yet the development root, workspace registration, toolchain roots,
  package cache roots, container grants, and skill import all need one.
- **Two existing rules conflict with the requested behaviour.** The security
  rules say a webview request cannot choose the provider profile, and that
  model and API keys stay out of frontend state. The user chose to switch the
  active connection and to type API keys in the desktop application. This
  record amends those rules deliberately.
- **Core owns every authorization decision.** Settings mutations already need
  the expected global revision, and core validates every value. The desktop
  must add no second authority and no second validation rule.

## Decision

1. **A third window, `settings`.** The shell gains a `settings` webview
   window beside `main` and `approvals`. It loads its own bundle, starts
   hidden, and is shown on request from `main`. It renders only
   operator-recorded values through core's display-safe functions: workspace
   folders, profile names, endpoints, grant targets, and skill names. It never
   renders transcript content, tool output, or a skill's description or body,
   because those are repository- or model-controlled.
2. **Settings commands exist for `settings` only.** Every command that reads
   the full configuration or changes a setting is granted to the `settings`
   window alone, in the same two layers as ADR-0046: the capability file for
   `main` does not grant it, and each handler checks the calling window in Rust
   as its first statement against the one policy table. `main` gains only
   `settings_show_window` and the chat-preference commands in point 8.
3. **Core does the work.** Each settings command calls the same `omgcode-app`
   or `omgcode-core` function the CLI calls, with the same validation, the
   same expected-revision check, and the same audit events. The CLI commands
   stay unchanged. Moving any CLI-only logic that the shell needs into
   `omgcode-app` is part of each phase.
4. **Host paths come from a native folder dialog that Rust opens.** A command
   that needs a folder takes no path. Rust opens the operating system's folder
   dialog, receives the chosen path, and passes it to core. The webview
   receives the folder display text only. Cancelling the dialog returns
   `cancelled` and changes nothing. The planned implementation is the Tauri
   dialog plugin called from Rust. No window is granted any `dialog:`
   permission, so the webview cannot open a dialog itself or receive a path.
   The dependency is justified and reviewed in the phase that adds it.
5. **Authority-raising changes need a native confirmation.** Before core
   applies a change that widens access, Rust shows a native message dialog
   with text that Rust composes from core's display-safe values, for example
   "Allow command execution in workspace `<folder>`?". The change is applied
   only if the operator confirms it there. The dialog is outside any webview,
   so a compromised `settings` webview cannot approve its own request.
   Changes that need this confirmation:
   - Setting or replacing the development root.
   - Registering a workspace.
   - Enabling `model_tools`, `read`, `write`, or `execute`.
   - Recording a container grant, a toolchain profile, a package cache root,
     or a package-network profile.
   - Selecting the active provider profile.
   Narrowing changes (disabling a permission, revoking a grant, removing a
   profile, a credential, or a skill) need no native confirmation. A narrowing
   change that deletes stored data asks for confirmation in the window only.
   Enabling a skill grants no permission, so it needs no native confirmation.
6. **API keys are typed into a write-only field in `settings`.** The user
   chose this over a native prompt and over keeping keys CLI-only.
   - The field is a password input. The frontend sends its value once, in the
     set-credential command, then clears the field. It stores the value
     nowhere else: no state object, no log, and no error text.
   - Rust wraps the value in the zeroing secret type, applies the checks of
     the matching CLI command (for example the 16 KiB bound, and
     `user:password` for a `basic` package credential), stores it through
     core's credential store, and never returns it. No command returns a secret, its length, or a digest of it.
   - The secret passes through the `settings` webview's memory and the IPC
     message. That is a weaker boundary than the CLI's standard input and is
     the cost of this choice. The window renders no model- or
     repository-controlled text, which narrows who can reach it.
   This amends the rule that keeps model and API keys out of frontend state:
   the `settings` window may carry a key from its input field to one command,
   and nothing else may hold it.
7. **The active connection is a trusted setting chosen in `settings`.**
   - The operator selects one recorded `lm_studio` profile by name in the
     `settings` window, with native confirmation. Core records the selection.
     The webview never supplies an endpoint or a token. Each turn still resolves
     the endpoint and the credential from that one profile (ADR-0048).
   - A selection applies to turns that start after it is recorded. A running
     turn keeps the connection it started with.
   - A launch argument wins. With `--provider-profile` or `--endpoint`, the
     connection is fixed for the process and `settings` shows it as set at
     launch. Without either, the recorded selection applies, and with no
     selection the default endpoint and `OMGCODE_LM_TOKEN` apply as today.
   - `main` cannot choose a profile or read which credential is used, as now.
   This amends the rule that a webview request cannot choose the provider:
   only the `settings` window can, and only among recorded profiles.
8. **Chat preferences are saved, non-authority preferences.** The `main`
   window may read and save its own defaults: model name, context size,
   output reserve, compaction threshold and mode, and custom instructions.
   They are stored in the data root, bounded, validated again by `chat_send`
   when a turn uses them, and grant nothing. Streaming stays a launch
   argument (`--stream`) in phase S1; saving it as a preference is deferred
   to a later phase. Choosing skills for one
   turn (the CLI's `--skill`) is added to the `main` composer, limited to
   skills core reports as enabled for that workspace. Skills still load as
   untrusted, labelled messages (ADR-0057).
9. **Delivery in phases.** Each phase is one pull request with its own tests,
   documentation, changelog entry, and version:
   - **S1:** the `settings` window, its bundle, capability file, and policy
     entries; a read-only view of the configuration; saved chat preferences;
     per-turn skill selection in `main`.
   - **S2:** development root, workspace registration, workspace permissions,
     and package cache roots, with the folder dialog and native confirmation.
   - **S3:** provider profiles, provider credentials, and the active
     connection.
   - **S4:** toolchain profiles, container grants, package-network profiles,
     and package credentials.
   - **S5:** skill import, enablement, inheritance, and removal.

## Alternatives considered

- **Change settings from the `main` window.** Rejected: one script-injection
  flaw in transcript rendering would then reach the permission controls and
  could enable `execute` and `write` for every workspace.
- **A review token and a one-second delay, as in ADR-0046, without a native
  dialog.** Rejected for authority-raising changes: the same `settings` webview
  would request the token and redeem it, so a compromised window could approve
  its own change. The token in ADR-0046 binds a decision to a reviewed record.
  Here there is no separately recorded request to bind to.
- **Typed host paths, checked in Rust.** Rejected: it adds a path-carrying
  command, and a compromised window could name any path. The folder dialog
  keeps a person in the selection.
- **A native credential prompt for API keys.** Stronger, because the key never
  enters web content. Not chosen by the user. Linux has no standard prompt, so
  it would need a separate implementation per platform. It remains a later
  hardening option.
- **Keep secrets CLI-only.** Not chosen by the user. It would leave the parity
  requirement unmet for connections.
- **Keep the active connection fixed at launch.** Not chosen by the user. It
  would require a restart to change the connection.
- **Tauri's isolation pattern.** Deferred, as in ADR-0046: per-window
  capabilities and the Rust window check already remove the commands from
  `main`.

## Consequences

- The desktop application can configure what the CLI configures, except the
  commands listed as out of scope. The CLI stays the headless surface.
- The shell's command count grows in each phase. Each command needs a policy
  entry, a capability entry, a handler that authorizes first, and tests that
  keep the build script, policy, capability files, and frontend names in step.
- A compromised `settings` webview can narrow access, remove profiles and
  credentials, select skills, and trigger dialogs. It cannot widen access
  without the operator confirming a native dialog, cannot name a host path,
  and cannot read a stored secret. It can read every key typed into it while
  it is compromised.
- A compromised `main` webview can change chat preferences and choose enabled
  skills for a turn. Both grant nothing.
- The native dialogs add a dependency and per-platform behaviour. On Linux the
  dialog depends on the desktop environment or the XDG portal, and no window
  has yet been opened on Linux (ADR-0045).
- Display-safe text in the native confirmation has the limits recorded in
  ADR-0046: it does not detect look-alike letters or mixed scripts.
- The security coding rules change when each phase lands, not before: S1
  adds the `settings` window rule, S2 the folder-dialog rule, and S3 the
  amended provider-selection and API-key rules. Until then the current rules
  apply.

## References

- [ADR-0045](0045-build-the-desktop-shell-on-tauri-with-a-narrow-command-surface.md)
- [ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)
- [ADR-0047](0047-read-the-local-provider-token-from-the-credential-store.md)
- [ADR-0048](0048-bind-a-stored-credential-to-its-profile-endpoint.md)
- [ADR-0057](0057-import-skills-into-a-private-registry-and-load-them-on-explicit-selection.md)
- [Desktop interface](../desktop-interface.md)
- [Workspace settings](../workspace-permissions.md)
- [Security coding rules](../security-coding-rules.md#trust-and-permissions)
- [Tauri capabilities](https://tauri.app/security/capabilities/)
