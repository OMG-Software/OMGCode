//! Whether the operator is working in this process's windows, and bringing
//! one of them forward without simulated input (ADR-0105).
//!
//! The desktop shell asks this before it brings a window to the front on its
//! own. On Windows, taking the foreground from another application needs a
//! workaround that sends that application a simulated key press, so the shell
//! brings a window forward only when this process already owns the foreground
//! window, and then with [`activate_if_foreground_owner`], which sends no
//! input. The answer is advisory and can change at any moment; it authorizes
//! nothing.

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

/// Activate `window` if this process owns the foreground window now, with
/// `SetForegroundWindow` alone and never simulated input.
///
/// `window` is the value of a top-level window's handle, such as the address
/// inside a toolkit's `HWND`. A handle is an opaque value, not memory, so it
/// is taken as an integer. It must belong to this process; a handle of
/// another process, a destroyed window, or 0 is left alone. Returns `true` only
/// when Windows activated the window. `false` means another process owns the
/// foreground, the handle is not this process's, or Windows refused, and the
/// caller should ask for attention instead.
///
/// Windows only: elsewhere the toolkit's own focus request sends no input.
#[cfg(windows)]
#[allow(unsafe_code)]
#[must_use]
pub fn activate_if_foreground_owner(window: usize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, SetForegroundWindow,
    };

    if window == 0 || this_process_owns_foreground() != Some(true) {
        return false;
    }
    let window = std::ptr::without_provenance_mut(window);
    let mut process = 0_u32;
    // SAFETY: `GetWindowThreadProcessId` only reads the handle value; an
    // invalid or destroyed handle makes it fail and return 0, handled below.
    // `process` is a live, writable `u32` for the whole call.
    let thread = unsafe { GetWindowThreadProcessId(window, &raw mut process) };
    if thread == 0 || process != std::process::id() {
        return false;
    }
    // SAFETY: `SetForegroundWindow` takes a handle value and reports failure
    // for one that is not a window; it sends no input. The handle was just
    // checked to name a window of this process.
    unsafe { SetForegroundWindow(window) != 0 }
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

    #[cfg(windows)]
    #[test]
    fn a_handle_that_is_not_this_processs_window_is_never_activated() {
        // Zero and a value no window of this process has are refused before
        // any activation.
        // No test activates a real window, which would take the host's focus.
        assert!(!activate_if_foreground_owner(0));
        assert!(!activate_if_foreground_owner(0x7FFF_FFF0));
    }
}
