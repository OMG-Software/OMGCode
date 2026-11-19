# Changelog

Changes are recorded here before a release version is assigned.

## Unreleased

### Changed

- Version 0.45.0 redesigns the three desktop windows with one dark theme
  (ADR-0065, accepted). No command, IPC shape, permission, or Rust authority
  check changed.
  - **Theme:** one dark palette of near-black greys with an orange accent
    replaces the light and dark sets, and `color-scheme` is `dark`. Orange
    marks actions, focus, selection, and "waiting for you"; a permission at
    Allow is solid neutral and Prompt is outlined in orange. A new token test
    checks WCAG AA contrast for every text colour on every surface, and 3:1
    for control borders and focus. Rust opens each window on the same
    background colour, and the `approvals` and `settings` windows open at 960
    by 720 (minimum 600 by 480).
  - **Shared components:** `ui/src/ui/` adds a segmented control, a tab
    strip, and a `<details>` popover, with no IPC imports.
  - **Approvals window:** a queue sits beside the review pane, so decided
    records no longer push **Approve** and **Deny** down. **Pending (n)** and
    **History (n)** are separate tabs. The window selects the
    soonest-expiring pending request on opening and the next one after each
    decision; a poll that only adds a request keeps the selection. Every newly
    shown review starts with writers-stopped unticked and its own one-second
    Approve delay, and focus goes to the review heading, never to Approve. No
    key approves or denies; the up and down arrows move through the pending
    list and Escape returns to it. Edits are drawn as a bounded line
    difference (2,000 lines a side, edit distance 2,000), with a fallback to
    the old and new blocks. The effect, the checkbox, and the buttons stay in
    a bar at the bottom of the pane, and the digest, tool, and binding move
    into **Technical details**. Only the newest detail answer is shown, so a
    late answer cannot display a review token that a newer request replaced.
    Because the window now opens a review by itself, a file-write review's
    audited broker read also happens on opening and after a decision, still
    one detail request per shown review (two reads for a rename). A failed
    review load stays on screen until another request is selected and is
    never retried by a poll.
  - **Settings window:** a sidebar chooses Model, Workspaces (the default),
    or a read-only Advanced section. Each workspace preference is one row
    with a description and a Deny, Prompt, or Allow segmented control, and
    the file-write tool limits sit in a disclosure under Write, with a
    "Limited by Write" note on a tool wider than `write`. With two or more
    workspaces a field chooses which one is shown. The model connection,
    its profiles, and **Add connection** share one section. The window
    sends the same requests as before.
  - **Main window:** search and a **View** menu (active or archived, sort,
    direction) replace the crowded sidebar header, and Approvals and
    Settings move to the bottom of the sidebar. The approval banner becomes a
    status-bar segment, "N approvals waiting" with **Review**. Tool requests
    are collapsed rows with a one-line argument preview, tool results compact
    rows, and turn notes thin dividers. Skills and chat settings move into
    composer menus, and the byte counter appears only above 80% of the limit.
- Vitest now processes CSS (`css: true`). It previously replaced every CSS
  module with an empty string, so the stylesheet remote-resource scan read
  empty text; the scan now also asserts that each stylesheet is non-empty.

### Added

- Version 0.44.0 adds Ollama Local, a model-provider dropdown in the
  desktop `settings` window, and a status bar in the `main` window
  (ADR-0064, accepted; the connection part of ADR-0059 phase S3).
  - **Ollama Local:** an `ollama_local` profile can record a loopback
    endpoint, such as `http://127.0.0.1:11434/v1`, under the same rule as
    `lm_studio`. The existing local Chat Completions client serves it; a
    probe of Ollama 0.40.16 on Windows returned the same reply, tool-call,
    and stream shapes, now kept as core test fixtures. The profile has
    authentication `none`, so no `Authorization` header is sent. The
    buffered (non-streaming) adapter drops the numeric `index` member that
    Ollama adds to each tool call, so its calls pass the strict runtime
    parser, as streamed calls already did. CLI `chat
    --provider-profile` and desktop `--provider-profile` accept it.
    `capabilities` reports `streaming_adapters` and `cancellable_adapters` as
    `["lm_studio", "ollama_local"]`. The endpoint refusal now reads "a local
    provider endpoint must be ...". Ollama's context length must be set in
    Ollama; the OpenAI-compatible API cannot change it.
  - **Active connection in core:** the control journal records
    `ActiveProviderSelected`. Only a local profile with an endpoint can be
    selected; an unchanged selection is refused; replay checks each one
    again; removing the selected profile clears it. The snapshot writes
    `active_provider` only when set, so older journals keep their bytes; a
    journal that records a selection cannot be read by 0.43.0. The
    new trusted CLI command `workspace provider-profile-select <name>` (or
    `--default`) records it. CLI `chat` does not read it.
  - **Settings window dropdown:** **Model provider** lists the default LM
    Studio endpoint and every selectable profile. Choosing one calls
    `settings_select_connection` with the profile name only, and Rust records
    it after a native Yes/No dialog that names the provider and endpoint.
    **Add a local connection** (`settings_add_local_connection`) records an
    Ollama or LM Studio profile with a typed loopback endpoint, prefilled
    with the provider's usual address. **Remove**
    (`settings_remove_connection`) removes a profile without a credential;
    removing the active one asks a native Yes/No first, because new turns
    then go to the default endpoint with `OMGCODE_LM_TOKEN`. One with a
    credential is refused with `credential_bound`.
  - **Launch arguments still win:** with `--endpoint` or `--provider-profile`
    the dropdown is disabled and selection is refused with
    `connection_fixed`. Without either, each turn resolves the active
    connection when it starts, and the default endpoint and
    `OMGCODE_LM_TOKEN` apply while none is chosen.
  - **Main window:** `chat_defaults` returns the endpoint the next turn uses.
    A send whose endpoint is stale is refused with `connection_changed`; the
    composer then shows the new endpoint and keeps the draft and model name.
    The endpoint note now reads "Chosen in the Settings window, or fixed when
    OMGCode starts."
  - **Status bar:** a footer along the bottom of the `main` window shows the
    open session's workspace, its Git branch, and a **Model provider**
    dropdown. `connection_status` lists the default and the local profiles
    with an endpoint. Choosing one calls `connection_select` with the profile
    name only; Rust shows the same native Yes/No dialog as the settings
    window before switching, and the composer then shows the new endpoint.
    `session_context` reads the branch from `HEAD` through the anchored
    broker, with no Git process, and follows a linked worktree's `gitdir`
    only inside the development root. The footer reads both again on focus,
    on session open, and after a turn.
  - The shell now has 34 commands: 22 for `main`, 3 for `approvals`, and 9 for
    `settings`. API-key entry in the window, execution setup, and skills
    still change only through the CLI.

- Version 0.43.0 lets the operator deny, prompt for, or allow each workspace
  preference, waits for approvals inside the desktop turn, and records every
  model action (ADR-0063).
  - **Three states:** `read`, `write`, `execute`, `task_network`, and
    `host_services` are each `deny`, `prompt`, or `allow`. `prompt` and
    `allow` both permit a trusted operator command. For a model tool call,
    `prompt` needs a single-use approval and `allow` runs it without one.
    `model_tools` stays on or off.
  - **Per-tool write limits:** `write_tools` holds a limit for each of the
    edit, multi-edit, create, rename, and delete tools. Each defaults to
    `allow` and can only narrow `write`; a `deny` limit removes the tool. The
    command tool uses the most restrictive of `execute`, `read`, and `write`,
    and the limits do not reach it.
  - **Prompted reads:** under `read` prompt, the read and list tools record a
    `file_read` approval for the exact path and limit and read nothing until
    it is approved. A read approval needs no writers-stopped acknowledgement.
  - **CLI:** `workspace permissions` takes `deny`, `prompt`, or `allow` for
    `--read`, `--write`, `--execute`, `--task-network`, and
    `--host-services`, and the new `--edit-tool`, `--multi-edit-tool`,
    `--create-tool`, `--rename-tool`, and `--delete-tool`. The earlier `true`
    and `false` still work and keep their meaning. `workspace status` reports
    states as strings and adds `write_tools`.
  - **Settings window:** each preference and each tool limit has a Deny,
    Prompt, or Allow selector; `model_tools` stays a checkbox. A change
    towards Allow asks for a native confirmation that names the old and new
    states.
  - **In-turn approvals:** a desktop turn now waits while the operator
    decides. An approved call is dispatched again, so every live check is
    repeated, and then completes in the same turn. A denial ends the call with
    `approval_denied`, expiry with `approval_expired`, and cancelling the turn
    ends the wait. Waiting does not count against the turn deadline. The CLI
    `chat` command does not wait. The approvals window says whether approving
    runs the request now or only lets a later identical request run, and the
    approval detail reports `waiting`.
  - **Standing-allow commands:** a model command run without an approval
    records `standing_allow`. The runner rechecks that the command state is
    still `allow` under its own settings lock, and the watcher withdraws the
    run once it is not, so a change to `prompt` cannot start or keep an
    unapproved command.
  - **Model action journal:** `<data-dir>/model-actions-v1.jsonl` records each
    executed call before it acts, with how it was authorised (a standing
    `allow` and its settings revision, or the consumed approval), and its
    outcome afterwards. A refused call is recorded with the reason. A call
    whose start cannot be recorded does not run and ends the turn with
    `audit_unavailable`. A target that fails validation is stored only as its
    digest and length. The journal rotates at 4 MiB or 16,384 events into
    four archives. `omgcode actions list [--limit N]` lists recent calls.

- Version 0.42.0 makes Enter send the desktop composer's message and
  Shift+Enter add a newline (ADR-0062, accepted). Before this, only
  Ctrl+Enter sent.
  - **Enter to send:** a check box in the composer's settings panel, on by
    default. Turning it off makes Enter add a newline again. Ctrl+Enter
    (Cmd+Enter) sends in both modes. An Enter that confirms an input-method
    composition never sends. The message box hint names the current send key.
    A change made before the saved defaults load is kept.
  - **Saved with the chat preferences:** **Save as defaults** stores the choice
    as `enter_to_send` in `chat-preferences.json`, and `chat_defaults` returns
    it beside the turn settings. A file or request without the field reads it
    as on, so files saved by version 0.41.0 keep working. An older build
    reports a file with the field as unreadable and uses its built-in values.
  - The panel is renamed from **Model settings** to **Chat settings**.
  - No command, permission, or capability changed. The choice grants nothing
    and never reaches a turn.
- Version 0.41.0 lets the desktop `settings` window change workspace settings
  (Phase S2 of ADR-0059; ADR-0060, accepted). Before this, the window was
  read-only and asked the operator to run `omgcode workspace status` first.
  - **No setup command:** `settings_overview` creates an empty settings store
    when none exists, as the CLI's `workspace status` does. The store records
    no root, workspace, or permission. The settings window loads at launch, so
    the store exists from the first launch. The `store_initialized` field and
    the "No settings store exists yet" message are removed.
  - **Five new `settings` commands:**
    - `settings_choose_root` sets the development root.
    - `settings_register_workspace` registers a folder beneath the root.
    - `settings_set_permissions` sets a workspace's complete set of
      `model_tools`, `read`, `write`, `execute`, `task_network`, and
      `host_services`.
    - `settings_choose_package_cache` and `settings_clear_package_cache` set
      and clear a workspace's package cache root.

    Each calls the same core function as the matching CLI command, with the
    same revision check, validation, and audit event, and returns the new
    overview. None takes a path.
  - **Native dialogs:** Rust opens the operating system's folder picker for
    every folder. It shows a native Yes/No confirmation before each change
    that widens access: the root, a registration, a preference turned on,
    and a cache root. The service writes the confirmation text and names
    each preference turned on with what it allows. Turning preferences off
    and clearing a cache root show no dialog. The dialogs come from the new
    dependency `rfd` 0.17.2 (MIT; `gtk3` backend on Linux, `windows-sys` on
    Windows), called from Rust. No Tauri plugin and no dialog command
    exists. `rfd` is the only new package in `Cargo.lock`.
  - **Safe ordering:** no settings lock is held while a dialog is open. A
    change made meanwhile makes the request fail with the new code
    `stale_revision`, so the change the operator confirmed is the one
    applied. Runs that lost access are stopped and observed
    (`runs_not_stopped` otherwise). One change runs at a time
    (`settings_change_running`). A closed dialog or a No answer returns
    `cancelled`. A missing root returns `no_development_root`.
  - **Settings window controls:** the development root has **Choose
    folder…** or **Change…**. Each workspace has a checkbox per preference
    and package cache **Choose…** and **Clear**. **Register workspace…** is
    disabled until a root is set. Controls are disabled while a change waits
    for the operator. After a failure or a declined dialog, the window reads
    the configuration again.
  - **Main window:** when no workspace is registered, the new-session dialog
    offers **Open settings** instead of CLI commands.
  - The shell now has 28 commands: 19 for `main`, 3 for `approvals`, and 6 for
    `settings`. Layer tests check the new commands across the policy,
    capability file, build list, handlers, and frontend keys. A new test
    keeps every Tauri plugin out of the shell. Bundle tests keep every
    settings command out of the main and approvals bundles. Language-model
    connections, credentials, execution setup, and skills still change only
    through the CLI (phases S3 to S5).
- Version 0.40.0 adds the first desktop settings increment (Phase S1 of
  ADR-0059, accepted). ADR-0059 records the design for configuring every CLI
  setting in the desktop application, delivered in phases S1 to S5. Only S1
  is implemented; the desktop still changes no trusted setting.
  - **Settings window:** a third webview window, `settings`, with its own
    capability file, bundle, and HTML page. It starts hidden and opens from
    **Settings** in the session sidebar through `settings_show_window`. Its one
    command, `settings_overview`, returns a read-only view of the trusted
    configuration: the settings revision, development root, workspaces and
    their preferences and package cache roots, the connection this process
    uses, provider, toolchain, and package-network profiles, container
    grants, and skill enablement. Paths and recorded names are display-safe
    text. No secret, environment value, skill description, or skill body is
    returned. The window has no input field and names the CLI commands that
    change each setting.
  - **Saved chat preferences:** `chat_preferences_save` stores the model name,
    context size, output reserve, custom instructions, compaction threshold,
    and automatic compaction switch in
    `<data_root>/desktop-v1/chat-preferences.json`, at most 512 KiB, written
    through a synced temporary file and a rename. No endpoint can be saved,
    and streaming stays a launch argument.
    `chat_defaults` now returns the settings with a `preferences` source of
    `defaults`, `saved`, or `unreadable`; an unreadable file falls back to the
    built-in values and the composer shows a note. A failed write returns the
    new error code `preferences`.
  - **Skills for one turn:** `session_skills` lists the names of the skills
    enabled for a session's registered workspace, and `chat_send` takes a
    required `skills` list of at most four names. Core loads each as for
    `chat --skill` (ADR-0057), so a skill that is no longer enabled fails the
    turn before any model request.
  - The shell now has three windows and 23 commands. The `settings` capability
    grants only `settings_overview`, which `main` cannot call; bundle tests
    keep the settings command out of the main and approvals bundles and the
    decision and chat-event commands out of the settings bundle.
  - The security coding rules add the S1 rules for the configuration view,
    saved preferences, and skill selection. Phases S2 to S5 (the native folder
    dialog, native confirmations, the write-only API key field, and choosing
    the active connection) are not implemented.

- Version 0.39.0 adds the first skills increment (Phase F1): trusted import of
  a skill folder into private storage, a registry with application and
  workspace enablement, and loading of an enabled skill into one chat turn on
  explicit selection. See ADR-0057 and the
  [implemented subset](docs/skills.md#implemented-subset-phase-f1). It adds
  one dependency, `saphyr-parser` 0.3.4 (MIT OR Apache-2.0), which brings
  `arraydeque` 0.5.1; it is used only for YAML events.
  - **Import:** `omgcode skills add <folder> --revision N` reads the folder
    through the native broker with a pinned identity and copies every regular
    file, create-only, into `<data_root>/skills-v1/packages/<skill_id>/`.
    Links, junctions, reparse points, special and multi-link files, hidden or
    non-portable names, and case-insensitive collisions refuse the whole import.
    Bounds: 128 files, 64 directories, depth 4, 128 entries per directory,
    1 MiB per file, 4 MiB in total, 64 KiB for `SKILL.md`, 8 KiB of
    frontmatter, and 32 KiB of body. The package digest is SHA-256 over a
    sorted list of file digests, sizes, and paths. Nothing is executed and the
    source folder is never changed.
  - **Format:** one YAML document with the Agent Skills fields as scalars and a
    flat scalar `metadata` mapping. Aliases, anchors, tags, sequences, flow
    collections, nesting, extra documents, duplicate keys, unknown keys, YAML
    directives, a byte order mark anywhere before the body, and NUL are
    refused. A plain `null`, `Null`, `NULL`, `~`, or empty value means the
    optional field is absent; quoted text stays literal. `allowed-tools` is
    advisory text only, at most 1,024 bytes (`allowed_tools_too_long`).
  - **Registry:** the control journal gains `skill_added`, `skill_removed`, and
    `skill_enablement_set`, each revisioned and revalidated on replay. At most
    64 skills, one per name. A new skill is disabled; a workspace override beats
    the application setting. `skills list`, `show`, `enable`, `disable`,
    `inherit`, and `remove` print JSON; `show` prints the body as a JSON string.
    `remove` records the removal before deleting the copy and reports a copy it
    cannot delete as a `package_orphaned` warning. Each registered skill
    reserves journal room for its removal, so `skills remove` succeeds when
    other changes have filled the journal. File reads and writes are refused
    once a permission withdrawal would no longer fit beside the grant and
    skill reserves, so the operator can always narrow file access. A registration write whose outcome
    is uncertain keeps the copied package and reports `registration_uncertain`
    with its identifier; an import never deletes a package directory it did
    not create. Replay refuses a package size that its file count cannot hold.
    `skills list` and `show` refuse an unregistered `--workspace`
    (`skill_workspace_unknown`). A control journal with skill records is
    refused by 0.38.0 and earlier.
  - **Loading:** `omgcode chat --skill <name>` (up to four) loads each skill
    only when it is registered and effectively enabled for the session's
    workspace and its stored `SKILL.md` matches the recorded size and digest
    (`skill_unavailable`, `package_tampered`, `skills_too_large`). Each body is
    sent as one labelled, untrusted user-level message after the base prompt and
    saved preferences and before the history, on both the text-only and
    tool-enabled paths, and is counted in the context estimate. The request
    manifest gains an optional `skills` list of identifier, name, and digest,
    written only when present; a session journal with it is refused by 0.38.0
    and earlier, and no schema version changed. A version 1 or 2 session
    refuses a skill selection (`skill_session_version`). The skill message is
    never written to the session, replayed, or sent to the compaction
    summarizer; the validated model reply is, and it can repeat skill text.
    Every turn rechecks the selection after compaction and before its first
    request. A tool-enabled turn also rechecks before each later model request
    and, under the settings lock that admits it, before each tool dispatch. A
    turn ends as `skill_withdrawn` when a selected skill's registry record was
    removed or replaced, its recorded digest changed, or it was disabled, even
    if it was enabled again. The recheck reads the registry, not the stored
    copy, so a deleted or edited stored `SKILL.md` affects later loads only.
    The context estimate counts each skill message's per-message overhead.
    `chat` JSON lists the loaded skills.
  - **Capabilities:** `capabilities` reports `skills` with `import`,
    `registry`, `chat_loading: ["cli"]`, and `scripts: false`.
  - **Not implemented:** Git sources, versioned updates, session overrides,
    agent selection, resource loading, script execution, desktop controls, and
    the desktop transcript does not show a request's skills. A command that is
    already running is not stopped when a skill selected for its turn is
    withdrawn, and the runner's own admission does not recheck skills. A
    withdrawal after a recheck passes applies to the next request or dispatch.

- Version 0.38.0 adds two approval-gated model file tools, rename and delete
  (Phase C19). See ADR-0055 and the
  [rename and delete routes](docs/file-access.md#model-rename-and-delete-routes-adr-0055).
  Until now the model could neither move nor remove a file. It was developed
  on branch `codex/model-file-tools` and merged to `main` in PR #40. It adds no
  dependency.
  - **Tools:** `workspace_rename_file` moves one regular file to a name that
    does not exist and never replaces an entry. `workspace_delete_file` removes
    one regular file after a durable checkpoint of its bytes. Both are advertised
    and dispatched only under live `model_tools`, `read`, and `write`, with the
    edit tool's per-call recheck and `edit_disabled` denial, in the order read,
    list, edit, multi-edit, create, rename, delete, command. Tool schema 0.5.0;
    0.2.0 to 0.4.0 sessions still replay. Eight of eight tool-name slots are used,
    and no cap rose.
  - **Approvals:** both reuse the `file_write` operation, so the
    writers-stopped acknowledgement is required and no enum, wire type, or
    TypeScript union changed. Each has its own digest domain tag
    (`omgcode-file-rename-v1`, `omgcode-file-delete-v1`), so an approval for any
    of the five file-write tools is never consumed by another. Before an
    approval is recorded, a live audited read supplies the source's exact size
    and SHA-256; a source over 1 MiB is the recoverable `edit_too_large`, and
    non-UTF-8 content is accepted. A rename also needs an absent destination
    (`file_exists`) and an existing parent (`not_found`).
  - **Rename:** a new `FileBroker::rename_file` records a `rename_file`
    file-operation intent that names both the source and the destination, then
    calls the platform's `rename_no_replace`. The source is pinned, its bytes
    are verified through the pinned handle, and directories, links, junctions,
    hard-linked files, and other filesystems are refused. Linux uses
    `renameat2` with `RENAME_NOREPLACE` and compares the name with the pinned
    inode before and after. Windows holds a DELETE handle that denies other
    writers and deleters and renames with POSIX semantics and no replace flag.
    An error after the native call is uncertain, leaves the audit record pending,
    and is never retried. Rename is not checkpointed.
  - **Delete:** a new `FileBroker::apply_removal` follows the checkpointed
    apply without a stage: the same binding, permission, admission, and
    qualified-filesystem gates, a durable checkpoint, a recheck, commit intent,
    one native removal, and the same uncertain-outcome recovery. Linux unlinks by
    name and reports a swapped name as uncertain from the pinned inode's link
    count; Windows deletes through the verified handle. Neither is power-loss
    qualified.
  - **Ledger:** operation records gain an optional `kind` of `replace` or
    `remove`, omitted for `replace` so every earlier ledger, carried record, and
    manifest stays byte-identical. A `remove` proposes exactly the empty artifact,
    has no stage, and goes from `checkpoint_prepared` to `commit_intended`.
    Replay refuses a `remove` that proposes bytes, has a stage record, or carries
    a stage identity; compaction carries `kind`. Older binaries refuse a ledger
    that holds a `remove` record. A `remove` entry's original need not be UTF-8.
  - **Restore:** trusted `file restore --removed` (and
    `FileBroker::restore_removed_file`) recreates a deleted file through the
    audited exclusive create when its name is free, records no new operation, and
    does not restore its mode, owner, access list, or timestamps. A replacement
    checkpoint and a removal checkpoint each refuse the other route.
  - **Review:** the desktop approvals window reproduces the digest from any of the
    five file-write tools (at most eight distinct candidates) and shows `From`,
    `To`, `Size`, and `SHA-256` for a rename and `File`, `Size`, and `SHA-256`
    for a delete. A rename review is stale once its source changed or its
    destination exists; a delete review is stale once the file changed or is gone.
  - **Limits:** neither tool detects a concurrent writer, and neither is
    power-loss qualified. The context estimate for tool definitions rises from
    5 KiB to 6 KiB (the eight definitions serialize to 5,968 bytes). The Linux
    paths are not qualified on a Linux host. A bounded read that exceeds its
    limit now carries a typed marker, `is_oversize_entry`, so a source over
    1 MiB is told from other invalid objects; the error kind is unchanged.
  - **Tests:** new platform tests cover the no-replace rename (existing target,
    directories, junctions, hard links, changed source, a swap the sharing mode
    prevents, and faults before and after the call) and the verified-handle
    removal (swapped name refused, single attempt, faults). New core tests cover
    the `remove` ledger rules and refusals, removal and restore through the
    broker, the audit record, strict parsing, the approval flow, cross-tool
    non-consumption, refusals, live-permission gating, output-fit refusal, schema
    0.4.0 replay, the eight-tool list, and review fields and staleness. New app,
    CLI, and frontend tests drive both tools through the desktop service, the
    operator flow, and `file restore --removed`.

- Version 0.37.0 adds registry credentials for the trusted `packages fetch`
  (Phase C20). See ADR-0056 and
  [registry credentials](docs/execution.md#registry-credentials). Until now
  fetch sent no credentials, so a private registry could not be used. It was
  developed on branch `codex/model-file-tools` and merged to `main` in PR #40.
  It adds no dependency.
  - **Binding:** `workspace package-credential-set <profile> --origin <origin>
    --auth bearer|basic --revision <n>` reads one secret from piped input only
    and binds it to one declared origin of a package-network profile.
    `package-credential-remove` unbinds and deletes it, and removing the
    profile unbinds every credential in the same event. Secrets are stored
    through the existing DPAPI or Secret Service store; the control journal
    records only the opaque ID and backend. Bearer secrets are visible ASCII
    without spaces; Basic secrets need a non-empty user before the first `:`.
  - **Fetch:** the request carries a sensitive `Authorization` header only when
    the matched origin, chosen by the longest path prefix, has a binding.
    Redirects stay refused, so the header cannot follow one. A bound secret
    that cannot be read refuses the fetch before a start record, and nothing is
    sent. The start record and report carry an optional `credential_id`.
  - **Limits:** the secret still crosses the network to the declared origin,
    which may log it. A journal with the new events cannot be read by an older
    version. Only loopback HTTP is tested; live HTTPS registries are not.
  - **Tests:** header shape for both schemes, no header on an unbound origin,
    no request when the secret is missing, no credential on a refused
    redirect, journal replay and refusals, store ordering, base64 vectors, and
    a CLI workflow that scans every data file and output for the secret.
- Version 0.36.0 adds two approval-gated model file tools, multi-edit and create
  (Phase C18). See ADR-0054 and the
  [multi-edit and create routes](docs/file-access.md#model-multi-edit-and-create-routes-adr-0054).
  Until now the model could change a file only one exact replacement at a time,
  each needing its own approval and checkpoint, and could not create a file.
  It was developed on branch `codex/model-file-tools` and merged to `main` in
  PR #40. It adds no dependency.
  - **Tools:** `workspace_multi_edit_file` applies one to 16 exact replacements
    to one existing file under one approval and one checkpointed apply.
    `workspace_create_file` creates one new UTF-8 file through the broker's
    exclusive create. Both are advertised and dispatched only under live
    `model_tools`, `read`, and `write`, with the edit tool's per-call recheck
    and `edit_disabled` denial. Tool schema 0.4.0; 0.2.0 and 0.3.0 sessions
    still replay. Six of the eight tool-name slots are used, and no cap rose.
  - **Approvals:** both reuse the `file_write` operation, so the
    writers-stopped acknowledgement is required and no enum, wire type, or
    TypeScript union changed. Each has its own digest domain tag
    (`omgcode-file-multi-edit-v1`, `omgcode-file-create-v1`), so an approval for
    `workspace_edit_file`, multi-edit, or create is never consumed by another
    tool. The approval flow itself is one shared gate; the edit tool's digest,
    codes, and messages are unchanged.
  - **Multi-edit rules:** every `old_text` must occur exactly once in the
    original live bytes and the matches may not overlap (`edit_overlap`);
    touching matches are allowed. The messages name the 1-based edit index and
    never file text. The result must differ and fit 1 MiB.
  - **Create rules:** before an approval is recorded, a live audited read must
    find the path absent (`file_exists`) and a listing must find the parent
    (`not_found`). A file that another writer creates after approval is never
    replaced; the call returns `file_exists` and the approval stays spent.
    Creation is not checkpointed. An uncertain creation is a terminal failure
    that leaves the pending file-operation record for the trusted
    acknowledgement.
  - **Review:** the desktop approvals window reproduces the digest from any of
    the three tools (at most eight distinct candidates in total) and shows
    numbered `Old text N` and `New text N` fields for a multi-edit and
    `File`, `Size`, `SHA-256`, and `Content` for a create. A create review is
    stale once its path exists. The approvals label reads "File change".
  - **Limits:** the 4 KiB argument cap bounds create content and multi-edit
    size. There is no multi-file atomicity and no concurrent-writer detection,
    and neither tool is power-loss qualified. No model delete or rename tool
    exists. The schema estimate for context accounting rises from 4 KiB to
    5 KiB (the six definitions serialize to 4,546 bytes).
  - **Windows missing paths:** the platform now exports `is_missing_entry`,
    which recognises the native not-found status Windows keeps inside an
    `ErrorKind::Other` error. A missing path in a read, list, edit, or create
    call is now the recoverable `not_found` on Windows, as it already was on
    Linux, instead of a terminal `native_error`.
  - **Tests:** new core tests cover strict argument parsing, the approval flow,
    cross-tool non-consumption, refusals, live-permission gating, output-fit
    refusal, summaries, and schema 0.3.0 replay. New review tests cover exact
    fields, staleness, the shared candidate bound, and hostile text. New app
    and CLI tests drive both tools through the desktop service and the operator
    flow. New frontend tests cover the review fields and the mock detail.

- Version 0.35.0 compacts the checkpoint ledger in place and raises its bounds
  (Phase A5). See ADR-0053 and
  [ledger compaction](docs/file-access.md#implemented-ledger-compaction-phase-a5).
  Until now the ledger held at most 4,096 records and 8 MiB for an
  installation's lifetime, so about 580 applies with deletion exhausted it.
  It was developed on branch `codex/checkpoint-ledger-compaction` and merged
  to `main` in PR #40. It adds no dependency.
  - **Bounds:** the ledger may hold 65,536 records and 64 MiB, and the store's
    aggregate quota grows from 64 MiB to 128 MiB. The 64-entry bound and the
    64 KiB record bound are unchanged.
  - **Format:** a compacted ledger is the original `created` record, one
    `compacted` record with the replaced ledger's size, SHA-256, record count,
    and revision floor, and one `checkpoint_carried` record per retained entry.
    Replay rebuilds each carried entry through the ordinary transitions and
    requires it to be resolved. `file checkpoints` reports `compacted_from`.
    Readers older than 0.35.0 refuse both new record types.
  - **Triggers:** the trusted `file compact-checkpoints --discard-history`
    command, and apply, restore, and deletion when the ledger lacks room for
    their records. Both refuse while an operation is unresolved or a deletion
    is unfinished, and write nothing when nothing would be dropped.
  - **Publication:** the store replays the candidate, writes and syncs
    `ledger.compact`, reads it back, checks the live ledger is unchanged, and
    replaces `ledger.jsonl` with one handle-relative rename: `renameat` on
    Linux and a POSIX-semantics replace-if-exists `NtSetInformationFile`
    rename on Windows. It flushes the directory and reopens and compares the
    ledger. A failure after the candidate exists is uncertain, poisons the
    writer, and is not retried. The new `PrivateDirectory::replace_file`
    performs the rename and checks that the name then refers to the source.
    Neither the rename nor the flush is power-loss qualified.
  - **Replay:** an append validates only its own record, and operation IDs are
    indexed, so a full replay is linear in its records.
  - **Deleted history:** compaction discards every record of deleted entries.
    Only the replaced ledger's digest, size, and record count remain.
  - **Tests:** new core tests cover carried state, refusal of unresolved work,
    each compaction fault, and compaction at the record cap before apply and
    before deletion. New replay tests cover the record positions, carried
    state reachability, and claim bounds. New CLI tests cover the command, and
    new platform tests cover the native rename on both platforms. The replay
    cap tests and the aggregate quota test use the new bounds.
- A [filesystem and power-loss qualification plan](docs/filesystem-qualification.md)
  defines the release filesystem matrix and the block-level and physical
  power-cut campaigns for the checkpoint store and trusted publication. None of
  it has run, and it changes no support claim.
- A Testing build workflow (`.github/workflows/testing-build.yml`) runs on every
  push to `main`. It calls the Rust and Frontend workflows, builds release
  binaries of `omgcode` and `omgcode-desktop`, and packages them without an
  installer for Windows x86_64 (`.zip`), Ubuntu x86_64 on `ubuntu-latest`
  (`.tar.gz`), and Fedora 44 x86_64 in a digest-pinned container (`.tar.gz`).
  Each package job checks that both binaries start. After every job passes, a
  publish job, the only one with `contents: write`, replaces the rolling
  `latest` prerelease and tag with the archives and a `SHA256SUMS` file. It
  refuses a commit behind the published one, so a re-run of an older run cannot
  roll the release back, and runs queue per ref, so a manual run on another
  branch cannot displace a waiting main run. The workspace tests do not run on
  Fedora. Assets are named `omgcode-<platform>.<zip|tar.gz>`. The Rust and Frontend workflows no
  longer trigger on a push to `main`, gain `workflow_call`, and use
  caller-scoped concurrency groups; the Rust workflow takes a `linux-runner`
  input. There is no application change, so the version stays 0.34.0. This
  workflow has not run on GitHub. See ADR-0052 and
  [the testing build](docs/repository-checks.md#testing-build-workflow).
- Version 0.34.0 lets a cancel request stop every model request, streamed or
  not, and the compaction requests, and lets an interrupt cancel a CLI turn
  (Phase D6). See ADR-0051 and
  [cancelling every request](docs/provider-authentication.md#cancelling-every-request-0340).
  Until now a non-streamed request and every compaction request ran to their
  end or to the thirty-second deadline, a non-streamed text-only turn had no
  cancel token, and Ctrl-C ended the CLI and left an open turn. It was
  developed on branch `codex/request-cancellation` (PR #34), stacked on
  `codex/desktop-interface`, and reached `main` with PR #33 in merge commit
  `9c9f606` on 2026-11-17. It adds no dependency and
  removes reqwest's `blocking` feature, which drops `futures-channel` and
  `futures-sink` from two dependency lists in `Cargo.lock`.
  - **Adapter:** `complete` and `complete_with_tools` take a `CancelToken`, like
    the streamed methods. All four methods share one transport: an async client
    built inside a single-thread runtime created for the request, with the
    settings and the thirty-second deadline of ADR-0050. The token is checked
    before the request is sent, while the response head is awaited, and before
    and while each piece of the body is awaited, every 25 ms. A set token drops
    the request and returns `ProviderError::Cancelled`; a non-streamed body
    read in part is discarded. The 1 MiB response bound is applied as pieces
    arrive. The blocking client is removed. The wire request is unchanged.
  - **Turn:** every chat turn creates one token after the session checks and
    before the compaction check, and `ChatHooks::on_cancel_token` receives it
    on every path, streamed or not. The new `ChatHooks::cancel` lets a front
    end supply the token, and the new `TurnRuntime::with_cancel_token` hands it
    to the runtime. A token already set when the turn would start ends it as
    `cancelled` before the turn marker, recording nothing; the text-only path
    now matches the runtime here.
  - **Compaction:** `compact_journal` takes a token. A cancelled compaction
    fails with the error kind `cancelled` and the message "Compaction was
    cancelled; nothing was recorded", and the journal is unchanged.
  - **CLI:** `omgcode chat` and `session compact` install the interrupt handler
    of the trusted `run` command once the journal is open. Ctrl-C or
    Ctrl-Break on Windows and `SIGINT` or `SIGTERM` on Linux set the token. The
    command fails with the code `cancelled`; a chat turn that had started is
    recorded as cancelled and the error carries the session locator. A handler
    that cannot be installed fails the command with the new code `interrupt`.
    `omgcode-platform` gains `watch_interrupt`, which runs a closure once on an
    interrupt; `cancel_on_interrupt` wraps it. `capabilities` reports
    `cancellable_adapters: ["lm_studio"]`.
  - **Desktop:** no command or wire type changes. `chat_cancel` and exit now
    stop a non-streamed request (`--stream off`) and a pre-turn compaction. A
    turn cancelled before it handed over its token ends before it is recorded.
  - **Tests:** core fixtures for a cancel of a non-streamed text and tool
    request before sending and before the head, a unit test that cancels
    while the rest of a held body is awaited once the head has arrived, and a
    slow non-streamed reply under a live token; the runtime's caller token; app
    tests for cancelled non-streamed text and tool turns, a cancelled pre-turn
    compaction, a cancelled `compact_journal`, and a token set before the turn;
    desktop tests for cancelling a non-streamed turn and for the shutdown grace
    with a turn held in an event sink; Linux CLI tests that deliver `SIGINT`
    during `chat` and `session compact`; and `watch_interrupt` in the platform
    interrupt test. Two tests that asserted the old behaviour were rewritten:
    `a_text_only_turn_..._calls_the_cancel_hook_once` (it skipped the hook) and
    the desktop `cancelling_a_non_streamed_text_turn_stops_its_request` (it
    asserted that cancelling changed nothing). Two others were replaced:
    `a_non_streamed_text_turn_never_calls_the_cancel_hook_and_cannot_be_cancelled`
    and the desktop shutdown-grace test's use of a held non-streamed reply,
    which can now be cancelled.
  - **Still not interruptible:** a read, list, or edit tool call in progress.
    Not verified: a live LM Studio server, and an interrupt delivered to the
    CLI on Windows.
- Version 0.33.0 interrupts a streamed model request when a turn is cancelled
  (Phase D5). See ADR-0050 and
  [cancellation](docs/provider-authentication.md#cancellation-0330). Until now
  cancellation was checked only between model rounds and tool calls, a request
  in flight ran to its end or to the thirty-second deadline, a text-only turn
  had no cancellation point, and on exit the desktop abandoned a turn still
  waiting on the model, leaving an open turn. It was developed on branch
  `codex/desktop-interface` and merged by PR #33 on 2026-11-17. It adds no dependency and no
  cargo feature.
  - **Adapter:** `complete_streaming` and `complete_with_tools_streaming` take a
    `CancelToken`. The streamed request runs on an async HTTP client inside a
    single-thread runtime created for that one request. The client is built
    inside that runtime and is not reused. Its settings equal the blocking
    client's: no proxy, no redirect, no retry, the same connect timeout, and
    the same thirty-second deadline for the whole response. The token is
    checked before the request is sent, so an already cancelled turn makes no
    connection, while waiting for the response head, and while waiting for each
    piece of the body, every 25 ms, and also before each piece. When it is set
    the request is dropped, which closes the connection, and the method returns
    the new error `ProviderError::Cancelled` (`local model request was
    cancelled`). Text already delivered stays delivered and is still
    provisional. The event-stream rules of ADR-0049 are unchanged; the reader
    is now a parser fed bytes in any chunking. The non-streaming methods keep
    the blocking client and cannot be interrupted.
  - **Runtime and recorded outcome:** `ToolModelRequest` carries the turn's
    cancel token and `ModelError` gains `Cancelled`. When a model returns it
    and the token is set, the turn ends as it does for a cancellation between
    rounds: failure reason `cancelled`, error `TurnError::Cancelled`. A model
    that returns it while the token is not set is recorded as a model error. A
    streamed text-only turn now creates a cancel token and hands it to the
    front end through the existing `on_cancel_token` hook. A cancelled request,
    or a cancellation seen after the reply arrived and before it is recorded,
    ends the turn with the failure reason `cancelled`. A non-streamed text-only
    turn creates no token and cannot be cancelled. A cancelled turn has a
    turn-failed record with the reason `cancelled`, no assistant message, and
    no open turn. The partial text is never recorded.
  - **Error kind:** the application layer reports a cancelled turn with the new
    kind `cancelled` and the message `turn was cancelled`. The tool path
    reported it with the general kind `turn` before.
  - **Desktop and frontend:** `chat_cancel` now stops a streamed request. The
    turn ends as `failed` with the error code `cancelled`, `finished` is
    pushed, and the main window shows the plain notice "Turn cancelled." with
    no error styling, decided by the error code only. Exit cancels every
    running turn, so a streamed turn ends within the five-second grace period
    and leaves no open turn. No command, argument, event, or wire type
    changes; the shell still has 19 commands. The content security policy, the
    windows, and the navigation guard are unchanged.
  - **Compatibility:** no journal record changes shape, and sessions stay at
    schema version 3.
  - **Limits:** these are not interrupted: a non-streamed request
    (`omgcode-desktop --stream off`, or the CLI without `--stream`), the
    compaction request before a turn, and a tool call in progress. The CLI has
    no cancel source, so stopping its process still leaves an open turn. A
    long generation still fails at the thirty-second deadline. No live LM
    Studio server was used, and no window has been opened on Linux. The
    Windows shell was driven end to end against a mock that holds a stream
    open, which does not qualify a live server. The CI workflows have still
    never run.
- Version 0.32.0 streams the local model's reply and pushes chat events to the
  desktop window (Phases D4 and E6). See ADR-0049 and
  [streamed replies](docs/provider-authentication.md#streamed-replies-0320).
  Until now the adapter sent one non-streaming request, and the desktop
  learned that a turn had ended by calling `chat_status` every 750 ms. It was
  developed on branch `codex/desktop-interface` and merged by PR #33. It adds no
  dependency.
  - **Adapter:** `LocalModelClient::complete_streaming` and
    `complete_with_tools_streaming` send the same body as the non-streaming
    methods with `stream: true` and `Accept: text/event-stream`.
    `stream_options` is never sent. The non-streaming methods are unchanged
    and remain the default. The response is read as server-sent events under
    strict rules: a success status and the `text/event-stream` content type;
    the 1 MiB response limit and the thirty-second request deadline cover the
    whole stream; LF or CRLF lines; empty lines, comment lines, and the
    `event`, `id`, and `retry` fields are ignored and any other non-`data`
    line is refused; `[DONE]` ends the stream; end of input without `[DONE]`
    is the new error `IncompleteStream` (`local model stream ended before
    completion`); every payload goes through the parser that refuses duplicate
    keys; a non-null `error` member is refused and never reported; zero or one
    choice and one finish reason, with the same accepted finish reasons as
    before. The text method refuses any tool call. Tool calls are assembled by
    `index` (in order from 0, below 64, only `index`, `id`, `type`, and
    `function` with `name` and `arguments`, `id` set once, pieces
    concatenated, a null member treated as absent). The assembled value holds
    only the members received, so the existing tool-call validation applies
    unchanged. Usage counters are present only when the server sends them.
  - **Provisional text and redaction:** text goes to the caller as each payload
    passes validation. It is provisional: a later failure fails the request and
    the delivered text is not a reply. `ChatHooks::on_stream` receives
    `ChatStreamEvent::RequestStarted` before each model request of a turn and
    `ChatStreamEvent::Text` for each piece. A turn sends streamed requests only
    when the hook is set, and compaction requests are never streamed.
    `StreamRedactor` removes the known server token across piece boundaries by
    withholding any tail that could be the start of the token. If its output
    would itself contain the token, it emits nothing further for that request.
    The final reply is still redacted as a whole before it is recorded.
    Streaming changes no journal record, and the journal does not say whether a
    reply was streamed. Sessions stay at schema version 3.
  - **CLI:** `omgcode chat --stream` writes the provisional text to standard
    error as it arrives, escaped by `DisplaySafeStream`, the streaming form of
    core's display-safe escaping, so model output cannot send control sequences
    to a terminal. It writes one line per model request that produced text.
    Standard output still carries the one JSON result. A failed write to
    standard error stops further writes and does not fail the turn. Without the
    flag the request is not streamed. `session compact` has no such flag.
    `capabilities` reports `"streaming_adapters": ["lm_studio"]`.
  - **Desktop:** a new command, `chat_subscribe`, is allowed for `main` only,
    so the shell has 19 commands (16 for `main`, 3 for `approvals`). Its
    argument `on_event` is a Tauri IPC channel created by the caller. The
    service keeps at most one subscriber; a new subscription replaces the old
    one and first replays the preview of every running turn. Events are tagged
    by `kind`: `request_started`, `delta` (text of at most 1,024 bytes),
    `preview_truncated` (the 16 KiB preview bound for one request was reached),
    and `finished` (the turn left `running`; the caller reads `chat_status`).
    The service keeps each running turn's preview for the current request
    only. Preview text is returned as received after token redaction, like
    transcript text. A guard sends `finished` after the result is recorded and
    records a panicked turn as failed first, so `chat_status` read after
    `finished` never reports `running`. Events are sent while the hub lock is
    held, and the `turns` and `events` locks are never held together. A channel
    needs no capability beyond the command's own permission; Tauri events would
    need a `core:event` permission, and both windows still have no `core:`
    permission. The launch argument `omgcode-desktop --stream <on|off>`
    defaults to `on`; `off` sends non-streaming requests, and `finished` is
    still pushed. Any other value is a usage error with exit status 2. It
    exists because the stream rules have not met a live LM Studio server.
  - **Frontend:** the main window subscribes once, on the first send or watch.
    It no longer polls `chat_status` every 750 ms. It reads `chat_status` once
    on `finished`, keeps a five-second reconcile check while a turn runs in
    case an event is lost, and falls back to the 750 ms poll if subscribing
    fails. While a turn generates, the open session shows one live entry
    labelled "Assistant (generating, not saved yet)", updated at most once per
    animation frame and replaced by the recorded transcript when the turn
    settles. A note appears when the preview is cut off. The frontend keeps
    previews for up to 4 sessions it is not tracking and bounds each at 64 KiB
    of UTF-16 code units. It renders preview text as text and marks
    bidirectional overrides. The command name lives in `src/ipc/chat_events.ts`,
    which only the main bundle imports, and a bundle test checks that no
    approvals asset contains `chat_subscribe`. The approvals polls (3 seconds)
    are unchanged.
  - **Compatibility:** the non-streaming adapter methods, the journal records,
    schema version 3, the content security policy, the windows, and the
    navigation guard are unchanged. Cancellation is still cooperative, between
    rounds and tool calls; a request in flight is not interrupted.
  - **Limits:** the stream rules were checked against local fixtures only, and
    no live LM Studio server was used. The Windows shell was driven end to end
    against a paced event-stream mock, which does not qualify a live server.
    No window has been opened on Linux, so the channel is unverified there. A streamed text reply has no usage
    counters unless the server sends them. Preview text can differ from the
    recorded reply: a later failure discards it, and a reply over the 16 KiB
    session message limit is shown in part and then fails as before. The
    request deadline is still thirty seconds for the whole response, so a long
    generation fails as before. The approvals polls and the session tree still
    do not use push. The CI workflows have still never run.
- Version 0.31.0 binds a stored credential to its profile's endpoint (Phase
  D3). See ADR-0048 and
  [endpoint binding](docs/provider-authentication.md#endpoint-binding-0310).
  In 0.30.0 a stored token went to whichever literal loopback endpoint the
  operator named with `--endpoint`. It was developed on branch
  `codex/desktop-interface` and merged by PR #33. It tightens the 0.30.0
  behaviour, which reached `main` only in the same merge and was never
  released, and adds no dependency.
  - **Profile endpoint:** `workspace provider-profile-add <name> ... --endpoint
    <url>` records one endpoint on a profile. `provider-profile-show` and
    `status` print `"endpoint"` only when one is recorded; a profile without
    one prints as before. An endpoint is not a secret. It is fixed for the
    profile's life: no command changes it. To use another endpoint, remove the
    profile and add a new one. Removing a profile already records the unbinding
    and deletes its credential, so a stored credential cannot be moved to
    another endpoint.
  - **Validation:** only an `lm_studio` profile accepts an endpoint. It must be
    `http://<literal loopback IP>:<non-zero port>/v1` (or `/v1/`), with no user
    information, query, or fragment, and at most 256 bytes. This is the rule
    the local adapter enforces on every request. Core has one implementation
    (`validate_base_url`), used by the provider client and the profile record,
    and journal replay applies it again. Every other provider kind refuses an
    endpoint for now. The text is stored exactly as given. The refusals are
    `provider profile refused: an lm_studio endpoint must be
    http://<loopback IP>:<port>/v1 with no user information, query, or
    fragment`, `... the provider endpoint is longer than 256 bytes`, and `...
    this provider kind does not accept an endpoint yet`.
  - **Selection:** a selected profile supplies the endpoint and the token.
    `ControlStore::provider_credential_binding` returns the recorded endpoint
    with the credential decision, so the two cannot come from different
    places. It still takes no credential store. It checks the settings store,
    the profile, the provider kind, the endpoint, and then the authentication
    mode. A selected profile with no recorded endpoint is refused in every
    authentication mode with a `credential` error that starts
    `endpoint_not_recorded:`, before any native store access or request.
    `--endpoint` and `--provider-profile` together are a usage error with exit
    status 2 in `omgcode chat`, `omgcode session compact`, and
    `omgcode-desktop`. The default value of `--endpoint` does not trigger it.
    Without a profile, `--endpoint`, its default, and `OMGCODE_LM_TOKEN` behave
    as before.
  - **Desktop:** with a profile, the shell resolves the endpoint once at
    launch, and a failure exits with status 1 before any window opens. That
    value is the process's fixed endpoint: the webview is shown it and each chat
    request is compared with it. Each turn resolves the profile again. If the
    recorded endpoint no longer equals the launch value, because the profile
    was removed and added again, `chat_send` fails at once with a `credential`
    error that starts `endpoint_changed:` and no turn starts. A replaced
    profile with the same endpoint and a new credential is accepted. The wire
    contract is unchanged.
  - **Compatibility:** the profile record's `endpoint` field is written only
    when present, so a control journal without endpoints replays and serializes
    with its exact bytes. A journal that contains an endpoint cannot be read by
    0.30.0 or earlier, which reject the unknown field. Replay rejects an
    invalid endpoint, an endpoint on a kind that does not accept one, and any
    unknown field. A profile recorded before 0.31.0 has no endpoint and cannot
    be selected until it is removed and added again with `--endpoint`; the
    credential must then be stored again.
  - **Capabilities:** `capabilities` adds `"provider_profile_endpoints": true`.
  - **Limits:** only `lm_studio` profiles accept an endpoint. No endpoint rule
    exists for a non-loopback provider, HTTPS, or certificates, and no cloud
    adapter exists. Any process running as the same user can still read a stored
    credential. A hand-edited control journal is checked only by replay
    validation and is not tamper-evident. The frontend shows nothing about the
    credential source beyond the endpoint text it already displayed.
- Version 0.30.0 adds stored credential use for the local LM Studio adapter
  (Phase D2). See ADR-0047 and
  [stored credential use](docs/provider-authentication.md#stored-credential-use-0300).
  Until now nothing read a stored credential, and the adapter took its optional
  server token only from `OMGCODE_LM_TOKEN`. It was developed on branch
  `codex/desktop-interface` and merged by PR #33. It adds no dependency outside
  the existing workspace set (`omgcode-app` now uses `zeroize`).
  - **Selection:** `omgcode chat --provider-profile <name>` and `omgcode
    session compact --provider-profile <name>` read the token from the
    credential bound to that recorded `lm_studio` profile. `omgcode-desktop
    --provider-profile <name>` does the same for the whole process, fixed at
    launch; the webview cannot choose or see it and the wire contract is
    unchanged. With a profile selected, `OMGCODE_LM_TOKEN` is not read.
    Without one, behaviour is unchanged and the variable remains the
    prototype input, which still does not meet the credential-service
    requirement.
  - **Core checks:** `ControlStore::provider_credential_binding(name,
    adapter_kind)` takes no credential store, so it cannot touch native
    storage. It refuses an unknown profile, a profile of another provider
    kind, a `subscription` profile, and an `api_key` profile with no bound
    credential, and it yields no token for a `none` profile. The native store
    is opened only when it returns a binding.
    `ControlStore::load_provider_credential` then refuses a binding stored by
    another backend before it reads any secret. The adapter passes
    `lm_studio`, so a key recorded for another provider is never sent to it.
  - **Failure:** any failure on the profile path ends the turn with a
    `credential` error before a model request is built. The message is `<cause
    code>: <text>`, with the codes `unknown_profile`, `wrong_provider`,
    `subscription_unsupported`, `not_bound`, `other_backend`,
    `settings_unusable`, `credential_not_found`, `credential_locked`,
    `credential_unavailable`, `credential_corrupt`,
    `credential_prompt_required`, `credential_io`, `credential_refused`, and
    `credential_invalid`. A missing or busy settings store is also a
    `credential` error, with no cause code, and a busy store is not retried.
    There is no fallback to no token or to another token. A locked Secret
    Service is an error; OMGCode still never runs a prompt. In the CLI the
    credential is resolved before the endpoint check and before any session or
    journal exists, so the error JSON has no session locator.
  - **Rotation and lifetime:** the secret is loaded while the settings store is
    held, so a rotation cannot delete the item between the binding lookup and
    the load. The store is released before any network request. A read records
    nothing: no control-journal entry and no revision change, and the session
    journal does not name the profile or the credential. The token is held in
    a zeroing buffer with a redacted `Debug` and no serialization for one
    turn, and the existing redaction of the token from recorded and returned
    text applies. The provider clients' request-header copies and transport
    buffers are not zeroed.
  - **Desktop:** the service resolves the token again for every turn and caches
    nothing, so a rotated credential applies to the next turn. An unreadable
    credential is a synchronous `credential` error and no turn thread starts.
    With a profile selected, the shell also resolves once at launch and exits
    with status 1 and `omgcode-desktop: credential: <cause>: <text>` before any
    window opens. The frontend shows nothing about the credential source.
  - **Capabilities:** `capabilities` adds `"stored_credential_adapters":
    ["lm_studio"]`. `provider_adapters` stays `false`: no adapter exists for
    any provider other than the local LM Studio route.
  - **Limits:** the credential is bound to a provider kind, not to an endpoint,
    so a stored LM Studio token is sent to whichever literal loopback endpoint
    the operator selects. Any process running as the same user can still read
    the stored credential. A memory-only session credential is not
    implemented, and no cloud adapter, subscription sign-in, token refresh, or
    account lifecycle exists.
- Version 0.29.0 adds the desktop interface and session tree (Phase E). See
  ADR-0044, ADR-0045, and ADR-0046, and the
  [desktop interface guide](docs/desktop-interface.md). It is a prototype,
  merged from branch `codex/desktop-interface` by PR #33; it has no installer and has run only on
  Windows. No CLI command changed behaviour: every existing CLI test suite
  passes unmodified, and `model_tools.rs` and `context_cli.rs` moved from
  `omgcode-cli` into the new `omgcode-app` crate.
  - **Journal inspection:** `SessionInspection::inspect` reads one journal
    without any lock call, without following links, and with the writer's
    validator, so a viewer never makes a writer report `Busy`. On Windows a
    lock or sharing violation becomes `Busy` with no retry. Bytes after the last
    line feed are ignored and reported as `incomplete_tail`; the file is never
    changed, and `SessionJournal::open` still rejects such a file. The result
    adds `created_at`, `updated_at`, and an ordered list of transcript entries;
    a request manifest entry carries the model, prompt version, and tool names
    only. `MessageSource` is now public.
  - **Discovery:** `session::discover` and `session::locate` walk only the dated
    `sessions` layout with a strict file-name parser, never open a journal or
    follow a link or reparse point, and stop with `LimitReached` after 50,000
    journals or 500,000 directory entries.
  - **Session index:** `session_index::SessionIndex` keeps a derived SQLite
    index with FTS5 in `<data_root>/session-index-v1/index.sqlite3`. It is a
    cache that is deleted and rebuilt when it is foreign or corrupt at open.
    Refresh is incremental and bounded to 1,000 journals per call; a busy
    journal keeps its last metadata, and an invalid journal is listed with a
    code. Search quotes every term and binds the expression as a parameter; it
    refuses more than 16 terms or 256 characters. Titles and the user,
    assistant, tool, and summary text are indexed; tool arguments, manifests,
    and instructions are not. New dependency: `rusqlite` 0.43.0 with the
    bundled SQLite 3.54.1 (MIT), which compiles C source at build time, so a C
    compiler is required. The lockfile gains `rusqlite`, `libsqlite3-sys`,
    `fallible-iterator`, and `fallible-streaming-iterator`.
  - **Application library:** `omgcode-app` holds the chat turn, compaction,
    session creation, the data-root resolver, token redaction, and
    `DesktopService`. The CLI and the desktop shell both call it. It grants no
    authority. Chat gains a cooperative cancellation hook; the CLI does not use
    it, and it cannot interrupt a provider request in flight.
  - **Approval review:** core adds `review_approval` and display-safe text. It
    recomputes an approval's digest from the recorded tool request with the
    tool handler's own function and returns `exact`, `stale`, or `not_found`.
    It decides, consumes, and applies nothing. `ControlStore` gains
    `workspaces()`, `directory_path()`, and `directory_display()`.
  - **Desktop shell:** the new `omgcode-desktop` crate is a Tauri 2 shell with
    two windows, 18 commands, a capability file per window and no `core:`
    permission, a Rust window check as the first statement of every handler, a
    content security policy, a navigation guard, and a five second bounded
    shutdown. It takes `--data-dir` and `--endpoint` and reads no other
    configuration. The webview receives no host path, journal path, or token.
    Chat tools follow the workspace settings as in the CLI. New dependencies:
    `tauri` 2.12.6 (`wry` and `x11` features only) and `tauri-build` 2.7.5,
    Apache-2.0 OR MIT, with no plugin, tray, updater, asset protocol, or
    devtools feature. The lockfile grows by about 283 entries. A Linux build
    needs the WebKitGTK development packages.
  - **Approvals window:** a separate `approvals` window holds the only decision
    controls. Approving needs an `exact` review binding and a single-use review
    token that lives 120 seconds; a file edit also needs the writers-stopped
    checkbox. The CLI `approvals` commands are unchanged. The window is still
    web content and is not a trusted computing base.
  - **Frontend:** `crates/omgcode-desktop/ui` is strict TypeScript with Vite,
    Vitest (jsdom), and Biome, with one runtime dependency, `@tauri-apps/api`
    2.12.5. Versions are exact, the lockfile is committed, and `.npmrc` sets
    `ignore-scripts=true`. The interface builds its document from text nodes
    only; tests scan for markup injection, inline script, remote resources, and
    browser storage. It polls for chat status and approvals; there is no push
    channel.
  - **Continuous integration:** the Rust workflow installs the Tauri Linux
    packages, sets up Node 24, builds the frontend before Clippy, and has a 40
    minute timeout (was 20). A new Frontend workflow runs the type check, Biome,
    Vitest, and the bundle checks on Ubuntu 24.04 and Windows Server 2025.
    `.gitattributes` checks the frontend out with LF. The root `.gitignore`
    gains `node_modules/` and `dist/`.
  - **Limits:** on Linux the shell builds and the workspace tests pass (WSL
    6.6, Ubuntu 24.04), but no window has been opened there, so its webview,
    IPC, and capability enforcement are unverified on Linux. The workflow
    changes have not run on GitHub, and CodeQL's JavaScript/TypeScript
    selection is unverified. Two link tests are skipped on a host without
    symlink privilege; they ran on Linux. The shell has no packaging, streaming, settings
    interface, development panels, or recovery interface. Corruption inside an
    index table behind a valid header is not rebuilt automatically, and two
    versions sharing a data root rebuild each other's index.
- Version 0.28.0 adds the approval-gated model edit tool (Phase C17). See
  ADR-0043.
  - **Tool:** `workspace_edit_file { path, old_text, new_text }` replaces
    exactly one occurrence of a non-empty `old_text` in an existing UTF-8
    regular file. The current and proposed bytes must each fit 1 MiB, and the
    arguments stay within the existing 4 KiB limit. Unknown fields, missing
    fields, an empty `old_text`, and absolute, rooted, or parent paths are
    refused before dispatch. The tool cannot create, delete, or rename files,
    and it has no multi-edit form.
  - **Offer and dispatch:** chat offers the tool, and the runtime dispatches
    it, only while the workspace's live `model_tools`, `read`, and `write` are
    all enabled. Because `read` and `write` default to enabled, enabling
    `model_tools` now offers the edit tool; disable `write` to remove it. A
    call without live `read` or `write` is denied with `edit_disabled`. The
    tool schema version is 0.3.0; sessions that recorded 0.2.0 still replay.
  - **Approval:** each call reads the live file through the audited broker and
    binds a `file_write` approval to a digest under the domain tag
    `omgcode-file-edit-v1`. The digest covers length-prefixed fields for the
    session, workspace, normalized path, and the size and SHA-256 of both the
    current and the proposed bytes, so the expected value comes from the live
    read and a changed file needs a new approval. The summary holds the path,
    sizes, and 12-character digest prefixes, never file text, with control
    and hidden formatting characters replaced. The first call returns
    `approval_required` and writes nothing; a pending record returns
    `approval_required` and a denied one `approval_denied`.
  - **Writers-stopped acknowledgement:** `approvals approve <id>
    --writers-stopped` is required for a `file_write` record and refused for
    every other operation. The decision records `writers_stopped`, which
    `approvals show` and `list` report. Core `consume_approval` refuses a
    `file_write` record without it; such a record from an earlier journal
    still replays. A control journal holding the acknowledgement cannot be
    read by 0.27.0; journals without it keep their exact bytes.
  - **Apply:** an approved identical call consumes the record and then calls
    the existing `apply_replacement` with the captured size and SHA-256 as
    the expected value. `edit_no_match`, `edit_ambiguous`, `edit_unchanged`,
    `edit_too_large`, `non_utf8`, and the checkpoint admission and conflict
    codes, such as `checkpoint_conflict`, `checkpoint_pending`,
    `checkpoint_busy`, and `checkpoint_unsupported`, are recoverable results
    that let the turn continue. Corrupt or uncertain checkpoint evidence,
    storage I/O failures, and denials end the turn. A consumed approval stays
    spent when the apply fails. A success returns the path, new size and
    SHA-256, and the checkpoint operation ID, never file contents. Neither
    apply route detects a concurrent writer.
  - **Review:** `session inspect` now lists every recorded tool request as
    `tool_requests`, with its turn, call ID, tool, and exact arguments, so the
    operator can review `old_text` and `new_text` before approving.
  - **Capabilities:** `model_write_tools` is `approval_required`, and
    `tool_policy` is
    `read_list_when_workspace_model_tools_enabled_edit_and_command_require_approval`
    on Linux and Windows. `workspace_file_tools` stays `false`.
- Version 0.27.0 adds the provider credential service (Phase D1). See
  ADR-0042. No provider adapter reads a stored credential yet.
  - **Native stores:** `omgcode-platform` stores each credential under a fresh
    random identifier and never overwrites one. Windows writes a DPAPI blob
    (current user, UI forbidden, entropy bound to the store label and
    identifier) create-only into `credentials-v1` of the verified-ACL private
    store, and rechecks the file's exact private DACL before decrypting.
    Linux stores one Secret Service item in the default collection, with
    `application` and `omgcode_credential` attributes. It creates an empty
    placeholder marked `omgcode_state=pending`, sends the secret only through
    the encrypted session, and then removes the marker; a placeholder loads as
    corrupt. A locked collection is reported, never unlocked, and a Secret
    Service prompt is dismissed and refused, never run. Other platforms,
    a missing bus or service, or no default collection are unavailable;
    nothing falls back to plaintext.
  - **Errors:** edited, truncated, or identifier-swapped Windows blobs, a
    broadened DACL, and duplicate Linux items are corrupt, never absent.
  - **Secrets in memory:** an owned buffer, bounded to 16 KiB, zeroed on drop
    and redacted from `Debug`.
  - **Journal:** `provider_credential_bound` and `provider_credential_removed`
    record a profile, an opaque identifier, and the backend under the
    revision rule. `provider_profile_removed` names a bound credential. Only
    `api_key` profiles accept one; subscription and `none` profiles are
    refused. Profile status reports `credential` as none or bound. A control
    journal written by 0.27.0 cannot be read by 0.26.0.
  - **CLI:** `workspace provider-credential-set <profile> --revision <n>`
    reads the secret from piped standard input only, removes one trailing
    newline, stores, binds, and then deletes a replaced credential.
    `provider-credential-remove` and `provider-profile-remove` unbind before
    deleting. An undeletable unbound item is a `credential_orphaned` warning.
    The secret is read before the control store is opened. Credential
    failures have stable `credential_*` error codes, including
    `credential_prompt_required` and `credential_binding_uncertain`, and
    `capabilities` reports `credential_store` and `provider_credentials`.
  - **CI:** the Linux job installs gnome-keyring and unlocks a login
    collection on the user session bus before the tests.
  - **Dependencies:** `zeroize` 1.9 (`alloc`), `secret-service` 5.2
    (Linux, `rt-tokio-crypto-rust`), zbus pinned to 5.19.0 as a direct Linux
    dependency with the features `secret-service` already enables, and the
    `windows-sys` `Win32_Security_Cryptography` feature. No crate was added
    to the lockfile for zbus.
- Version 0.26.0 adds readable context compaction and accounting. See
  ADR-0041.
  - **Manual:** `session compact` summarizes older active messages with the
    selected local model and records a schema-v3 `context.compacted` event.
    Original journal messages are retained.
  - **Automatic:** `chat --auto-compact --compact-threshold <tokens>` compacts
    before a turn when the conservative estimate reaches
    `min(threshold, safe input budget)`. Without auto-compaction, an oversized
    request is refused with a manual-compaction hint.
  - **Active context:** the labelled, untrusted summary replaces messages
    before the recorded boundary. The latest user message and all later
    assistant/tool messages stay verbatim, preserving the current turn and its
    tool pairs.
  - **Tool-result authority:** saved tool results reach the summarizer only
    under live `model_tools` and `read`, the same rule that replays them in a
    turn. The record states whether its summary used tool results; such a
    summary is withheld, and counted as withheld, when a later turn lacks that
    authority. Text-only chat and tool-enabled turns share one replay path for
    the active context.
  - **Validation:** the recorded digest must match the summary, and the first
    retained message must be a user message. A summary that would not shrink
    the active context is refused and records nothing.
  - **Accounting:** the pre-turn estimate counts input only, so the output
    reserve is not counted twice against the safe input budget. Compaction is
    also due at the 128-message and 256 KiB prompt-assembly limits. Summary
    chunks are sized from the summary request's output reserve and its full
    fixed prompt.
  - **Revocation:** live `model_tools` and `read` are rechecked before each
    summarizer request that carries tool-derived content.
  - **Schema 2:** existing schema-version-2 journals keep accepting tool
    manifests; the first draft of this change rejected them.
  - **Meter:** chat reports the conservative input estimate, context and
    output budgets, effective trigger, auto-compaction state, and compaction
    boundary and summary digest.
  - **Limits:** summarizer input is split into bounded chunks with hard chunk
    and depth limits; summaries are at most 8 KiB; a failure, empty result, or
    no reduction fails closed. The configured local token is redacted from
    summarizer input and output.
  - **Not implemented:** mid-turn compaction after tool results, persisted
    auto-compaction settings, provider-native compaction, and caches.
- Version 0.25.0 adds an approval-gated model command tool. See ADR-0040.
  - **Offering:** `workspace_run_command` is advertised only when the
    workspace's live `model_tools`, `execute`, `read`, and `write`
    preferences are enabled. Every call rechecks them; the trusted run checks
    them again.
  - **Arguments:** a recorded toolchain profile, program, at most 64
    arguments, and an optional workspace-relative `cwd`. The tool has no shell
    mode and never re-parses a command line; the model may name an interpreter
    the approved profile exposes.
  - **Approval:** the first call records a pending approval and returns
    `approval_required`; it starts nothing. After `approvals approve <id>`, a
    second identical call consumes the approval and calls the trusted runner.
    The model cannot approve, select limits, or select network/host services.
  - **Limits:** 20-second deadline, 4 KiB per output stream, 1 GiB memory,
    64 processes, 20 seconds of CPU, and 256 MiB of disk growth.
  - **Records:** the session journal records the tool intent/result; the run
    journal records the run. A failed run consumes the approval; no replay
    executes a recorded call.
  - **Qualified:** a successful approved command through the real chat CLI on
    both hosts. A mock provider requests the platform shell's `echo`; the first
    turn records `approval_required`, the operator approves, and the second
    identical turn consumes the record and runs the command through the trusted
    runner. The test asserts an `exited` result with code 0, the bounded output,
    the session tool result, and the consumed approval. On Linux the CLI starts
    in a delegated cgroup; on Windows the scene grants the workspace.
- Version 0.24.0 records provider profiles and trusted per-call approvals.
  See ADR-0038 and ADR-0039.
  - **Provider profiles:** `workspace provider-profile-add/show/remove` record
    a name, provider kind, and authentication mode (`subscription`,
    `api_key`, or `none`). Subscription is limited to OpenAI and Anthropic;
    local providers use `none`. No endpoint, account, token, or credential is
    stored, and no adapter or login uses a profile. `capabilities` reports
    `provider_profiles: true` and `provider_adapters: false`.
  - **Approvals:** `approvals request/list/show/approve/deny/consume` manage
    trusted, single-use records bound to a session, workspace, operation,
    exact request digest, and settings revision. Records expire in 60 to 3600
    seconds; a settings change invalidates them; no argument text is stored.
    The CLI consume command executes nothing, and no model tool consumes an
    approval yet.
  - **Records:** `provider_profile_added`, `provider_profile_removed`,
    `approval_requested`, `approval_decided`, and `approval_consumed` in the
    control journal. A 0.24.0 journal cannot be read by 0.23.0.
- Version 0.23.0 adds a per-workspace package cache root and scoped Windows
  access. See ADR-0037.
  - **Commands:** `workspace set-package-cache <workspace-id> <dir>
    --revision <n>` and `workspace clear-package-cache <workspace-id>
    --revision <n>`. `status` lists roots under `package_cache_roots`.
  - **Rules:** the root must exist, must not overlap application storage or
    the development root, and must not overlap another workspace's root.
  - **Fetch:** `packages fetch` writes into the selected root. Without one,
    Linux keeps the application-storage fallback and Windows has none.
  - **Runs:** a selected root is exposed read-only and named by
    `OMGCODE_PACKAGE_CACHE` on both platforms. Windows requires a read-execute
    container grant on that root and traversable ancestors; without the grant
    the run fails before the program starts. `run_started` records
    `package_cache` when true.
  - **Records:** `package_cache_root_set` and `package_cache_root_cleared` in
    the control journal. A 0.23.0 control journal cannot be read by 0.22.0.
  - **Limits:** no invalidation, eviction, or freshness check; changing the
    root leaves old files and old grants for the operator to manage.
- Version 0.22.0 exposes the fetched-package cache to Linux runs read-only.
  See ADR-0036.
  - **Linux:** when `<data-dir>/package-cache/<workspace-id>` exists as a
    plain directory, the run gets it read-only and `OMGCODE_PACKAGE_CACHE`.
    The parent, journals, settings, and other workspaces stay denied; the run
    cannot write the cache.
  - **Windows:** the cache is not exposed. The AppContainer cannot read
    application storage without an ACL grant, and container grants refuse
    that storage.
  - **Records:** `run_started` records `package_cache` only when true. A 0.22
    journal that records it cannot be read by 0.21.0.
  - **Reserved:** `OMGCODE_PACKAGE_CACHE` cannot be declared in a toolchain
    profile.
  - **Limits:** no invalidation, eviction, or freshness check; configuring a
    toolchain to use the directory is operator work.
- Version 0.21.0 adds the trusted operator command `omgcode packages fetch`
  and a private allowlisted package cache. See ADR-0035.
  - **Command:** `packages fetch <workspace-id> --profile <name> --url <url>
    --name <cache-file> [--max-bytes <n>] [--timeout-secs <n>]`.
  - **Gating:** trusted CLI only; never a model tool. The workspace must be
    registered and enable `task_network`.
  - **Destination:** the URL must match a recorded origin's scheme, host,
    port, and path prefix. Normal use is `https`; `http` is accepted only for
    loopback tests. Credentials, queries, fragments, and redirects are
    refused. Proxy environment variables are ignored.
  - **Transport:** system TLS trust through `reqwest`'s `native-tls` feature;
    certificate validation is never disabled.
  - **Bounds:** one GET, 1 byte to 1 GiB, 1 to 600 seconds. The body is
    streamed, hashed with SHA-256, and stopped at the limit.
  - **Cache:** `<data-dir>/package-cache/<workspace-id>/<cache-file>`, private
    directories and files, exclusive name, temporary file then rename. No
    power-loss claim. The run does not yet read the cache.
  - **Audit:** `package_fetch_started` and `package_fetch_finished` in the
    control journal preserve the settings revision and omit the URL text. A
    control journal written by 0.21.0 cannot be read by 0.20.0.
- Version 0.20.0 records operator-declared package-network profiles as
  configuration only. See ADR-0034.
  - **Commands:** `workspace package-profile-add`, `package-profile-show`, and
    `package-profile-remove` use the expected settings revision. `status`
    lists recorded profiles.
  - **Contents:** a name and 1 to 8 registry origins. An origin is `https`
    with a host, optional port, and optional path prefix; `http` is accepted
    only for `localhost`, `127.0.0.1`, or `::1`. Credentials, queries,
    fragments, wildcards, and path traversal are refused.
  - **Not enforcement:** no run accepts a profile, `--network` remains
    all-or-nothing, and no destination filter exists. A profile never
    authorizes network access or a run.
  - **Records:** `package_profile_added` and `package_profile_removed` events
    in the control journal. A control journal written by 0.20.0 cannot be read
    by 0.19.0; 0.20.0 reads earlier journals.
- Version 0.19.0 bounds each trusted run's net disk growth. See ADR-0033.
  - **Flag:** `--max-disk-mib` (16 to 1048576, default 8192) is required for
    every trusted run. A run without a disk limit is refused before anything
    is recorded.
  - **Enforcement:** the supervisor measures allocated bytes and file lengths
    under the confinement's writable roots before the program starts and once
    a second on a separate thread. Each total has its own budget; passing
    either ends the whole tree and records `limit_exceeded` with `disk`. A
    final measurement runs after the tree is empty. The limit is sampled; it
    is not a filesystem quota.
  - **Fail closed:** a baseline that cannot be measured records `not_started`.
    An unreadable root, or more than 4,000,000 entries, while the run lives
    ends it as `supervision_failed`. Links and reparse points are not
    followed. Other writers to the roots count as the run's growth.
  - **Records:** `run_started` records `max_disk_bytes`, and the `usage` field
    of `run_finished` records `peak_disk_bytes`. A run journal written by
    0.19.0 cannot be read by 0.18.0; 0.19.0 reads 0.18.0 journals.
  - **Not counted:** directory entries, inode or MFT use, and filesystem
    metadata. A hard-linked file is counted once per name. Tested on Windows
    11 build 26300 and WSL 6.6 only.
- Version 0.18.0 adds outbound network access for trusted runs. See ADR-0032.
  - **Gating:** `omgcode run ... --network` needs the workspace
    `task_network` preference. Without both, a run has no IP networking, as
    before. Turning the preference off stops networked runs, recorded as
    `revoked`.
  - **Linux:** the socket filter allows `AF_INET` and `AF_INET6` sockets.
    Every destination is reachable, including loopback and the local
    network. On Landlock ABI 4 and later, an explicit TCP bind stays denied,
    but `listen` on an unbound socket can still accept connections. The run
    can read a fixed list of resolver files and public certificate stores,
    never the private-key directories under `/etc/ssl` and `/etc/pki`.
  - **Windows:** the AppContainer gets the `internetClient` capability only.
    Loopback and private networks stay blocked.
  - **Records:** `run_started` records `network` with the support paths.
    Records written by 0.17.0 still replay.
- Version 0.17.0 limits every trusted run as a whole tree. See ADR-0031.
  - **Limits:** `--max-memory-mib` (default 8192), `--max-processes`
    (default 1024), and `--max-cpu-secs` (default 7200). Values out of range
    are refused before anything is recorded.
  - **Windows:** the run's job object gets a job memory limit and an active
    process limit; CPU time comes from the job's accounting.
  - **Linux:** OMGCode must run in a delegated cgroup v2, for example under
    `systemd-run --user --scope -p Delegate=yes`. Each run gets its own child
    cgroup with `memory.max`, no swap, group OOM kill, and `pids.max`. A run
    outside a delegated cgroup is refused before it is recorded.
  - **Result:** reaching a limit ends the whole tree and records
    `limit_exceeded` with the limit. `run_started` records the limits;
    `run_finished` records CPU time and, where available, peak memory.
    Records written by 0.16.0 still replay.
- Version 0.16.0 adds the trusted CLI command `omgcode run <workspace-id>
  --profile <name> -- <program> [arguments]`. It runs one program in a
  workspace whose `execute` preference is enabled.
  - **Confinement:** always through `run_confined`. Linux uses Landlock and the
    socket filter, with a private per-run scratch directory as `HOME` and
    `TMPDIR`. Windows uses the workspace AppContainer and needs an active
    read-write grant on the workspace.
  - **Program and environment:** the program is found through the profile's
    `PATH` entries, in the workspace, or inside a profile root. The run
    receives only the profile's `PATH`, runner-owned home and temporary
    variables, and the profile's variables.
  - **Options:** `--cwd`, `--timeout-secs` (default 600), and
    `--max-output-bytes` (default 1 MiB). Ctrl+C or `SIGTERM` cancels the
    whole tree.
  - **Preferences:** a run needs `execute`, `read`, and `write`.
  - **Revocation:** while a program runs, a watcher rereads the settings after
    each change. When they no longer allow the run, it stops the whole tree
    and records `revoked`. `workspace permissions`, `profile-remove`,
    `container-revoke`, and `set-root` wait up to 60 seconds for the runs they
    affect to stop, and fail if one does not.
  - **Output:** printed, never stored.
- New trusted commands `workspace profile-add`, `profile-show`, and
  `profile-remove` manage operator-declared toolchain profiles.
  - **Contents:** a name, read-only roots, `PATH` entries inside them, and
    extra variables.
  - **Recording:** each change needs the expected revision and is recorded in
    the control journal by directory identity.
  - **Refused:** roots that overlap the development root or application
    storage; reserved variable names.
  - **Stale profiles:** a moved or replaced directory makes the profile stale,
    and runs refuse it. Remove it and add it again.
- A separate run journal, `runs-v1.jsonl`, records each run's intent before it
  starts and its outcome afterwards.
  - **Recorded:** the profile and its revision, the program, an argument count
    and SHA-256 digest, the working directory, the socket mode, the result,
    the confinement, and byte counts. Argument text and output are not
    recorded.
  - **Pending runs:** a run with a missing outcome or unconfirmed cleanup
    stays pending until `runs acknowledge`. A run whose supervising process is
    still live cannot be acknowledged. `runs audit` lists records, and
    `runs archive` moves a journal with no pending runs.
- A `host_services` workspace preference, off by default, lets a Linux run
  select host Unix-socket access when the run also passes `--host-services`.
  Windows refuses it. Settings logs that record a profile or enable
  `host_services` cannot be read by 0.15.0.
- `capabilities` reports `process_execution: "trusted_cli_only"`,
  `toolchain_profiles`, and the sandbox backend. Settings diagnostics still
  report `execution_allowed: false`, and no model tool starts a process. See
  ADR-0030 and `docs/execution.md`.
- Toolchain acceptance tests, ignored by default, run real Rust, Python, Node,
  C/C++, Go, and Java workflows through `omgcode run`. Each covers builds,
  tests, formatting where available, subprocesses, denied host data, writes
  outside the workspace, loopback connections, and missing dependencies. All
  but Java passed on Windows 11 build 26300 and WSL 6.6; Java has not run
  because no JDK was installed.
  - **Node on Windows** needs `NODE_OPTIONS=--preserve-symlinks
    --preserve-symlinks-main` in its profile. Child processes with piped stdio
    hang in the container, so `node --test` needs `--test-isolation=none`.
  - **Ubuntu's Node** needs `/etc/ssl` as a profile root.
- Version 0.15.0 adds Windows AppContainer confinement to
  `process::run_confined`, for tests only. Each workspace has one container,
  `OMGCode.Workspace.<id>`, with no capabilities, so it has no network access,
  including loopback. It reaches only directories granted to it and paths that
  every application package can read, such as the Windows directory. Before
  anything starts, Windows checks that the container profile exists, that
  `LOCALAPPDATA` is in the environment, that each listed path and the program
  are granted at their level, and refuses `HostServices` with `Unsupported`.
  New trusted CLI commands, `workspace container-grant` and
  `workspace container-revoke`, give and remove persistent ACL grants:
  read-write only on the workspace directory, read-execute on the workspace,
  inside it, or outside the development root. The control journal records each
  intent before the ACL change and each outcome after it, keeps unconfirmed
  grants visible and revocable, and keeps room to revoke every outstanding
  grant when it is otherwise full. The first grant creates the container
  profile and revoking the last one deletes it. `workspace status` lists
  outstanding grants and `capabilities` reports `workspace_container_grants`.
  Settings logs that record a grant cannot be read by 0.14.0. A grant also
  reaches hard-linked files under their other names. Nothing runs in the
  container from the CLI yet. See ADR-0029 and `docs/execution.md`.
- Version 0.14.0 adds `process::run_confined` to the platform crate, for tests
  only. On Linux it applies a Landlock filesystem allowlist of read-only and
  read-write paths and a seccomp filter that refuses IP and other non-Unix
  sockets and `io_uring`. Landlock-handled filesystem operations are denied
  outside the listed paths; metadata changes such as `chmod` and `chown`, and
  device `ioctl` before ABI 5, are not restricted. Descendants inherit the
  rules. It needs Landlock ABI 3 or later and starts nothing
  otherwise, or when a path is missing, is a symbolic link, or has an
  unsupported type. A per-run `UnixSocketAccess` mode defaults to `PairsOnly`,
  which allows only stream and sequenced-packet `socketpair`; `HostServices`
  also allows connecting to host Unix sockets such as the Docker daemon and is
  for explicit operator use. The outcome records the Landlock ABI and the mode.
  Windows returns `Unsupported` until AppContainer confinement exists. No CLI
  command or model tool calls it. See ADR-0028 and `docs/execution.md`.
- Version 0.13.0 adds a process-tree runner core to the platform crate, for
  tests only. It starts one absolute-path program with an exact environment,
  null input, and bounded output. It ends the whole tree on exit, deadline, or
  cancellation and reports success only after observing the tree empty.
  Windows assigns a Job Object at creation and inherits only the standard
  handles. Linux uses a process group, a seccomp filter that denies `setsid` and
  `setpgid`, a parent-death signal, and close-on-exec descriptors. It applies no
  filesystem or network confinement, and no CLI command or model tool can call
  it. The accepted confinement is Landlock and seccomp on Linux and
  AppContainer on Windows; see ADR-0027 and `docs/execution.md`.
- Version 0.12.0 lets chat offer the local LM Studio model two tools,
  `workspace_read_file` and `workspace_list_directory`, for a workspace whose
  new `model_tools` preference is enabled. The preference is off by default and
  is set with `workspace permissions --model-tools true`. Settings logs that
  never enable it stay readable by 0.11.0. Each call rechecks the live
  `model_tools` and read preferences and stays inside the registered workspace;
  parent, absolute, rooted, and link paths are refused. There is no per-call
  approval prompt. A turn makes at most four model requests within 120 seconds.
  Tool results are source-labelled user messages, and the configured server
  token is removed from them before they are recorded or sent. `workspace
  status` reports `model_tools`, and `capabilities` reports
  `model_read_list_tools` and a new `tool_policy` value. Live LM Studio
  tool-call compatibility is not qualified. One opt-in Windows spot-check with
  LM Studio 0.4.28 and `google/gemma-4-e4b` passed; see ADR-0026. Saved tool
  results are sent again only while the live `model_tools` and `read`
  preferences allow it; otherwise, and in every text-only turn, each is replaced
  by a withheld notice and counted in `withheld_tool_results`. A tool call that
  contains the server token is refused before it is recorded. Tool-enabled
  output reports the final `finish_reason`, and platforms other than Linux and
  Windows stay text only with `platform_unsupported`.
- Version 0.11.0 adds trusted `file restore` and
  `file delete-checkpoint --permanent` on Linux ext4 and Windows NTFS. Restore
  writes a retained checkpoint's original bytes back through the normal apply
  path. It requires the current file's digest and size and a stopped-writers
  acknowledgement, and it records the replaced bytes in a new checkpoint.
  Deletion removes one resolved checkpoint's fixed files without recursion. It
  keeps the operation's history and frees its retained slot. An interrupted
  deletion blocks other file work until the same request finishes it. Version
  0.10.0 refuses a ledger that contains deletion records. `capabilities` reports
  `checkpoint_restore` and `checkpoint_deletion`. Neither command is power-loss
  qualified; see ADR-0025.
- Version 0.10.0 enables checkpoint storage and trusted `file apply` on Windows
  NTFS. Apply preserves the owner, group, DACL, and content-indexing opt-out, and
  refuses metadata it cannot reproduce. The stage stays owner-only until the
  commit attempt, and a failure before the rename tries to restore that DACL.
  The opt-out changes only while the stage is private, so a crash between the
  two metadata writes leaves a stage that recovery accepts.
  A crash or failed restore in that window can leave a retained stage with the
  source DACL, which may expose unpublished bytes to principals it admits.
  Windows makes no power-loss claim and is not qualified for release; see
  ADR-0024.
- Add a Windows NTFS private-store backend with exact protected per-user DACLs,
  handle-relative access, and a failing directory flush. Microsoft documents no
  durability contract for the flush; see ADR-0023.

### Changed

- The Testing build's publish job now uploads the archives to a draft
  prerelease under a run-specific `testing-build-staging-` tag name, checks
  each asset's name and size, and only then deletes the previous `latest`
  release and publishes the draft as `latest`. The `latest` downloads used to
  fail for the whole upload; now they fail only during that final swap. If the
  publish call fails after the old release is deleted, the exit trap removes
  the draft and no `latest` release exists until a re-run of the publish job
  or a later successful run publishes one. An exit trap deletes an unpublished
  draft, and each run first removes drafts that a stopped run left behind. See ADR-0058, which amends ADR-0052. The new flow
  was tested locally against a stub `gh` (normal swap, first publish, orphan
  tag, refused rollback, failed upload, asset mismatch, failed publish call,
  failed draft listing); it has not run on GitHub. The repository-checks guide
  and coding rules no longer say that the workflows have not run: Testing build
  run 44405313895 at `3208b03` passed on Ubuntu and Windows and published the
  first `latest` release.

- The implementation plan's baseline now records PRs #33, #34, #36, and #37
  as merged. It had still listed the desktop interface and Phases D2 to D6 as
  unmerged branches with no pull request, and it did not mention the Testing
  build workflow. The plan now notes that the workflow's first runs were still
  queued on 2026-11-17 and that no `latest` release existed yet. The older
  unreleased entries for versions 0.29.0 to 0.34.0 no longer call that work
  unmerged. The plan, the contributor guide, and the platform support table
  no longer say that every push or merge to `main` is tested and published:
  while a Testing build run is active, GitHub keeps only the newest waiting
  run, so an intermediate push can be superseded. Documentation only.
- Package-network destination enforcement is defined as a contract only in
  ADR-0034. No run accepts a package-network profile, and `--network` remains
  all-or-nothing. An advisory proxy or proxy environment variables are
  explicitly rejected because a confined tree can bypass them. This is a
  documentation change; no application version is bumped.
- The SonarQube Cloud project key is now `OMG-Software_OMGCode`, matching the
  key a Sonar administrator set on the server. `sonar-project.properties` and
  the repository-checks guide use the new key.
- `workspace` settings commands wait up to 10 seconds for another command's
  control lock instead of failing at once. Run watchers hold that lock briefly
  while they reread the settings.
- `cancel_on_interrupt` returns an `InterruptWatch` that stops and joins its
  watcher thread, instead of leaving a detached thread for the life of the
  process.
- Rename the project and GitHub repository to OMGCode. Version 0.7.0 uses
  `omgcode-*` Rust crates, the `omgcode` command, `OMGCODE_LM_TOKEN`, the OMGCode
  application-data identity, and `omgcode-` session filenames. Prompt version
  0.1.4 carries the new identity. Old names have no runtime aliases. Existing
  private data stays in place; explicit transition instructions are in the
  development guide. Keep the existing Sonar project key to retain its history.

### Fixed

- Approving a request in the desktop approvals window now completes the call.
  Before, the call ended when it recorded the approval, and only an identical
  call in a later turn could use it, so approving appeared to do nothing
  (ADR-0063).
- The approvals window can review a request while its turn waits for the
  decision. The waiting turn shares the exact call it waits on, and core
  still checks it against the approval digest. Before, any running turn made
  the review `busy`.
- The chat and dispatch paths wait up to 2 seconds for another holder of the
  settings store instead of failing. A turn polling for its approval, another
  turn, or the approvals window could make a call fail with `control journal
  is already open`.

- A run whose access a settings change withdrew kept running if another
  change restored that access before the run's watcher read the settings (it
  polls every 50 ms). This applied to `execute`, `read`, `write`,
  `host_services`, `task_network`, the package cache root, and the
  development root, from the CLI and, in 0.41.0, from the desktop.
  The command that withdrew the access could then report success. A run's
  watcher now checks the state after every settings event recorded since it
  last read them, not only the latest, so the change's own event stops the
  run even after access is restored. Settings commands list the runs they
  withdraw while they still hold the settings lock, through the core
  function `run::withdrawn_runs`, and wait for them after releasing it
  (ADR-0061). Nothing new is written to the settings journal. If the run
  journal cannot be read, the change stays recorded, the watchers still stop
  the runs, and the command fails after waiting for what it can find.
- Every Testing build run on `main` failed in the Ubuntu Rust job:
  `an_abandoned_run_is_pending_until_acknowledged` panicked parsing its start
  marker. `ubuntu-latest` is now Ubuntu 26.04 with systemd 259, and since
  systemd 258 `systemd-run --scope` expands `$` in the command's arguments by
  default. The test's `sh -c '... echo $$ > started.txt ...'` reached the
  shell as `echo $`, so the marker held `$`. Ubuntu 24.04 (systemd 255), used
  by the pull-request workflow and the WSL checks, does not expand scope
  arguments, so it passed there. Forcing `--expand-environment=yes` on
  systemd 255 reproduced the failure with marker `"$\n"`.
  - The CLI test helper and the platform tree-limit re-run now pass
    `--expand-environment=no`, so every argument reaches the CLI literally.
  - New Linux test `dollar_signs_reach_the_program_verbatim` runs
    `printf` with `$$`, `$HOME`, `${HOME}`, and `$` and checks the run's
    stdout. It failed with forced expansion and passes with the fix.
  - The start-marker parse failure now prints the marker text.
  - The operator command lines in the [execution](docs/execution.md#linux-delegated-cgroup)
    and [development](docs/development.md) guides, and the runner's
    "not delegated" error, now include `--expand-environment=no`. Without it,
    on systemd 258 and later, a run starts and journals a different command
    line than the operator typed. The switch needs systemd 254 or later.
- The six cancellation tests in `crates/omgcode-app/tests/chat_turn.rs` now
  time the interval from the moment the token is cancelled until the turn or
  compaction returns, as the desktop cancellation tests already did. They
  timed the whole turn, which also covers reading settings and the synced
  journal appends made before the request. On a slow Windows runner,
  `a_streamed_text_turn_cancelled_mid_stream_is_recorded_as_cancelled`
  failed with "cancellation took 5.9s". In the same job, two `omgcode-app`
  unit tests that only open a service on a temporary directory took 4 to 5
  seconds each, against milliseconds on other runs. Local timing probes put
  the cancel-to-return step under 1 ms, also under full CPU load and under
  disk-flush contention. A 6-second delay before the response head
  reproduced the failure with the old timing and passes with the new one. A
  slowed-down cancellation (a 6-second poll) still fails the new check. The
  5-second bound and the adapter are unchanged.
- Findings from the first Sonar analysis of the desktop frontend:
  - Both workflows now install the frontend's dependencies with
    `npm ci --ignore-scripts`. `ui/.npmrc` already set this; the flag makes
    it hold even if that file changes.
  - Four lines of frontend source and tests held raw bidirectional control
    characters, inside the pattern and the test strings that deal with those
    characters. They are now written as `\u` escapes, which denote the same
    characters, and a frontend test fails if a raw one is added.
  - `sonar-project.properties` declares test code as test code. Every
    duplicated line Sonar reported, and about half the lines it counted as
    code to cover, were in test files analysed as production code.
  - Sonar's condition on coverage of new code failed because no coverage
    report existed for the frontend's TypeScript sources. `npm run coverage`
    now writes one with `@vitest/coverage-v8` 5.1.0, a new development
    dependency pinned to the Vitest version. The Sonar workflow runs it in a
    separate job that never references the Sonar credential and passes only
    the report to the analysis job. Nothing is excluded from the measure. It
    stood at 85.7% of lines and 76.0% of branches on 2026-11-17, about 82%
    by Sonar's combined measure against a required 80%. Rust is still not
    measured for coverage.
- The CLI test `concurrent_runs_complete_and_are_recorded` no longer depends
  on host speed. On Windows each run spun a fixed loop of 30,000 `cd .`
  iterations before writing its file, and the test allowed 60 seconds for
  both. On a host held near 100% CPU by an unrelated build, one such loop took
  22 to 24 seconds in a plain shell, and the test failed three times in a row,
  on this change and on the commit before it. Each run now writes its own file
  and waits until it sees the other run's file. Exit code 0 from both proves
  the runs were alive at the same time, which the old scripts did not: two
  runs made one after the other would also have passed. A run that never sees
  the other's file exits non-zero. The Linux scripts use the same rendezvous
  in place of `sleep 1`. The assertions are unchanged, and no limit was
  raised. No production code changed.
- PR #32 review follow-up:
  - Compaction splits summarizer input at the 128-message prompt-assembly
    limit as well as the byte limit, in history chunks and combination
    rounds. 130 short older messages used to form one chunk that prompt
    assembly refused.
  - Summarizer chunks are sized by each message's JSON-escaped length, so
    content with quotes, backslashes, or control characters no longer makes
    a chunk exceed the serialized request bound and fail compaction.
  - An approved `workspace_edit_file` call is refused with `output_too_large`
    before its approval is consumed when the widest success report cannot
    fit the result limit, so an apply never runs without a reportable result.
  - The chat preflight estimate replays saved tool results only under live
    `model_tools` and `read`, matching the request the turn will send, so a
    withheld result no longer triggers a needless compaction refusal. The
    authority is read again after auto-compaction, so a `read` revoked during
    the summarizer request is reflected in the post-compaction estimate.
  - A Secret Service `CreateItem` reply that carries both an item and a prompt
    is refused with `credential_prompt_required` like a prompt alone; the
    prompt is never run and the placeholder is deleted.
  - Windows credential tests inject a directory-sync failure after the file
    is written, with a successful and with a failed cleanup.
  - `provider-credential-set` reads the secret before it opens the control
    store, so the store's lock is not held while standard input is awaited.
  - A failed credential store that may have left an item, such as a Windows
    directory sync failure after a complete write, deletes it. The Windows
    store now removes its file on that failure too. If cleanup fails, the
    command reports `credential_orphaned` with the identifier.
  - A binding append refused before any write still deletes the new item. A
    binding whose journal write or sync failed may be recorded, so the new
    item is now kept and reported as `credential_binding_uncertain` with its
    identifier and any replaced identifier. A recorded binding no longer
    names an item that OMGCode deleted.
  - Linux no longer lets `secret-service` run a prompt returned by
    `CreateItem` or `Item.Delete`, which waited without a bound. Those calls
    go through zbus directly; a returned prompt is dismissed, never run, and
    reported as `credential_prompt_required`. Mock-service tests prove that
    `Prompt` is never called.
  - Credential tests keep their temporary directory until the stores close,
    and the history-limit CLI tests seed journals inside the spawn gate. The
    credential command example pipes the key on standard input.

- PR #30 review follow-up:
  - Expired, undecided approvals no longer occupy the 64 pending slots during
    requests or replay. Only records unexpired at the request's event time
    count.
  - A legacy toolchain profile that declares `OMGCODE_PACKAGE_CACHE` still
    replays. New declarations are refused; the runner skips a legacy value so
    its own cache path wins.
  - Windows package fetch now requires a selected cache root, matching the
    runner's lack of an application-storage fallback.
  - Package fetch checks its overall deadline before each body read, and the
    send and every chunk read are bounded by the remaining time. A body that
    completes after the deadline is rejected.
  - A stale foreign cache-grant record refuses a cache-root selection, and a
    stale owner grant is revoked fail-closed instead of being treated as
    unrelated.
  - A `model_tools` true-to-false change records a durable per-workspace
    count. A model-origin run stops even if the preference is re-enabled
    before its watcher reads the setting.
  - Disk sampling preserves a real measurement error that races with
    shutdown; only its own cancellation is ignored.
  - Package cache publication uses an atomic no-replace hard link. Two
    concurrent fetches cannot overwrite each other, and a synchronized
    collision test covers the winner.
  - A run records its configured package cache root and whether it came from a
    model tool. Clearing or replacing the root, or disabling `model_tools`,
    withdraws the live run. The cache-root CLI commands wait for affected runs.
  - Changing or clearing a cache root revokes read-execute container grants
    that covered it, and selecting a root another workspace can already read is
    refused.
  - The model command summary finds a UTF-8 boundary before truncation; a long
    multibyte program name no longer panics.
  - Disk sampling observes cancellation during traversal, and directory
    enumeration checks the entry budget before queuing each path.
  - Documents that `workspace_run_command` has no shell mode but may name an
    interpreter under the exact-request approval. Corrects stale plan, guide,
    and AGENTS descriptions of approval records and model tools.
  - Strengthens the ungranted-cache refusal test to prove the program did not
    start, and the cache-root replay test to compare the exact workspace and
    directory identity.
- Tree limits, after the PR #27 review:
  - **Range checks before recording:** the core checks every run limit before
    it records a run, so a direct caller with an out-of-range tree limit gets
    `InvalidLimits` and nothing is recorded.
  - **Nested Windows jobs:** a job nested in the run's job, such as a tool's
    own stricter job, can send its limit messages to the run's port. The
    runner now counts a message only when the run's own job confirms it. Each
    poll takes at most 256 messages. Once the program has exited, the runner
    takes up to 64 batches before it stops the tree, so a limit queued behind
    other messages still decides the result. If messages remain after that,
    the run is recorded as `supervision_failed`, not as a normal exit. A
    nested job's memory message
    can still end a run already within a sixteenth of its own memory limit,
    because Windows does not identify the sending job.
  - **Linux termination:** a failed `cgroup.kill` no longer skips the process
    group kill; both are tried and the first failure is reported.
  - **Cleanup:** the run's cgroup or job limit state is released even when its
    usage cannot be read.
  - **Grant revocation:** an unreadable run journal no longer blocks
    `container-revoke`. The grant is revoked, then the command fails and says
    so.
- Settings changes now confirm that withdrawn runs stopped, as the PR #26
  review asked:
  - **Unconfirmed cleanup:** a pending run whose cleanup was unconfirmed may
    still have processes. A settings change that withdraws it now fails and
    lists it, instead of reporting success.
  - **Acknowledged runs:** a run counts as stopped only when an outcome other
    than `cleanup_unconfirmed` is recorded. A run acknowledged without an
    outcome while the command waited no longer counts.
  - **Windows grants:** revoking any container grant of a workspace now stops
    that workspace's runs, not only revoking its read-write grant. The run's
    watcher compares a per-workspace revocation count, so it cannot miss a
    revocation between two polls. `container-revoke` waits for each run of the
    workspace that started before the revocation.
- Run workflow tests now allow settings commands their full 60-second wait,
  keep grants whose revocation failed so the fallback can retry them, and on
  Linux end the process group of the abandoned run before the test finishes.
- The Windows runner test for escaping descendants now allows up to five
  seconds for a killed grandchild's process object to become signaled. Windows
  can report the job empty before that object is signaled; the instant check
  failed intermittently on loaded CI runners, including `main` at `6b1ebe4`. A
  grandchild that survives still fails the test.
- Stop parallel CLI integration tests from failing with `Busy` on Linux. A child
  spawned while a test held a store open could inherit that store's `flock` until
  `exec`, so the lock outlived the test's close. A test-only gate now keeps child
  spawns and in-process store sections apart. Production locking is unchanged.

- Use a typed checkpoint inspection state while preserving its JSON values.
  Clarify that checkpoint admission cannot detect complete erasure of both
  checkpoint paths. Document persistence helper contracts and the acceptance
  scenarios for checkpoint storage and recovery.

- Harden the core turn/tool runtime: reserve assembled-prompt capacity and
  enforce the selected tool-message limit before recording results, retain
  interim assistant text in follow-up history, preserve original policy
  binding errors, and cover failed-turn call-identifier reuse in replay.

- Keep operational file checks out of the settings journal so file traffic and
  audit-cap refusals cannot exhaust capacity needed for permission changes.
- Add explicit, no-replace operation-journal exports and read-only archive
  inspection; preserve raw bytes and refuse unresolved logs.
- Reject unusable workspace scopes during registration/binding, classify invalid
  target paths as request errors, and report unchecked broker availability honestly.
- Continue Linux listings when entries disappear and distinguish definite mkdir
  permission refusals from uncertain mutations. Add synchronized pinned-read,
  bounded-consumption, archive-fault, CLI recovery, and capacity regressions.
- Clarify recovery prerequisites, export/privacy limits, and credentials in names.

- Reject stale saved workspace paths before reusing registrations, and reject
  moved application storage before interpreting overlap checks.
- Open Unix workspace selections as directories so named pipes cannot hold the
  control-store lock indefinitely; test unsupported creation-time metadata.
- Bound workspace CLI test processes, check the exact policy-audit event count,
  and document registry status and internal policy contracts.

- Reject indeterminate ancestor walks and invalid control-file links before
  accepting workspace configuration or selecting legacy session behavior.

- Reserve terminal journal capacity, recover full legacy active turns with one
  bounded terminal tail, and test write-success/sync-failure handling.
- Preflight encoded request events before starting turns, mark deterministic
  reply-size failures as failed turns, and retain locators on chat errors.
- Return lossless native journal locators and support reopening paths that are
  not Unicode; prevent renaming open Windows journals with delete sharing denied.
- Redact the configured local server token from retained/displayed chat text
  and resumed context, and apply a total deadline to each model request.
- Apply title redaction to direct session creation as well as chat, and verify
  redaction of credentials seeded in historical conversation text.
- Add review regression tests and clarify smoke-check, coverage, and CLI examples.

### Added

- Rust 0.9.0 Phase A2c: add trusted `file apply` on Linux ext4 with an exact
  SHA-256/size precondition and an explicit stop-writers acknowledgement. Preserve
  supported owner/group/mode metadata, verify a synced private stage, persist
  commit intent, then attempt one handle-relative rename. Sync the published
  file and both parents before success. Keep unresolved evidence on failure;
  never retry, restore, or delete it. Windows apply remains unavailable.
  Add native publication, fault/reopen, and CLI tests. ADR-0022 records the
  filesystem and metadata limits. Dependency versions are unchanged.

- Rust 0.8.0 Phase A2b: protected Linux checkpoint preparation through the
  authorized core file broker, exact payload/manifest validation, bounded private
  storage, durable initialization and journal appends, and failure/reopen handling.
  Add `file checkpoints` and `file acknowledge-checkpoint` for existing-only
  recovery. Unresolved or invalid state blocks new broker work without growing
  the old operation log. Preserve unused legacy stores and permission changes.
  Windows refuses checkpoint storage before mutation; apply/restore/deletion
  remain disabled. Add native, public-API, CLI, quota, and injected-failure tests.
  Enable the existing Rustix dependency's safe UID-query feature; no dependency
  versions change. ADR-0021 records the storage boundary and remaining gates.

- Rust 0.6.0 read-only replacement-record validation for library callers:
  strict initialization/ledger replay, bounded records and descriptors, native
  path validation, exact manifest references, ordered lifecycle transitions,
  unresolved-state retention, and acknowledgement without replay or success
  synthesis. Add public-API boundary tests and an executable documentation example.
  No filesystem writer, checkpoint store, CLI apply, or broker-admission change
  is enabled. ADR-0019 records the remaining implementation increments.

- Record merged PR #10 as acceptance of ADR-0018 and update the resume baseline.

- Phase A1 replacement/checkpoint/recovery proposal and ADR-0018. Define exact
  preconditions, private same-filesystem staging, bounded checkpoint retention,
  versioned audit, uncertain-outcome handling, and native backend/test gates.
  Update the plan for merged PR #9 and split contract review from Phase A2
  implementation. This documentation enables no overwrite or recovery command.
  Define first-use ledger provisioning and the limit on detecting reverted edits.

- Root implementation plan recording the verified baseline, phase order,
  acceptance criteria, verification commands, and known cautions for resuming.

- Rust 0.5.0 read-only edit preview: `file diff` compares a broker-read
  workspace file with trusted proposed UTF-8 content, returns exact digests and
  a bounded single-hunk diff, and never writes to the workspace or creates a
  checkpoint. Existing-file replacement remains disabled.

- Rust 0.4.0 session-owned turn/tool runtime for library callers and tests:
  strict read/list tool admission, a fresh workspace binding per operation,
  control-store release before model I/O, round/call/byte/deadline limits,
  cooperative cancellation, model-addressable Unicode listing names,
  source-labelled untrusted tool messages, and durable tool intents/results.
  No CLI command or real provider adapter dispatches these tools.
- JSONL session schema version 2 with `tool.requested`, `tool.completed`, tool
  messages, and exact tool schema/name manifest fields. Replay verifies that
  each tool message matches its recorded size and SHA-256. Version 1 journals
  remain readable and writable with their text-only subset. New sessions use
  version 2; mixed versions and version-1 tool events are rejected.

- User-provided pull-request-review skill source package, preserved instructions,
  provenance/digest catalog, and documentation links. Runtime skill loading and
  enablement remain planned.

- OrangeCode application namespace across Rust crates, CLI, token environment
  variable, default data directory, session filenames, repository links, and
  Sonar project configuration. Existing private data is not moved automatically.
- Compiled prompt version 0.1.3 with the renamed identity and an updated digest
  regression check; explicit prototype data-transition instructions.

- Rust 0.3.0 native workspace file broker with bounded reads/listings, exclusive
  new-file/directory creation, current binding checks, and handle-relative
  Linux/Windows enforcement. Existing entries cannot be replaced or deleted.
- Separate bounded operation intent/outcome journal, read withholding on audit
  failure, explicit uncertainty acknowledgement, and no action replay.
- Trusted file CLI commands, native traversal/link/mount and collision tests,
  audit failure/recovery coverage, and an ADR for native file-access boundaries.
  Model tool dispatch and command execution remain disabled. Windows directory
  flag compatibility is an explicit release qualification requirement.

- Rust 0.2.0 workspace settings core: explicit development root, persistent
  directory-backed workspace IDs, permission preferences, revision checks, and
  short-lived process-local session bindings. No file or command tools enabled.
- Bounded single-writer settings/audit JSONL with strict replay, synced
  acknowledgement, poisoned-writer handling, and configuration-only decisions.
- Workspace CLI commands and stable IDs in new sessions when configured, with
  legacy text-only session compatibility and Windows/Linux boundary tests.

- First Rust 0.1.0 CLI prototype, with a pinned toolchain, locked dependencies,
  and native Windows/Linux formatting, lint, and test workflow.
- Compiled base prompt inspection, lower-priority per-request user instructions,
  bounded LM Studio chat, and refusal of all model-requested tools.
- Typed JSONL session creation, request manifests, exclusive writer ownership,
  strict replay validation, archive/restore, and explicit abandoned-turn recovery.
- Native journal-handle checks, prompt/provider/journal/CLI tests, a development
  guide, and an ADR separating this prototype from future controlled execution.
- User-selected project/global memories with review, edit, delete, disable,
  scoped retrieval, provenance, and context/cache invalidation requirements.
- A memory-scope ADR and memory guidance in draft system prompt version 0.1.2.
- A compiled, read-only base prompt contract with separate lower-priority user
  customization and current-task input; draft prompt version 0.1.1.
- MCP client requirements for stdio and Streamable HTTP, scoped server/tool
  permissions, authentication, untrusted context, and supervised execution.
- Session archive/restore and versioned JSONL journals in a UTC date tree, with
  stable filenames, a rebuildable index, crash recovery, and no action replay.
- LM Studio as a separate model connection with explicit endpoint, model,
  authentication, and capability qualification.
- MCP/session acceptance requirements and supporting architecture decisions.
- Product priorities for secure operation, prompt injection defence, and efficiency.
- A visible, versioned SYSTEM prompt draft and scoped custom-instruction design.
- Configurable token-based auto-compaction, safe context budgets, provenance,
  bounded summaries, and recovery requirements.
- Scoped artifact and provider prompt caching, invalidation, clear controls,
  and usage measurements without replaying side effects.
- Skills settings for import, removal, enablement, and disablement, with validated
  packages and the existing controlled execution boundary.
- Acceptance requirements and an ADR for instruction/context trust boundaries.
- Initial project overview, provider plans, and Windows/Linux development roadmap.
- Rust coding rules adapted from Merovingian, with security and testing guidance.
- Required contributor guidance, a documentation index, and platform targets.
- An architecture decision register that records the Rust rule adaptations.
- Required ChatGPT and Claude subscription authentication profiles, with provider
  evidence, explicit billing modes, credential lifecycle, and readiness gates.
- General development-tool access through installed or managed toolchain
  profiles, including scoped runtimes, SDKs, outputs, caches, and subprocesses.
- Authentication and toolchain acceptance requirements and supporting ADRs.
- CodeRabbit automatic reviews using project guidelines, CodeQL language-aware
  analysis, and SonarQube Cloud CI configuration.
- PR-only main protection with no bypass actors and blocks on force-pushes
  and branch deletion.
- Repository-wide CodeRabbit guideline scopes, vendor-aware CodeQL selection,
  and precise analysis coverage and Sonar credential setup guidance.
