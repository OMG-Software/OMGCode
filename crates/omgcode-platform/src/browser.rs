//! Open an `https` address in the operator's default browser (ADR-0094).
//!
//! Windows hands the address to the shell's URL association with
//! `ShellExecuteExW` and the `open` verb, the documented way to open a URL.
//! Linux and other Unix systems run `/usr/bin/xdg-open` with the address as
//! one argument. Neither route starts a command shell, and neither parses the
//! address as a command line. A started browser is not supervised: it belongs
//! to the operator's desktop session, not to OMGCode.

/// Longest address handed to a browser, in bytes.
pub const MAX_ADDRESS_LEN: usize = 8 * 1024;

/// Why an address was not handed to a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserError {
    /// The address is not an `https` URL of at most [`MAX_ADDRESS_LEN`] bytes
    /// drawn from RFC 3986 URI characters.
    InvalidAddress,
    /// No browser could be started for the address.
    LaunchFailed,
}

/// Open `address` in the default browser and return once the hand-off is done.
///
/// Only an `https://` address made of RFC 3986 URI characters is accepted,
/// so a caller's already-encoded URL cannot be mistaken for a file path, a
/// second argument, or a different scheme. The caller remains responsible
/// for checking that the address points where it should.
///
/// On Windows this blocks while the shell resolves the URL association, on a
/// dedicated thread that initializes COM for the call.
///
/// # Errors
///
/// [`BrowserError::InvalidAddress`] for a refused address and
/// [`BrowserError::LaunchFailed`] when the shell or `xdg-open` could not be
/// started or reported failure.
pub fn open_https_address(address: &str) -> Result<(), BrowserError> {
    if !is_plain_https(address) {
        return Err(BrowserError::InvalidAddress);
    }
    launch(address)
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
fn launch(address: &str) -> Result<(), BrowserError> {
    let file: Vec<u16> = address.encode_utf16().chain(std::iter::once(0)).collect();
    // A new thread owns its COM apartment, so the caller's apartment (the
    // desktop shell's UI thread, a runtime worker, or the CLI's main thread)
    // is never changed.
    std::thread::Builder::new()
        .name("omgcode-browser".to_owned())
        .spawn(move || shell_open(&file))
        .map_err(|_| BrowserError::LaunchFailed)?
        .join()
        .map_err(|_| BrowserError::LaunchFailed)?
}

/// Open the NUL-terminated UTF-16 `file` with the shell's `open` verb. Must
/// run on a thread that has not initialized COM.
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

    const OPEN: [u16; 5] = [0x6f, 0x70, 0x65, 0x6e, 0]; // "open\0"

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
        lpVerb: OPEN.as_ptr(),
        lpFile: file.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: `info` is a fully initialized SHELLEXECUTEINFOW whose cbSize is
    // its own size. `lpVerb` and `lpFile` point to NUL-terminated UTF-16 that
    // outlives the call, and the other pointers and handles are null. Without
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
fn launch(address: &str) -> Result<(), BrowserError> {
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
    Ok(())
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
                open_https_address(address),
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
}
