//! Whether the operator is working in this process's windows (ADR-0105).
//!
//! The desktop shell asks this before it brings a window to the front. On
//! Windows, taking the foreground from another application needs a workaround
//! that sends that application a simulated key press, so the shell does that
//! only when this process already owns the foreground window. The answer is
//! advisory and can change at any moment; it authorizes nothing.

/// Whether the foreground window belongs to this process.
///
/// On Windows, `Some(true)` when the window the operator is working in is
/// owned by this process, `Some(false)` when another process owns it or no
/// window is in the foreground. Elsewhere `None`: the caller decides with its
/// own toolkit's focus state.
///
/// Two Win32 calls with no safe wrapper in `windows-sys`; each `unsafe` block
/// states its obligations.
#[cfg(windows)]
#[allow(unsafe_code)]
#[must_use]
pub fn this_process_owns_foreground() -> Option<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    // SAFETY: `GetForegroundWindow` takes no arguments and only returns a
    // window handle value, or null when no window is in the foreground.
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return Some(false);
    }
    let mut process = 0_u32;
    // SAFETY: `window` is a handle value the system just returned. If that
    // window has since been destroyed, the call fails and returns 0, which is
    // handled below; the handle is never dereferenced here. `process` is a
    // live, writable `u32` for the whole call.
    let thread = unsafe { GetWindowThreadProcessId(window, &raw mut process) };
    Some(thread != 0 && process == std::process::id())
}

/// Whether the foreground window belongs to this process. Not known outside
/// Windows.
#[cfg(not(windows))]
#[must_use]
pub const fn this_process_owns_foreground() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_known_only_on_windows() {
        // Which window is in the foreground depends on the test host, so only
        // whether there is an answer is checked.
        assert_eq!(this_process_owns_foreground().is_some(), cfg!(windows));
    }
}
