//! Helpers shared by tests that need the real native credential store or a
//! loopback mock HTTP server.

use std::fs;
use std::io::Read;
use std::path::Path;

/// Wait, within the stream's read timeout, until a client that received a
/// complete `Connection: close` reply closes its side, and only then let the
/// caller drop the stream.
///
/// The client then holds the connection's TIME_WAIT. On Windows the next
/// client can receive the same local port, and a TIME_WAIT that the server
/// holds after a large request delays that connect until it times out
/// (ADR-0078). Call this only after a reply framed by `Content-Length`, never
/// after a reply that the close delimits, and only on a stream with a read
/// timeout. A read error or timeout ends the wait; the caller still closes.
///
/// Returns whether the client's close, orderly or abortive, was observed. A
/// test that depends on
/// the client-side TIME_WAIT must assert it, because a timeout means the
/// fixture, not the client, closes first.
pub(crate) fn await_client_close(stream: &mut impl Read) -> bool {
    let mut sink = [0_u8; 512];
    loop {
        match stream.read(&mut sink) {
            Ok(0) => return true,
            Ok(_) => {}
            // An abortive close, as a cancelled client makes, leaves no
            // TIME_WAIT on either side.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                return true;
            }
            Err(_) => return false,
        }
    }
}

/// Create `data` as a private data root. Windows temporary directories
/// inherit grants the private store refuses, so give the root an owner-only
/// ACL, as an installed profile has.
pub(crate) fn private_data_root(data: &Path) {
    fs::create_dir(data).expect("data root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(data, fs::Permissions::from_mode(0o700)).expect("private mode");
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        let output = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .expect("whoami");
        assert!(output.status.success(), "whoami failed");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        let sid = text
            .trim()
            .rsplit(',')
            .next()
            .expect("SID column")
            .trim_matches('"')
            .to_owned();
        let status = Command::new("icacls")
            .arg(data)
            .args(["/inheritance:r", "/grant:r", &format!("*{sid}:(OI)(CI)F")])
            .output()
            .expect("icacls");
        assert!(status.status.success(), "icacls failed");
    }
}
