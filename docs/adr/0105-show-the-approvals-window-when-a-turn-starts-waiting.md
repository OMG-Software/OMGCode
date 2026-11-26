# 0105: Show the approvals window when a turn starts waiting

- Status: Accepted
- Date: 2026-11-26

## Context

A desktop chat turn whose tool call needs an approval waits in the turn for
the operator's decision (ADR-0063). Until now the operator learned about it
from the `main` window's advisory count, which polls `approvals_summary`
every 3 seconds, and had to open the hidden `approvals` window with
**Review** or the sidebar row (`approvals_show_window`). An operator asked
for the approvals window to open and come to the front by itself when an
approval is requested, and for the `settings` window to come to the front
when its button opens it.

Bringing a window to the front is not free on Windows. When another
application owns the foreground, `SetForegroundWindow` is refused. tao, the
windowing library Tauri uses, then sends a simulated Alt key press to steal
the foreground permission (`force_window_active` in tao 0.37.2). That key
press reaches the application the operator is working in. The operator
chose that OMGCode must not take focus from another application: it shows
the approvals window and flashes its taskbar button instead.

A window's own focus flag cannot answer "is the operator in OMGCode?" on
Windows: tao reports a window focused only while its top-level window holds
the keyboard focus, which the WebView2 child normally holds instead.

The approvals window already guards against a window that appears under the
operator's hands: **Approve** is never the default focus, no key approves or
denies, and **Approve** is armed only one second after a review renders.

## Decision

1. **Core tells an observer when a wait begins.** `ApprovalWaitSlot` can
   carry a `WaitObserver`, a `Fn()` that the turn calls on its own thread
   each time it publishes the call it waits on, with no lock of the slot
   held, before the wait starts. The observer receives nothing and decides
   nothing. A slot without one behaves as before.
2. **The desktop service holds one observer.**
   `DesktopService::set_approval_wait_observer` replaces it. Each turn
   copies it when it starts; a running turn keeps its copy until its thread
   ends. The turn table, which keeps finished slots, holds
   `ApprovalWaitSlot::reader`, which shares the awaited call but not the
   observer. `DesktopService::shutdown` drops the service's copy, so once
   the turn threads have ended nothing holds the observer, which ends the
   reference cycle a shell observer forms through the application handle.
3. **The shell shows the approvals window from Rust.** At startup the shell
   registers an observer that calls `present_approvals`, which posts one
   closure to the event loop. There, where window calls take effect at once,
   the shell checks the foreground and acts, so only a few window calls on
   one thread separate the check from the activation, not a queued message.
   Another application can still take the foreground in that gap, so the
   activation checks ownership again (below) rather than relying on the
   callback to make the check and the activation atomic. When this process
   owns the foreground window, the approvals window is unminimized, shown,
   and activated. Otherwise it is shown and its taskbar button flashes
   until the operator comes to it (`UserAttentionType::Critical`), and
   nothing takes the foreground. On Windows, ownership of the foreground
   window is read with `GetForegroundWindow` and `GetWindowThreadProcessId`
   in `omgcode-platform` (`foreground::this_process_owns_foreground`), and
   the window is activated with `SetForegroundWindow` alone
   (`foreground::activate_if_foreground_owner`), never with tao's
   `set_focus`, whose fallback sends the simulated key press. If Windows
   refuses, or another application took the foreground since the check, the
   window flashes instead. On Linux, where the prototype has not run, any
   OMGCode window that reports focus counts, and the toolkit's focus
   request is used.
4. **Nothing reaches a webview.** No command, event, or capability is added.
   The `main` window cannot open or focus the approvals window in any new
   way, and the approvals window decides exactly as before.
5. **The approvals window lists again when it gains focus.** Its list poll
   runs every 3 seconds and pauses while the window is hidden, so a hidden
   window lists at once when shown, but a window that was already open
   could show the new request up to 3 seconds after coming forward. A
   `focus` listener on the page now runs one poll tick, which is skipped if
   a list call is in flight. The tick changes only the list: a pending
   request under review stays selected, as for any poll.
6. **Settings is unchanged.** `settings_show_window` and
   `settings_show_workspace` already unminimize, show, and focus the window,
   and now share the same helper as `approvals_show_window`. That helper
   keeps tao's `set_focus`, because it runs on the operator's own click in
   OMGCode, which leaves this process in the foreground. The operator
   saw the window open in front every time in their latest testing and had
   seen it open behind `main` only occasionally before. No cause was found
   in the code, so no speculative change was made.

## Alternatives considered

- **Open the window from the `main` window's 3-second poll.** It needs no
  core change, but it adds up to 3 seconds and depends on the `main`
  webview's timers, which the browser engine slows while the window is
  minimized.
- **Push a chat event and let `main` call `approvals_show_window`.** The
  chat event sink exists only while `main` is subscribed, and a webview
  round trip adds nothing that Rust cannot do itself.
- **Always take the foreground.** Rejected by the operator: on Windows it
  sends a simulated key press to another application.
- **Use the window focus flags on Windows.** They read as unfocused while a
  webview holds the keyboard focus, so the window would only flash while the
  operator types in OMGCode.
- **Emit an event to the approvals window to list now.** It would reach the
  window even when Windows delivers no focus event, but it adds an event and
  a capability to the isolated window; a focus tick needs neither.
- **Poll the approvals list faster.** It would cut the delay for every
  request, at the cost of more store reads while nothing changes.
- **Poll the turn table from a shell thread.** It needs a supervised thread
  and still reacts late; the observer runs exactly when the wait begins.
- **Check the foreground on the turn's thread and queue the focus.** Review
  found that the operator could switch applications before the queued
  `set_focus` ran, which would then send the simulated key press to the
  other application.
- **Clear the observer from each slot when its turn finishes.** It works,
  but every path that ends a turn would have to remember to do it; a reader
  that never held the observer needs no cleanup.

## Consequences

- A desktop turn that starts waiting shows the approvals window at once. An
  approval recorded by a CLI turn still shows only in the count.
- Showing the window selects and reviews the soonest-expiring pending
  record, as opening it always has. For a file-write approval, that review
  makes its audited broker read, now without a click.
- The observer runs before the wait starts, so a slow observer would delay
  the wait; the shell's observer only posts one closure to the event loop.
- Showing a hidden window still goes through tao's `show`, which calls
  `ShowWindow` with `SW_SHOW`, and unminimizing calls it with
  `SW_RESTORE`; both flags request activation. When another application
  owns the foreground, Windows' foreground lock refuses that activation
  and flashes the taskbar button instead, and no input is sent. A
  non-activating show is not available through tao once a window exists:
  its `MARKER_DONT_FOCUS` flag applies only at creation. A direct
  `ShowWindow` with `SW_SHOWNOACTIVATE` would leave tao's visibility flag
  clear, and tao skips a change that matches its flag, so a later `hide`,
  such as closing the window, would do nothing. Where the shown window
  lands in the stacking order while another application owns the
  foreground was not checked on a Windows desktop.
- `omgcode-desktop` now depends on the workspace crate `omgcode-platform`.
  `omgcode-platform` gains two Windows-only functions with four `unsafe`
  calls; no third-party code is added.
- Every time the operator switches to the approvals window it makes one
  extra `approvals_list` call, which reads the approval store as each poll
  does.
- Tests cover the slot's observer and its reader, that a real desktop turn
  tells the observer once and still waits for a decision, that shutdown
  drops the observer with and without a finished turn in the table, that
  activation refuses a handle that is not this process's window, and that
  a focus event lists a new request without moving the review. No test
  activates a real window. Which window ends up in front, the taskbar
  flash, and whether WebView2 delivers the page's `focus` event when the
  window is activated depend on the Windows desktop and were not driven by
  automation.

## References

- [docs/desktop-interface.md](../desktop-interface.md#approvals)
- [ADR-0063](0063-deny-prompt-or-allow-each-preference-wait-for-approvals-in-the-turn-and-journal-model-actions.md),
  [ADR-0059](0059-configure-trusted-settings-in-an-isolated-desktop-settings-window.md)
- Microsoft, [`SetForegroundWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow)
  and [`GetWindowThreadProcessId`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowthreadprocessid)
