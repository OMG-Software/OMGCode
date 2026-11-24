//! Open an `https` address in the operator's default browser (ADR-0094).
//!
//! Windows hands the address to the shell's URL association with
//! `ShellExecuteExW` and the handler's default verb, the documented way to
//! open a URL.
//! Linux and other Unix systems run `/usr/bin/xdg-open` with the address as
//! one argument. Neither route starts a command shell, and neither parses the
//! address as a command line. A started browser is not supervised: it belongs
//! to the operator's desktop session, not to OMGCode.

use std::time::Duration;

/// Longest address handed to a browser, in bytes.
pub const MAX_ADDRESS_LEN: usize = 8 * 1024;

/// Longest time [`open_https_address`] waits for the Windows shell to finish
/// the hand-off. A URL handler that hangs must not hold the caller, which may
/// own a sign-in slot and must still observe its own cancellation and deadline.
pub const HAND_OFF_WAIT: Duration = Duration::from_secs(5);

/// How often the hand-off wait checks the caller's stop condition.
#[cfg(any(windows, test))]
const STOP_POLL: Duration = Duration::from_millis(50);

/// How far the hand-off got before [`open_https_address`] returned.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    /// The Windows shell accepted the address, or `xdg-open` was started.
    Done,
    /// The Windows shell had not finished within [`HAND_OFF_WAIT`], or the
    /// caller's stop condition held first. The call continues on its own
    /// thread and its outcome is not reported, so the browser may or may not
    /// open.
    Pending,
}

/// Why an address was not handed to a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserError {
    /// The address is not an `https` URL of at most [`MAX_ADDRESS_LEN`] bytes
    /// drawn from RFC 3986 URI characters.
    InvalidAddress,
    /// No browser could be started for the address.
    LaunchFailed,
    /// An earlier Windows hand-off is still inside the shell call, so a URL
    /// handler appears to be hung. Only one hand-off thread runs at a time.
    Busy,
}

/// Set while a Windows hand-off thread is inside the shell call, so a hung
/// URL handler holds one thread for the process, not one per retry.
#[cfg(any(windows, test))]
static HAND_OFF_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The one hand-off thread's claim on [`HAND_OFF_RUNNING`]. Dropping it,
/// including when the thread unwinds or is never started, releases it.
#[cfg(any(windows, test))]
struct HandOffSlot;

#[cfg(any(windows, test))]
impl HandOffSlot {
    fn claim() -> Option<Self> {
        use std::sync::atomic::Ordering;
        HAND_OFF_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

#[cfg(any(windows, test))]
impl Drop for HandOffSlot {
    fn drop(&mut self) {
        HAND_OFF_RUNNING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Open `address` in the default browser.
///
/// Only an `https://` address made of RFC 3986 URI characters is accepted,
/// so a caller's already-encoded URL cannot be mistaken for a file path, a
/// second argument, or a different scheme. The caller remains responsible
/// for checking that the address points where it should.
///
/// On Windows the shell call runs on a dedicated thread that initializes COM
/// for it, and this function waits at most [`HAND_OFF_WAIT`] for its result,
/// checking `stop` every 50 ms so a caller's cancellation ends the wait
/// promptly. After either it returns [`Handoff::Pending`] and leaves the
/// thread to finish on its own: the thread holds only its copy of the address,
/// and no OMGCode lock, so a hung URL handler keeps that one thread alive and
/// nothing else. While that thread is still in the shell call, a new Windows
/// hand-off is refused as [`BrowserError::Busy`] instead of starting another.
/// `stop` is not consulted on Unix, where `xdg-open` is started without
/// waiting.
///
/// # Errors
///
/// [`BrowserError::InvalidAddress`] for a refused address,
/// [`BrowserError::Busy`] while an earlier Windows hand-off is still running,
/// and [`BrowserError::LaunchFailed`] when the shell or `xdg-open` could not
/// be started or reported failure.
pub fn open_https_address(address: &str, stop: &dyn Fn() -> bool) -> Result<Handoff, BrowserError> {
    if !is_plain_https(address) {
        return Err(BrowserError::InvalidAddress);
    }
    launch(address, stop)
}

fn is_plain_https(address: &str) -> bool {
    address.len() <= MAX_ADDRESS_LEN
        && address
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty())
        && address.bytes().all(is_uri_byte)
}

/// RFC 3986 unreserved and reserved characters and `%`. Quotes, spaces,
/// backslashes, and control or non-ASCII bytes must arrive percent-encoded.
fn is_uri_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&byte)
}

#[cfg(windows)]
fn launch(address: &str, stop: &dyn Fn() -> bool) -> Result<Handoff, BrowserError> {
    let slot = HandOffSlot::claim().ok_or(BrowserError::Busy)?;
    let file: Vec<u16> = address.encode_utf16().chain(std::iter::once(0)).collect();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    // A new thread owns its COM apartment, so the caller's apartment (the
    // desktop shell's UI thread, a runtime worker, or the CLI's main thread)
    // is never changed. Its join handle is dropped on purpose: the result
    // arrives through the channel, and a thread still in the shell call after
    // HAND_OFF_WAIT is left to finish (see `open_https_address`).
    std::thread::Builder::new()
        .name("omgcode-browser".to_owned())
        .spawn(move || {
            // The slot is released when this closure ends, or is dropped
            // unrun if the thread cannot be started.
            let _slot = slot;
            // After a Pending return nobody receives; that outcome is
            // documented as unreported, so a failed send is ignored.
            let _ = sender.send(shell_open(&file));
        })
        .map_err(|_| BrowserError::LaunchFailed)?;
    await_handoff(&receiver, HAND_OFF_WAIT, stop)
}

/// Wait at most `wait` for a hand-off result, ending early once `stop`
/// holds. A sender dropped without a result means the shell thread panicked.
#[cfg(any(windows, test))]
fn await_handoff(
    receiver: &std::sync::mpsc::Receiver<Result<(), BrowserError>>,
    wait: Duration,
    stop: &dyn Fn() -> bool,
) -> Result<Handoff, BrowserError> {
    use std::sync::mpsc::RecvTimeoutError;
    let deadline = std::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() || stop() {
            return Ok(Handoff::Pending);
        }
        match receiver.recv_timeout(left.min(STOP_POLL)) {
            Ok(result) => return result.map(|()| Handoff::Done),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(BrowserError::LaunchFailed),
        }
    }
}

/// Open the NUL-terminated UTF-16 `file` with the URL handler's default
/// verb. Must run on a thread that has not initialized COM.
#[cfg(windows)]
#[allow(unsafe_code)]
fn shell_open(file: &[u16]) -> Result<(), BrowserError> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if file.last() != Some(&0) {
        return Err(BrowserError::InvalidAddress);
    }
    let size =
        u32::try_from(size_of::<SHELLEXECUTEINFOW>()).map_err(|_| BrowserError::LaunchFailed)?;
    let mode = u32::try_from(COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
        .map_err(|_| BrowserError::LaunchFailed)?;
    // Microsoft documents that ShellExecuteEx may activate shell extensions
    // through COM, so the thread initializes a single-threaded apartment
    // with OLE1 DDE disabled first.
    // SAFETY: the reserved argument must be null, and this thread is new, so
    // no earlier initialization in another mode can conflict.
    let initialized = unsafe { CoInitializeEx(std::ptr::null(), mode) };
    if initialized < 0 {
        return Err(BrowserError::LaunchFailed);
    }
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size,
        // NOASYNC waits for the hand-off to finish before this thread ends;
        // FLAG_NO_UI reports failure instead of showing a dialog.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        // A null verb uses the handler's default verb, then `open`, so a
        // handler without an `open` verb still works.
        lpVerb: std::ptr::null(),
        lpFile: file.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: `info` is a fully initialized SHELLEXECUTEINFOW whose cbSize is
    // its own size. `lpFile` points to NUL-terminated UTF-16 that outlives
    // the call, and the verb and the other pointers and handles are null. Without
    // SEE_MASK_NOCLOSEPROCESS no process handle is returned, so nothing needs
    // closing afterwards.
    let opened = unsafe { ShellExecuteExW(&mut info) };
    // SAFETY: balances the successful CoInitializeEx above (S_OK or S_FALSE)
    // on the same thread, after the last COM use.
    unsafe { CoUninitialize() };
    if opened == 0 {
        Err(BrowserError::LaunchFailed)
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn launch(address: &str, _stop: &dyn Fn() -> bool) -> Result<Handoff, BrowserError> {
    let mut child = std::process::Command::new("/usr/bin/xdg-open")
        .arg(address)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| BrowserError::LaunchFailed)?;
    // Reap the launcher so it does not linger as a zombie. Its exit status is
    // not reported: xdg-open returns once it has handed the address on, and
    // the browser's own outcome is not observable here.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(Handoff::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_anything_but_a_plain_https_address() {
        let long = format!("https://example.test/?q={}", "a".repeat(MAX_ADDRESS_LEN));
        for address in [
            "",
            "https://",
            "http://example.test/",
            "HTTPS://example.test/",
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Windows\\explorer.exe",
            "https://example.test/ second-argument",
            "https://example.test/\tx",
            "https://example.test/\"x",
            "https://example.test/\\x",
            "https://example.test/<x>",
            "https://example.test/\u{e9}",
            "https://example.test/\0x",
            long.as_str(),
        ] {
            assert_eq!(
                open_https_address(address, &|| false),
                Err(BrowserError::InvalidAddress),
                "{address:?} must be refused before any launch"
            );
        }
    }

    #[test]
    fn accepts_a_long_encoded_sign_in_address() {
        // The ChatGPT authorize address is several hundred bytes of
        // percent-encoded query; a length that broke the old explorer.exe
        // route must pass validation.
        let address = format!(
            "https://auth.openai.com/api/accounts/authorize?response_type=code&state={}&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback",
            "s".repeat(600)
        );
        assert!(is_plain_https(&address));
    }

    #[test]
    fn a_hand_off_that_does_not_finish_in_time_is_pending_not_a_block() {
        // The sender stays alive and never sends, as a hung URL handler would.
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        let started = std::time::Instant::now();
        assert_eq!(
            await_handoff(&receiver, Duration::from_millis(50), &|| false),
            Ok(Handoff::Pending)
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait must end at its bound"
        );
    }

    #[test]
    fn a_finished_hand_off_reports_its_result() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.send(Ok(())).expect("the receiver is alive");
        assert_eq!(
            await_handoff(&receiver, Duration::from_secs(5), &|| false),
            Ok(Handoff::Done)
        );
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .send(Err(BrowserError::LaunchFailed))
            .expect("the receiver is alive");
        assert_eq!(
            await_handoff(&receiver, Duration::from_secs(5), &|| false),
            Err(BrowserError::LaunchFailed)
        );
    }

    #[test]
    fn a_shell_thread_that_ends_without_a_result_is_a_failure() {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Result<(), BrowserError>>(1);
        drop(sender);
        assert_eq!(
            await_handoff(&receiver, Duration::from_secs(5), &|| false),
            Err(BrowserError::LaunchFailed)
        );
    }

    #[test]
    fn only_one_hand_off_thread_holds_the_slot_at_a_time() {
        // No other test reaches a launch, so this test alone uses the slot.
        let first = HandOffSlot::claim().expect("the slot starts free");
        assert!(
            HandOffSlot::claim().is_none(),
            "a second hand-off must not start while one is running"
        );
        drop(first);
        let again = HandOffSlot::claim().expect("dropping the claim frees the slot");
        drop(again);
    }

    #[test]
    fn a_stop_request_ends_a_stalled_hand_off_before_its_bound() {
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        let started = std::time::Instant::now();
        let stop_after = started + Duration::from_millis(100);
        assert_eq!(
            await_handoff(&receiver, Duration::from_secs(30), &|| {
                std::time::Instant::now() >= stop_after
            }),
            Ok(Handoff::Pending)
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a stop must end the wait well before its 30-second bound"
        );
    }
}
