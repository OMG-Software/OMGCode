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

Microsoft documents `ShellExecuteEx` with the `open` verb as the way to open a
URL with the handler registered for its scheme. It may activate shell
extensions through COM, so the calling thread should initialize COM first. With
`SEE_MASK_NOASYNC` it finishes the hand-off before it returns.

## Decision

- A new `omgcode-platform::browser` module owns opening a browser.
  `open_https_address` accepts only an `https://` address of at most 8 KiB made
  of RFC 3986 URI characters. It refuses spaces, quotes, backslashes, control
  bytes, and non-ASCII before any launch.
- On Windows it calls `ShellExecuteExW` with the `open` verb,
  `SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI`, and `SW_SHOWNORMAL`. The call runs
  on a new thread that initializes a single-threaded COM apartment with OLE1
  DDE disabled and uninitializes it afterwards, so the caller's apartment is
  never changed. No shell, `cmd.exe`, or `explorer.exe` command line is used.
- The caller waits at most five seconds (`HAND_OFF_WAIT`) for that thread's
  result. `NOASYNC` makes the shell call wait for the URL handler, and the
  sign-in calls the opener before its callback wait, which is what observes
  cancellation and the five-minute deadline. An unbounded wait would let a hung
  handler hold the sign-in, its slot, and the settings change guard. After the
  bound the function returns `Handoff::Pending` and leaves the thread to
  finish alone; it holds only its copy of the address. `omgcode-app` treats
  `Pending` as "the browser may still open" and goes on to the callback wait.
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
- A URL handler that hangs for longer than five seconds leaves one detached
  thread until the shell call returns. A cancel or the deadline still ends the
  sign-in, at most five seconds later than the hand-off began.
- Tests cover address validation and the bounded wait (done, failed, panicked,
  and still running). A test that opens a browser on the developer's desktop
  is not practical, so the launch itself was not exercised by an automated
  test, and the original failure was not reproduced.

## References

- [ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)
- [ShellExecuteExW](https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw)
- [SHELLEXECUTEINFOW](https://learn.microsoft.com/windows/win32/api/shellapi/ns-shellapi-shellexecuteinfow)
- [Provider authentication](../provider-authentication.md#chatgpt-sign-in-adr-0093)
