# OMGCode desktop UI

Webview frontend for the Tauri 2 desktop app. The Rust core is the only authority.
This code renders model- and repository-controlled text and must treat it as hostile.

Vanilla strict TypeScript, Vite (multi-page), Vitest with jsdom, and Biome for both
formatting and linting. The only runtime dependency is `@tauri-apps/api`. Dependencies are
pinned exactly, `package-lock.json` is committed, and `.npmrc` sets `ignore-scripts=true`.

## Scripts

| Script              | Purpose                                                                               |
| ------------------- | ------------------------------------------------------------------------------------- |
| `npm run dev`       | Vite dev server on port 1420 (strict port)                                            |
| `npm run build`     | `tsc --noEmit`, then `vite build` into `dist/`                                        |
| `npm run check`     | Strict type check                                                                     |
| `npm run lint`      | `biome check .` (lint, format check, import order)                                    |
| `npm run format`    | `biome format --write .`                                                              |
| `npm test`          | Vitest. The `dist/` checks run only if `dist/` exists; otherwise they show as skipped |
| `npm run test:dist` | Build, then run `tests/dist.test.ts`                                                  |
| `npm run tauri`     | Tauri CLI, run from `crates/omgcode-desktop` (`cd .. && tauri`)                       |

## Running the desktop app

Run this from `crates/omgcode-desktop/ui` (the script changes to the parent directory,
where `tauri.conf.json` lives, so it works in PowerShell, Git Bash, and Linux shells):

```
npm run tauri -- dev -- -- --data-dir <an empty scratch directory>
```

- **Always pass `--data-dir`.** Without it the app uses the real per-user data directory,
  including your real sessions and approvals. Use a fresh directory for development.
- The first `--` ends `npm` options, the second ends the Tauri CLI options, and the rest go to
  the app. `tauri dev` starts Vite on port 1420 through `beforeDevCommand` (`cwd: ui`) and
  builds the Rust shell without the `custom-protocol` feature.
- `npm run tauri -- --version` and `npm run tauri -- info` also work from here.
- `ui/dist` must exist before any `cargo check`, `clippy`, `build`, or `test` that includes
  the desktop crate, because the Tauri build embeds it. Run `npm run build` first.

## Windows

- `index.html` is the main window: session tree, search, transcript, composer.
- `approvals.html` is the approvals window. Only this entry imports
  `src/ipc/approvals_client.ts`, so the main bundle never contains `approval_decide` or
  `approval_detail`. It renders no transcript text.

## Rules

- Untrusted text is rendered only as text nodes through `src/dom.ts`. Bidirectional override
  characters are replaced by visible `[U+XXXX]` markers. No `innerHTML`, `eval`, string timers,
  computed dynamic imports, `window.open`, links, or web storage. A test scans `src/`.
- No inline script or style, and no remote resources. The build must run under
  `default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self';
  connect-src ipc: http://ipc.localhost; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`.
- All IPC goes through `src/ipc/client.ts`, the only file that imports `@tauri-apps/api`.
  Core error messages are shown as text. Client-side limits are usability only; the core
  validates everything.
- The model endpoint is fixed when the desktop process starts. The composer shows the value
  from `chat_defaults` as read-only text and sends back exactly that value.
- The main window subscribes once with `chat_subscribe`, a Tauri channel. It reads
  `chat_status` once when a `finished` event arrives and keeps a five-second reconcile check
  while a turn runs, in case an event is lost. If subscribing fails, it polls `chat_status`
  every 750 ms instead. The command name lives in `src/ipc/chat_events.ts`, which only the main
  window imports, so the approvals bundle never contains it. Streamed text is provisional
  and is rendered as text. A cancelled turn shows `Turn cancelled.` and is recognised by the
  `cancelled` error code, never by message text. Pollers (`src/poll.ts`) never overlap calls and pause while the
  page is hidden; the approvals window still polls.

## Mock backend

When `import.meta.env.DEV` is true and Tauri is absent, `client.ts` loads `src/mock/` with a
literal dynamic `import()`. Production builds drop that branch. The mock holds 12 sessions in
all five statuses, a 255-entry transcript, hostile strings (markup, `</script>`, a right-to-left
override, a 5,000-character token, a `javascript:` URL), a chat turn that completes after 2 s
(model `fail` fails; cancelling ends it at once as `cancelled`), and three approvals (two exact, one stale). `OMGCODE_MOCK_FIXTURE_MARKER`
must never appear in `dist/`; `tests/dist.test.ts` checks that. A production build with no
Tauri shows "This page must run inside OMGCode."
