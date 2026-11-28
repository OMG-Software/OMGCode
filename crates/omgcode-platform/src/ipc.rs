//! A local, full-duplex byte channel that only a sandboxed peer should reach.
//!
//! The trusted OMGCode process binds a [`LocalListener`]; one sandboxed
//! program connects to it with [`connect`]. Loopback TCP is not used, so a
//! sandbox that denies network access still reaches this channel. Both
//! platforms hand back one [`LocalStream`] that [`LocalStream::split`] turns
//! into a reader, a writer, and a [`ShutdownHandle`] that two threads can use
//! at once.
//!
//! # Who may connect
//!
//! - **Windows.** The listener is a uniquely named pipe with a protected
//!   DACL that names only the current user and one AppContainer, a Low
//!   mandatory label, one instance, and no remote clients. The user entry
//!   also admits ordinary programs of the same user, so [`LocalListener::accept`]
//!   checks the connected client's process token for the container SID and
//!   disconnects anything else. A same-user program can still occupy the one
//!   instance and deny service; it cannot pass the check.
//! - **Linux.** The listener is a socket file inside a directory that is
//!   owned by the current user and has mode `0700`, and `accept` checks the
//!   peer's user ID with `SO_PEERCRED`. Linux gives no cheap proof that the
//!   peer is inside the sandbox: the private directory and the user check are
//!   the boundary, and callers add their own secret (a nonce the sandboxed
//!   program must present) before they trust the peer.
//!
//! # Errors
//!
//! [`IpcError`]'s `Display` never contains a path or pipe name, because the
//! text can reach a model as a tool result.

use std::error::Error;
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::time::Instant;

#[cfg(windows)]
use crate::container::ContainerName;
use crate::process::CancelSignal;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(windows)]
use windows as imp;

/// Where the listener lives and who may connect.
#[derive(Clone, Debug)]
pub enum ListenerTarget {
    /// A new uniquely named pipe that only a process in this AppContainer
    /// (and no other process of any user) can complete a connection to.
    #[cfg(windows)]
    Container {
        /// The container whose SID the connected client must hold.
        container: ContainerName,
    },
    /// A socket file `name` inside `directory`, which must be an existing
    /// absolute directory owned by the current user with mode `0700` exactly.
    /// The directory is opened without following any link.
    #[cfg(target_os = "linux")]
    PrivateDirectory {
        /// The private directory.
        directory: PathBuf,
        /// The socket file's name: one component, not `.` or `..`.
        name: OsString,
    },
}

/// A failed channel operation. No variant means a peer was accepted.
#[derive(Debug)]
pub enum IpcError {
    /// The target or address was refused before anything was bound or
    /// connected. The text is fixed and names no path.
    InvalidTarget(&'static str),
    /// The deadline passed first.
    Deadline,
    /// Cancellation was requested first.
    Cancelled,
    /// Several clients connected and failed the identity check, so the
    /// listener gave up. Those clients were disconnected.
    PeerRejected,
    /// A system call failed.
    Os {
        /// The operation that failed.
        operation: &'static str,
        /// The operating-system cause.
        source: io::Error,
    },
}

impl IpcError {
    pub(crate) fn os(operation: &'static str, source: io::Error) -> Self {
        Self::Os { operation, source }
    }
}

impl fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(reason) => write!(formatter, "invalid channel target: {reason}"),
            Self::Deadline => formatter.write_str("the channel deadline passed"),
            Self::Cancelled => formatter.write_str("the channel operation was cancelled"),
            Self::PeerRejected => {
                formatter.write_str("clients that failed the identity check were disconnected")
            }
            Self::Os { operation, source } => match source.raw_os_error() {
                Some(code) => write!(formatter, "{operation} failed (os error {code})"),
                None => write!(formatter, "{operation} failed ({})", source.kind()),
            },
        }
    }
}

impl Error for IpcError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Os { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The trusted side's listener. It admits one client; [`accept`] consumes it.
///
/// [`accept`]: LocalListener::accept
pub struct LocalListener(imp::Listener);

impl LocalListener {
    /// Create the listener described by `target`.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::InvalidTarget`] for a directory or name that is not
    /// acceptable, and [`IpcError::Os`] when the system refuses.
    pub fn bind(target: ListenerTarget) -> Result<Self, IpcError> {
        imp::Listener::bind(target).map(Self)
    }

    /// The address a client passes to [`connect`]: the full pipe name, or the
    /// socket path. Hand it only to the program that may connect.
    pub fn address(&self) -> &OsStr {
        self.0.address()
    }

    /// Wait for one acceptable client until `deadline`, polling `cancel`.
    ///
    /// A client that fails the identity check is disconnected and the wait
    /// continues, up to a small fixed number of such clients. On every
    /// outcome the listener is closed; on Linux the socket file is removed
    /// first, so a late client finds nothing.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Deadline`], [`IpcError::Cancelled`],
    /// [`IpcError::PeerRejected`], or [`IpcError::Os`].
    pub fn accept(self, deadline: Instant, cancel: &CancelSignal) -> Result<LocalStream, IpcError> {
        self.0.accept(deadline, cancel).map(LocalStream)
    }
}

impl fmt::Debug for LocalListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalListener")
            .finish_non_exhaustive()
    }
}

/// One connected channel, before it is split into halves.
pub struct LocalStream(imp::Stream);

impl LocalStream {
    /// Split into halves that two threads can use at the same time: one
    /// blocked read does not delay a write on the other half.
    ///
    /// The peer sees the channel close when the reader, the writer, and every
    /// [`ShutdownHandle`] clone are dropped.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Os`] when a half cannot be created.
    pub fn split(self) -> Result<(LocalReader, LocalWriter, ShutdownHandle), IpcError> {
        let (reader, writer, shutdown) = self.0.split()?;
        Ok((
            LocalReader(reader),
            LocalWriter(writer),
            ShutdownHandle(shutdown),
        ))
    }
}

impl fmt::Debug for LocalStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalStream")
            .finish_non_exhaustive()
    }
}

/// The read half. `read` returns `Ok(0)` when the peer closed the channel or
/// [`ShutdownHandle::shutdown`] ran.
pub struct LocalReader(imp::Reader);

impl Read for LocalReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl fmt::Debug for LocalReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalReader")
            .finish_non_exhaustive()
    }
}

/// The write half. `write` fails with [`io::ErrorKind::BrokenPipe`] once the
/// peer has closed the channel or [`ShutdownHandle::shutdown`] ran.
pub struct LocalWriter(imp::Writer);

impl Write for LocalWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl fmt::Debug for LocalWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalWriter")
            .finish_non_exhaustive()
    }
}

/// Ends a channel from a third thread. Clones share one channel.
///
/// `shutdown` makes a read that is blocked on the [`LocalReader`] return
/// `Ok(0)` promptly, cancels a blocked write, and makes later reads and writes
/// fail or return `Ok(0)`. It is idempotent. On Linux the peer then sees the
/// close at once; on Windows the peer sees it when the last handle is dropped.
#[derive(Clone)]
pub struct ShutdownHandle(imp::Shutdown);

impl ShutdownHandle {
    /// End the channel as described on the type.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error when the request cannot be made.
    pub fn shutdown(&self) -> io::Result<()> {
        self.0.shutdown()
    }
}

impl fmt::Debug for ShutdownHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShutdownHandle")
            .finish_non_exhaustive()
    }
}

/// Connect to the listener at `address` until `deadline`. This is the
/// sandboxed program's side.
///
/// On Windows `address` must be a local pipe name of the form the listener
/// reports; any other text is refused before it is opened. A busy pipe is
/// retried until the deadline.
///
/// # Errors
///
/// Returns [`IpcError::InvalidTarget`], [`IpcError::Deadline`], or
/// [`IpcError::Os`].
pub fn connect(address: &OsStr, deadline: Instant) -> Result<LocalStream, IpcError> {
    imp::connect(address, deadline).map(LocalStream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_name_no_path_or_pipe() {
        let secret = "pipe OMGCode-mcp-0123 /private/socket";
        let errors = [
            IpcError::InvalidTarget("the directory is not private"),
            IpcError::Deadline,
            IpcError::Cancelled,
            IpcError::PeerRejected,
            IpcError::os("open", io::Error::from_raw_os_error(5)),
        ];
        for error in errors {
            let text = error.to_string();
            assert!(!text.contains("OMGCode-mcp") && !text.contains("/private/"));
        }
        // An OS cause that carries text is reported by code only.
        let text = IpcError::os("open", io::Error::other(secret)).to_string();
        assert_eq!(text, "open failed (other error)");
        assert_eq!(
            IpcError::os("open", io::Error::from_raw_os_error(5)).to_string(),
            "open failed (os error 5)"
        );
    }
}
