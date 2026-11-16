# 0045: Build the desktop shell on Tauri with a narrow command surface

- Status: Accepted
- Date: 2026-11-16

## Context

The project overview has always proposed a desktop interface built on Tauri
with a bundled TypeScript web interface. Phase E builds it. The user selected a
session browser with chat and in-application approvals, and vanilla strict
TypeScript with Vite.

These constraints shape the shell:

- **The Rust side stays the authority.** The security rules treat frontend
  messages as untrusted input, require core to authorize every IPC request, and
  forbid loading remote content into a webview with privileged IPC.
- **The UI must not receive journal paths, credentials, or unrestricted host
  capabilities.**
- **Chat lived in the CLI binary.** About 600 lines of chat orchestration
  (context preflight, secret redaction, compaction, session creation) were in
  `omgcode-cli`, which has no library target, and were tied to its argument and
  error types. All model tools and approval logic were already in core.
- **Frontend packages are supply-chain inputs.** The coding rules require
  strict TypeScript, a committed lockfile, and documented lint rules.

## Decision

1. **Three layers.**
   - `omgcode-app` is a new front-end-neutral library. It holds the chat turn,
     session creation, compaction, the data-root resolver, token redaction, and
     the desktop service. The trusted CLI and the desktop shell both call it,
     so they run the same turn. It grants no authority; every authorization
     decision stays in `omgcode-core`.
   - `omgcode-desktop` is a thin Tauri adapter: windows, capabilities, and one
     handler per command that checks the caller and calls the service.
   - `crates/omgcode-desktop/ui` is the TypeScript frontend.
2. **The CLI is unchanged in behaviour.** The extraction is a refactor. Every
   existing CLI test passes without modification, and its output, error codes,
   and exit codes are the same.
3. **Tauri 2, stable line.** `tauri` 2.12.6 and `tauri-build` 2.7.5 with
   `default-features = false` and the `wry` and `x11` features only. No Tauri
   plugin, tray, updater, asset protocol, or devtools feature is enabled.
   Bundling is disabled; packaging belongs to a later phase.
4. **A closed command list.** The shell exposes 18 application commands and
   nothing else. One file lists their names; the build script, the Rust window
   policy, and the tests all read that file.
   - The build script declares every command, so each needs an explicit
     permission in a capability file. No `core:` permission is granted to
     either window.
   - Each handler first checks the calling webview's label against the Rust
     policy table. Tests compare that table with the capability files, the
     build list, and the command names the frontend calls.
5. **Sessions and workspaces are addressed by identifier.** Commands take a
   session UUID or a registered workspace identifier. No command accepts a host
   path. A new session can be created only in a workspace the operator has
   already registered with the trusted CLI.
6. **The chat endpoint is fixed at launch.** The webview cannot choose the
   provider endpoint. The service refuses a chat request whose endpoint differs
   from the one selected when the process started. The local server token is
   read from the environment for each turn and never reaches the webview.
7. **No push channel.** The frontend polls for chat status and pending
   approvals. This removes the event permission from both windows.
8. **Content rules.**
   - The content security policy is `default-src 'none'` with `'self'` scripts,
     styles, images, and fonts, and IPC as the only connection target. Tauri
     appends hashes for its own bootstrap scripts.
   - The global Tauri object is disabled and the object prototype is frozen.
   - A navigation guard allows only the application's own origin, plus the
     local development server in debug builds. New-window requests are denied.
   - The frontend builds its document only from elements and text nodes. Tests
     scan the source for markup-injecting interfaces, inline script or style,
     remote resources, and browser storage. Bidirectional override characters
     are shown as visible markers.
9. **Frontend stack.** Strict TypeScript with no framework, Vite, Vitest with
   jsdom, and Biome for both formatting and linting. `@tauri-apps/api` is the
   only runtime dependency and is imported in one module. Versions are pinned
   exactly, the lockfile is committed, and install scripts are disabled.
10. **Blocking work stays off the interface thread.** A Tauri command without
    `async` runs on the main thread. Every handler that touches a journal, the
    index, the settings store, or the provider is asynchronous and hands the
    work to a blocking task. Each chat turn runs on its own supervised thread.

## Alternatives considered

- **Give `omgcode-cli` a library target and depend on it from the shell.**
  Rejected: the desktop would link argument parsing and printing, and the CLI's
  error type would become a shared contract.
- **Move chat orchestration into `omgcode-core`.** Rejected: core owns
  contracts and authorization. Reading the environment, resolving the user's
  data directory, and front-end output shapes are application concerns.
- **Run the CLI as a child process from the shell.** Rejected: it would add a
  process boundary to parse and a second place for errors to be reinterpreted.
- **A native Rust interface toolkit without a webview.** Rejected for now: the
  project's documented architecture is Tauri, and transcript rendering,
  search, and review layouts are faster to build and review as web content. It
  removes the webview risk, so it stays a possible later change.
- **React or Svelte.** Rejected by the user in favour of no framework, which
  keeps the reviewed dependency set small.
- **ESLint and Prettier.** Rejected: Biome covers both with one package.
- **Letting the webview choose the provider endpoint.** Rejected: injected
  script could aim the provider client at any loopback port.
- **A folder picker for new sessions.** Rejected: it would let the webview
  name a host path and would need a dialog plugin.
- **Tauri events for progress.** Deferred: polling needs no extra permission
  and the provider adapter does not stream.

## Consequences

- The workspace gains two crates and a Node toolchain. The lockfile grows by
  about 280 packages for the webview stack, most of them platform-specific.
- The Rust build of the shell embeds the built frontend, so the frontend must
  be built before the workspace is compiled or linted. Linux builds need the
  WebKitGTK development packages.
- The embedded interface is used only when the crate's `custom-protocol`
  feature is enabled. It is a default feature, so a plain `cargo build`
  embeds the interface, and `tauri dev` drops it to reach the development
  server. Without it the shell loads the development server address, which a
  release build's navigation guard refuses.
- A compromised `main` webview can use every `main` command: read transcripts,
  search, create sessions in registered workspaces, send chat turns, archive,
  restore, and interrupt. It cannot name a host path, choose the endpoint,
  read the token, or decide an approval (ADR-0046).
- The capability layer is enforced by Tauri. The Rust label check and the
  agreement tests do not need a webview; the capability files do. On Windows
  the built shell was driven over the WebView2 debugging port to confirm that
  `main` is refused `approval_decide` and that remote fetches, navigation,
  popups, and string evaluation are blocked. On Linux the shell compiled and
  its tests passed under WSL 6.6, but no window has been opened there.
- The default WebView2 context menu remains enabled. Developer tools are not
  available in a release build.
- Polling adds a small constant IPC load while a window is visible.
- A turn that is cut off when the application exits leaves an open turn in its
  journal. The tree shows it and the operator can interrupt it.

## References

- [ADR-0010](0010-build-a-local-chat-core-before-execution.md)
- [ADR-0044](0044-observe-journals-without-the-writer-lock-and-index-them-in-sqlite.md)
- [ADR-0046](0046-decide-model-tool-approvals-in-an-isolated-desktop-window.md)
- [Security coding rules](../security-coding-rules.md#frontend-persistence-and-dependencies)
- [Tauri capabilities](https://tauri.app/security/capabilities/)
- [Tauri content security policy](https://tauri.app/security/csp/)
- [Tauri: calling Rust from the frontend](https://tauri.app/develop/calling-rust/)
