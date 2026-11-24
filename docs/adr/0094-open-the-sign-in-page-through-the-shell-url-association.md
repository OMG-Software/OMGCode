# 0094: Open the sign-in page through the shell URL association

- Status: Accepted
- Date: 2026-11-24

## Context

ADR-0093 opens OpenAI's authorization page by running `explorer.exe` from the
Windows directory with the address as its one argument. An operator reported
that **Sign in with ChatGPT** opened a File Explorer window instead of a
browser.

The authorization address carries PKCE, `state`, `nonce`, a loopback
`redirect_uri`, scopes, a resource, and a host identifier, so it is several
hundred bytes long. Microsoft does not document `explorer.exe` as a way to open
a URL; its command line is parsed as a folder or a set of switches. Our
inference, not verified by reproduction, is that Explorer did not accept the
long address as a URL and fell back to opening a folder window. Either way the
route depended on undocumented behaviour.

Microsoft documents `ShellExecuteEx` as the way to open a URL with the
handler registered for its scheme. With no verb it uses the handler's default
verb, then `open`, then the first registered verb. It may activate shell
extensions through COM, so the calling thread should initialize COM first. With
`SEE_MASK_NOASYNC` it finishes the hand-off before it returns.

## Decision

- A new `omgcode-platform::browser` module owns opening a browser.
  `open_https_address` accepts only an `https://` address of at most 8 KiB made
  of RFC 3986 URI characters. It refuses spaces, quotes, backslashes, control
  bytes, and non-ASCII before any launch.
- On Windows it calls `ShellExecuteExW` with no verb, so the handler's
  default verb is used and a handler without an `open` verb still works,
  `SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI`, and `SW_SHOWNORMAL`. The call runs
  on a new thread that initializes a single-threaded COM apartment with OLE1
  DDE disabled and uninitializes it afterwards, so the caller's apartment is
  never changed. No shell, `cmd.exe`, or `explorer.exe` command line is used.
- The caller waits at most five seconds (`HAND_OFF_WAIT`) for that thread's
  result, and checks a caller-supplied stop condition every 50 ms.
  `NOASYNC` makes the shell call wait for the URL handler, and an unbounded
  wait would let a hung handler hold the sign-in, its slot, and the settings
  change guard. After the bound, or once the stop condition holds, the
  function returns `Handoff::Pending` and leaves the thread to finish alone;
  it holds only its copy of the address. `omgcode-app` passes the sign-in's
  cancel token as the stop condition, treats `Pending` as "the browser may
  still open", and goes on to the callback wait, which reports a cancel at
  once.
- Only one hand-off thread runs per process. While a hung handler still holds
  it, a new hand-off is refused as `BrowserError::Busy`, which the sign-in
  reports as `browser_unavailable`, so retries cannot pile up blocked
  threads.
- The sign-in's five-minute deadline starts before the browser step, and the
  callback wait gets only what is left, so a slow hand-off cannot stretch
  the sign-in past five minutes.
- On Linux and other Unix systems it runs `/usr/bin/xdg-open` with the address
  as one argument, as before.
- `omgcode-app` keeps its check that the address is OpenAI's authorization
  page and now calls the platform module. It depends on `omgcode-platform`
  directly; the unsafe Windows call stays in the platform crate, and
  `omgcode-app` still forbids unsafe code.

## Alternatives considered

- Keep `explorer.exe` and quote the address: rejected. It still relies on
  Explorer's undocumented command-line parsing.
- `cmd /c start "" <address>`: rejected. It starts a command shell and makes
  `&` in the query significant to it.
- `rundll32 url.dll,FileProtocolHandler <address>`: rejected. It is an
  undocumented entry point reached through a general-purpose loader.
- A browser-opening crate: rejected. It would add a dependency for one call
  the platform crate can make with `windows-sys`, which it already uses.

## Consequences

- On Windows the sign-in page opens in the browser registered for `https`.
  When no handler is registered, or the shell reports failure, the CLI prints
  the address and says the browser could not be opened. The desktop sign-in
  reports `browser_unavailable` as before.
- `omgcode-platform` enables the `windows-sys` features `Win32_UI_Shell`,
  `Win32_UI_WindowsAndMessaging`, and `Win32_System_Registry`, which
  `SHELLEXECUTEINFOW` requires.
- A URL handler that hangs leaves one detached thread for the process until
  the shell call returns, and later sign-ins fail as `browser_unavailable`
  until it does. A cancel ends the sign-in within about 50 ms, and the
  deadline holds at five minutes from the start of the sign-in.
- Tests cover address validation; the bounded wait (done, failed, panicked,
  still running, and stopped); the one-thread claim; a sign-in whose browser
  step outlasts the wait; and a desktop sign-in cancelled during a stalled
  browser step, which ends `cancelled` and releases its slot. A core test of
  a cancel during the browser step is kept as a regression guard; it passed
  before this change too. A test that opens a browser on the developer's
  desktop is not practical, so the launch itself was not exercised by an
  automated test, and the original failure was not reproduced.

## References

- [ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)
- [ShellExecuteExW](https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw)
- [SHELLEXECUTEINFOW](https://learn.microsoft.com/windows/win32/api/shellapi/ns-shellapi-shellexecuteinfow)
- [Provider authentication](../provider-authentication.md#chatgpt-sign-in-adr-0093)
