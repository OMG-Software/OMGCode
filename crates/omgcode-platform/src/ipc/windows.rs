//! The Windows channel: one named-pipe instance that an AppContainer reaches.
//!
//! # Security descriptor
//!
//! ```text
//! D:P(A;;0x100083;;;<current user>)(A;;0x100083;;;<container>)S:(ML;;NW;;;LW)
//! ```
//!
//! - `D:P` is a protected DACL with no inheritance and exactly two allow
//!   entries. A process in an AppContainer must pass the ordinary user and
//!   group check with its token's user SID and a second check with the
//!   container's package SID; each must be granted, so there is one entry for
//!   each. No other principal, not even administrators, has an entry.
//! - Mask `0x100083` is `FILE_READ_DATA | FILE_WRITE_DATA |
//!   FILE_READ_ATTRIBUTES | SYNCHRONIZE`. The first two are what a client
//!   needs to read and write the stream. `CreateFileW` adds the other two to
//!   every open request (observed: the same mask without them fails with
//!   `ERROR_ACCESS_DENIED` for the owner), and they let the holder read
//!   attributes and wait on the handle, nothing more. The mask leaves out
//!   `FILE_CREATE_PIPE_INSTANCE` (`0x4`), which on a pipe is the same bit as
//!   `FILE_APPEND_DATA` and the one `GENERIC_WRITE` adds: a client that could
//!   set it could create a second server instance. It also leaves out
//!   `FILE_WRITE_ATTRIBUTES`, the extended-attribute rights, `READ_CONTROL`,
//!   `WRITE_DAC`, and `WRITE_OWNER`. For the same reason the client asks for
//!   the two specific data rights and never for `GENERIC_WRITE`. Only the
//!   first instance exists (`nMaxInstances` is 1, and
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` stops another process from creating the
//!   name first).
//! - `S:(ML;;NW;;;LW)` is a Low mandatory integrity label with no-write-up.
//!   AppContainer processes run at Low integrity, and a pipe left at the
//!   default Medium label would refuse their writes. No-write-up is the
//!   default policy bit; no read-up or execute-up is set, so lower-integrity
//!   processes cannot be locked out of reading.
//!
//! The user entry also admits any other program that runs as this user, so
//! [`Listener::accept`] checks the connected client's process token for the
//! container SID and disconnects any other client.
//!
//! # Concurrency
//!
//! A synchronous handle serializes its I/O, so a blocked read would block a
//! write on the same handle. Every handle here is opened overlapped, and each
//! half owns its own event, so a read and a write proceed independently and
//! `CancelIoEx` can end a pending one. An overlapped operation is always
//! waited to completion before its `OVERLAPPED` and buffer go out of scope.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_NOT_FOUND,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_APPCONTAINER_INFORMATION,
    TOKEN_QUERY, TokenAppContainerSid, TokenIsAppContainer,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES,
    FILE_READ_DATA, FILE_WRITE_DATA, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_ANONYMOUS, SECURITY_SQOS_PRESENT, SYNCHRONIZE, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    WaitForSingleObject,
};

use super::{IpcError, ListenerTarget};
use crate::container::windows::{derive_sid, sid_string};
use crate::private_store::windows::{Sid, current_user_sid};
use crate::process::CancelSignal;

/// Every pipe this module creates starts with this prefix.
const PIPE_DIRECTORY: &str = r"\\.\pipe\";
/// The generated name's fixed part, before 32 lowercase hexadecimal digits.
const NAME_PREFIX: &str = "OMGCode-mcp-";
/// Longest pipe name a client may ask for, after the directory prefix.
const MAX_NAME_CHARS: usize = 128;
/// Bytes of kernel buffer for each direction.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
/// Largest transfer one read or write call asks for.
const IO_CHUNK_BYTES: u32 = 64 * 1024;
/// How often a wait rechecks the deadline and the cancel signal.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long one `WaitNamedPipeW` waits before the deadline is rechecked.
const BUSY_WAIT_MAX: Duration = Duration::from_millis(250);
/// Clients turned away before `accept` gives up. The one pipe instance means
/// each rejected client is disconnected before the next can connect.
const MAX_REJECTED_CLIENTS: u32 = 8;
/// What a client asks for when it opens the pipe.
const CLIENT_ACCESS: u32 = FILE_READ_DATA | FILE_WRITE_DATA;
/// What the descriptor grants each of the two principals: the client request
/// plus the two rights `CreateFileW` always adds. See the module documentation.
const GRANTED_ACCESS: u32 = CLIENT_ACCESS | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// Largest token SID information accepted, in bytes.
const MAX_TOKEN_INFORMATION_BYTES: u32 = 1024;

fn last_code() -> u32 {
    io::Error::last_os_error()
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
        .unwrap_or(0)
}

fn code_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX))
}

fn wide_nul(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// Take ownership of a handle returned by a call that signals failure with a
/// null or invalid handle.
#[allow(unsafe_code)]
fn own(raw: HANDLE) -> Option<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: `raw` is a valid handle that the caller just received from a
    // successful system call and that nothing else owns.
    Some(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// A manual-reset event for one half's overlapped operations. It is not
/// inheritable.
#[allow(unsafe_code)]
fn new_event() -> Result<OwnedHandle, IpcError> {
    // SAFETY: null attributes and name request an unnamed, non-inheritable
    // event; the other arguments are plain flags.
    let raw = unsafe { CreateEventW(null(), 1, 0, null()) };
    own(raw).ok_or_else(|| IpcError::os("create event", io::Error::last_os_error()))
}

/// An open pipe handle, shared by the halves of one stream. It closes when the
/// last owner drops it; the owners are the reader, the writer, and every
/// shutdown handle, none of which refers back, so there is no cycle.
struct Pipe {
    handle: OwnedHandle,
    shut: AtomicBool,
}

impl Pipe {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            handle,
            shut: AtomicBool::new(false),
        }
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle()
    }

    fn is_shut(&self) -> bool {
        self.shut.load(Ordering::SeqCst)
    }
}

fn overlapped(event: &OwnedHandle) -> OVERLAPPED {
    OVERLAPPED {
        hEvent: event.as_raw_handle(),
        ..OVERLAPPED::default()
    }
}

/// Finish an overlapped read or write that was just started on `pipe`.
///
/// `start_error` is `None` when the call reported immediate success, and
/// otherwise the error code read straight after the call. A pending operation
/// is cancelled here if shutdown already ran: shutdown sets its flag before it
/// cancels, and the caller starts the operation before this check, so either
/// shutdown's cancel finds the operation or this check sees the flag. The
/// operation is always waited to completion, so the buffer and `OVERLAPPED` can
/// be released. Returns the byte count or the error code.
#[allow(unsafe_code)]
fn complete(pipe: &Pipe, operation: &OVERLAPPED, start_error: Option<u32>) -> Result<u32, u32> {
    match start_error {
        None => {}
        Some(ERROR_IO_PENDING) => {
            if pipe.is_shut() {
                // SAFETY: the handle is open while `pipe` lives and `operation`
                // names an operation started on it. A failure means it already
                // finished, which the wait below observes.
                let _ = unsafe { CancelIoEx(pipe.raw(), operation) };
            }
        }
        Some(code) => return Err(code),
    }
    let mut transferred = 0_u32;
    // SAFETY: the handle and `operation` are valid; waiting on the operation's
    // own event is what `bWait` requests, and `transferred` is writable.
    let finished = unsafe { GetOverlappedResult(pipe.raw(), operation, &raw mut transferred, 1) };
    if finished == 0 {
        Err(last_code())
    } else {
        Ok(transferred)
    }
}

/// The read half of a stream. One thread uses it at a time.
pub(super) struct Reader {
    pipe: Arc<Pipe>,
    event: OwnedHandle,
}

impl Reader {
    #[allow(unsafe_code)]
    pub(super) fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.pipe.is_shut() {
            return Ok(0);
        }
        let length = u32::try_from(buffer.len())
            .unwrap_or(u32::MAX)
            .min(IO_CHUNK_BYTES);
        let mut operation = overlapped(&self.event);
        // SAFETY: `buffer` is writable for `length` bytes and, with
        // `operation`, outlives the operation because `complete` waits for it.
        // A null byte count is required for an overlapped call.
        let started = unsafe {
            ReadFile(
                self.pipe.raw(),
                buffer.as_mut_ptr(),
                length,
                null_mut(),
                &raw mut operation,
            )
        };
        let start_error = (started == 0).then(last_code);
        match complete(&self.pipe, &operation, start_error) {
            Ok(count) => {
                usize::try_from(count).map_err(|_| io::Error::other("read length does not fit"))
            }
            // The peer closed the pipe, or shutdown cancelled the read.
            Err(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_OPERATION_ABORTED) => Ok(0),
            Err(code) => Err(code_error(code)),
        }
    }
}

/// The write half of a stream. One thread uses it at a time.
pub(super) struct Writer {
    pipe: Arc<Pipe>,
    event: OwnedHandle,
}

impl Writer {
    #[allow(unsafe_code)]
    pub(super) fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.pipe.is_shut() {
            return Err(closed());
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        let length = u32::try_from(buffer.len())
            .unwrap_or(u32::MAX)
            .min(IO_CHUNK_BYTES);
        let mut operation = overlapped(&self.event);
        // SAFETY: `buffer` is readable for `length` bytes and, with
        // `operation`, outlives the operation because `complete` waits for it.
        let started = unsafe {
            WriteFile(
                self.pipe.raw(),
                buffer.as_ptr(),
                length,
                null_mut(),
                &raw mut operation,
            )
        };
        let start_error = (started == 0).then(last_code);
        match complete(&self.pipe, &operation, start_error) {
            Ok(count) => {
                usize::try_from(count).map_err(|_| io::Error::other("write length does not fit"))
            }
            Err(
                ERROR_BROKEN_PIPE
                | ERROR_NO_DATA
                | ERROR_PIPE_NOT_CONNECTED
                | ERROR_OPERATION_ABORTED,
            ) => Err(closed()),
            Err(code) => Err(code_error(code)),
        }
    }

    /// A write returns after the pipe accepts the bytes into its buffer, so
    /// there is nothing to flush. `FlushFileBuffers` would wait for the peer
    /// to read them.
    pub(super) fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the channel is closed")
}

/// Cancels pending I/O on the stream from any thread.
#[derive(Clone)]
pub(super) struct Shutdown(Arc<Pipe>);

impl Shutdown {
    #[allow(unsafe_code)]
    pub(super) fn shutdown(&self) -> io::Result<()> {
        self.0.shut.store(true, Ordering::SeqCst);
        // SAFETY: the handle is open while the `Arc` lives; a null `OVERLAPPED`
        // cancels every pending operation on it.
        if unsafe { CancelIoEx(self.0.raw(), null()) } == 0 {
            let code = last_code();
            // Nothing pending is the usual case and not a failure.
            if code != ERROR_NOT_FOUND {
                return Err(code_error(code));
            }
        }
        Ok(())
    }
}

/// A connected pipe that has not been split.
pub(super) struct Stream {
    pipe: Arc<Pipe>,
}

impl Stream {
    pub(super) fn split(self) -> Result<(Reader, Writer, Shutdown), IpcError> {
        let read_event = new_event()?;
        let write_event = new_event()?;
        Ok((
            Reader {
                pipe: Arc::clone(&self.pipe),
                event: read_event,
            },
            Writer {
                pipe: Arc::clone(&self.pipe),
                event: write_event,
            },
            Shutdown(self.pipe),
        ))
    }
}

/// Frees memory that `LocalAlloc` returned, such as a converted descriptor.
struct LocalMemory(PSECURITY_DESCRIPTOR);

impl Drop for LocalMemory {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a call that requires `LocalFree`
            // and is freed only here. A failed free can only leak.
            let _ = unsafe { LocalFree(self.0.cast()) };
        }
    }
}

/// The listening side: one pipe instance, waiting for its one client.
pub(super) struct Listener {
    pipe: Pipe,
    address: OsString,
    expected: Sid,
    event: OwnedHandle,
}

impl Listener {
    pub(super) fn bind(target: ListenerTarget) -> Result<Self, IpcError> {
        let ListenerTarget::Container { container } = target;
        let expected = derive_sid(&container)
            .map_err(|source| IpcError::os("derive container SID", source))?;
        let user =
            current_user_sid().map_err(|source| IpcError::os("read the user SID", source))?;
        let descriptor = security_descriptor(&user, &expected)?;
        let random = random_hex().map_err(|source| IpcError::os("generate a pipe name", source))?;
        let address = OsString::from(format!("{PIPE_DIRECTORY}{NAME_PREFIX}{random}"));
        let pipe = create_pipe(&address, &descriptor)?;
        Ok(Self {
            pipe,
            address,
            expected,
            event: new_event()?,
        })
    }

    pub(super) fn address(&self) -> &OsStr {
        &self.address
    }

    pub(super) fn accept(
        self,
        deadline: Instant,
        cancel: &CancelSignal,
    ) -> Result<Stream, IpcError> {
        let mut rejected = 0_u32;
        loop {
            let connected = self.await_client(deadline, cancel)?;
            if connected && self.client_is_expected() {
                return Ok(Stream {
                    pipe: Arc::new(self.pipe),
                });
            }
            rejected += 1;
            if rejected > MAX_REJECTED_CLIENTS {
                // Dropping the pipe closes every client's connection.
                return Err(IpcError::PeerRejected);
            }
            self.disconnect()?;
        }
    }

    /// Wait for a client to connect. `Ok(false)` means one connected and left
    /// before it could be checked.
    #[allow(unsafe_code)]
    fn await_client(&self, deadline: Instant, cancel: &CancelSignal) -> Result<bool, IpcError> {
        check_limits(deadline, cancel)?;
        let mut operation = overlapped(&self.event);
        // SAFETY: the pipe is open and `operation` stays alive until this
        // function ends, after the operation is complete or abandoned.
        let connected = unsafe { ConnectNamedPipe(self.pipe.raw(), &raw mut operation) };
        if connected == 0 {
            match last_code() {
                // A client opened the pipe before the call: the usual case
                // when it connects before `accept` starts.
                ERROR_PIPE_CONNECTED => return Ok(true),
                ERROR_NO_DATA => return Ok(false),
                ERROR_IO_PENDING => {}
                code => return Err(IpcError::os("wait for a client", code_error(code))),
            }
            self.wait_for_connection(&operation, deadline, cancel)?;
        }
        let mut transferred = 0_u32;
        // SAFETY: the operation has signalled completion; the handle and
        // `operation` are valid and `transferred` is writable.
        let finished = unsafe {
            GetOverlappedResult(
                self.pipe.raw(),
                &raw const operation,
                &raw mut transferred,
                1,
            )
        };
        if finished != 0 {
            return Ok(true);
        }
        match last_code() {
            ERROR_NO_DATA => Ok(false),
            code => Err(IpcError::os("wait for a client", code_error(code))),
        }
    }

    /// Poll the pending connect until it signals, the deadline passes, or
    /// cancellation is requested. On the latter two the operation is cancelled
    /// and waited out before this returns.
    #[allow(unsafe_code)]
    fn wait_for_connection(
        &self,
        operation: &OVERLAPPED,
        deadline: Instant,
        cancel: &CancelSignal,
    ) -> Result<(), IpcError> {
        loop {
            if let Err(stop) = check_limits(deadline, cancel) {
                self.abandon(operation);
                return Err(stop);
            }
            // SAFETY: the event handle is open for the call.
            let status = unsafe {
                WaitForSingleObject(
                    self.event.as_raw_handle(),
                    wait_millis(deadline, POLL_INTERVAL),
                )
            };
            if status == WAIT_OBJECT_0 {
                return Ok(());
            }
            if status != WAIT_TIMEOUT {
                let source = io::Error::last_os_error();
                self.abandon(operation);
                return Err(IpcError::os("wait for a client", source));
            }
        }
    }

    /// Cancel the pending connect and wait until it has ended, so `operation`
    /// can be released. Its outcome no longer matters: either it was aborted
    /// or a client arrived that the caller is about to drop.
    #[allow(unsafe_code)]
    fn abandon(&self, operation: &OVERLAPPED) {
        let mut transferred = 0_u32;
        // SAFETY: the handle is open and `operation` names the operation
        // started on it; the wait observes its end. Failures only mean the
        // operation had already ended.
        let _ = unsafe { CancelIoEx(self.pipe.raw(), operation) };
        // SAFETY: as above; `transferred` is writable.
        let _ = unsafe { GetOverlappedResult(self.pipe.raw(), operation, &raw mut transferred, 1) };
    }

    #[allow(unsafe_code)]
    fn disconnect(&self) -> Result<(), IpcError> {
        // SAFETY: the pipe is open and no I/O is pending on it.
        if unsafe { DisconnectNamedPipe(self.pipe.raw()) } == 0 {
            return Err(IpcError::os(
                "disconnect a client",
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    /// Whether the connected client's process token holds the container SID.
    /// Any failure to find out means no.
    #[allow(unsafe_code)]
    fn client_is_expected(&self) -> bool {
        let mut process_id = 0_u32;
        // SAFETY: the pipe is open and `process_id` is writable.
        let found = unsafe { GetNamedPipeClientProcessId(self.pipe.raw(), &raw mut process_id) };
        if found == 0 || process_id == 0 {
            return false;
        }
        matches!(
            process_container_sid(process_id),
            Ok(Some(sid)) if sid.equals(&self.expected)
        )
    }
}

fn check_limits(deadline: Instant, cancel: &CancelSignal) -> Result<(), IpcError> {
    if cancel.is_cancelled() {
        return Err(IpcError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(IpcError::Deadline);
    }
    Ok(())
}

/// Milliseconds to wait: the time to `deadline`, at most `longest`, at least 1.
fn wait_millis(deadline: Instant, longest: Duration) -> u32 {
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .min(longest);
    u32::try_from(wait.as_millis()).unwrap_or(1).max(1)
}

/// The SID of the AppContainer that the process with `process_id` runs in, or
/// `None` when it is not in one. This opens the process for limited query
/// only. A process that cannot be opened is an error.
#[allow(unsafe_code)]
fn process_container_sid(process_id: u32) -> io::Result<Option<Sid>> {
    // SAFETY: the access mask is a plain constant and the id is a number.
    let process = own(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) })
        .ok_or_else(io::Error::last_os_error)?;
    let mut raw: HANDLE = null_mut();
    // SAFETY: `process` is open and `raw` is writable.
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = own(raw).ok_or_else(|| io::Error::other("the process token is invalid"))?;

    let mut is_container = 0_u32;
    let mut returned = 0_u32;
    // SAFETY: the buffer is a writable `u32` and its size is passed.
    let queried = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIsAppContainer,
            (&raw mut is_container).cast(),
            4,
            &raw mut returned,
        )
    };
    if queried == 0 {
        return Err(io::Error::last_os_error());
    }
    if is_container == 0 {
        return Ok(None);
    }

    // SAFETY: a null buffer with zero length is the documented size query.
    let sized = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenAppContainerSid,
            null_mut(),
            0,
            &raw mut returned,
        )
    };
    if sized != 0 || last_code() != ERROR_INSUFFICIENT_BUFFER {
        return Err(io::Error::other("unexpected token size answer"));
    }
    if returned == 0 || returned > MAX_TOKEN_INFORMATION_BYTES {
        return Err(io::Error::other("token information has an invalid size"));
    }
    let bytes = usize::try_from(returned).map_err(|_| io::Error::other("size overflow"))?;
    let mut storage = vec![0_u64; bytes.div_ceil(size_of::<u64>())];
    // SAFETY: `storage` is writable, 8-byte aligned, and at least `returned`
    // bytes long.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenAppContainerSid,
            storage.as_mut_ptr().cast(),
            returned,
            &raw mut returned,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success wrote a `TOKEN_APPCONTAINER_INFORMATION` at the start of
    // the aligned storage; its SID pointer, when set, refers into `storage`,
    // which is alive for the copy.
    let sid =
        unsafe { (*storage.as_ptr().cast::<TOKEN_APPCONTAINER_INFORMATION>()).TokenAppContainer };
    if sid.is_null() {
        return Ok(None);
    }
    Sid::copy_from(sid).map(Some)
}

/// The descriptor text for a pipe that `user` and `container` may use.
fn security_descriptor(user: &Sid, container: &Sid) -> Result<Vec<u16>, IpcError> {
    let user = sid_string(user).map_err(|source| IpcError::os("format the user SID", source))?;
    let container =
        sid_string(container).map_err(|source| IpcError::os("format the container SID", source))?;
    let sddl = format!(
        "D:P(A;;{GRANTED_ACCESS:#x};;;{user})(A;;{GRANTED_ACCESS:#x};;;{container})S:(ML;;NW;;;LW)"
    );
    Ok(wide_nul(OsStr::new(&sddl)))
}

#[allow(unsafe_code)]
fn create_pipe(address: &OsStr, sddl: &[u16]) -> Result<Pipe, IpcError> {
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `sddl` is NUL-terminated UTF-16 that outlives the call and
    // `descriptor` is writable; the size output is optional.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(IpcError::os(
            "build the pipe descriptor",
            io::Error::last_os_error(),
        ));
    }
    let descriptor = LocalMemory(descriptor);
    let length = u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
        .map_err(|_| IpcError::InvalidTarget("security attribute size"))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: length,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide_nul(address);
    // SAFETY: `name` is NUL-terminated UTF-16 and `attributes` refers to a
    // valid descriptor; both outlive the call, which copies what it keeps.
    let raw = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            &raw const attributes,
        )
    };
    own(raw)
        .map(Pipe::new)
        .ok_or_else(|| IpcError::os("create the pipe", io::Error::last_os_error()))
}

/// 32 lowercase hexadecimal digits from the system random number generator.
#[allow(unsafe_code)]
fn random_hex() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    // SAFETY: the buffer is writable for its length, and the system-preferred
    // generator needs no algorithm handle.
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            bytes.as_mut_ptr(),
            16,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(io::Error::other("the system random generator failed"));
    }
    let mut text = String::with_capacity(32);
    for byte in bytes {
        write!(text, "{byte:02x}").map_err(|_| io::Error::other("hex formatting failed"))?;
    }
    Ok(text)
}

/// Accept only `\\.\pipe\` followed by a short run of letters, digits, `.`,
/// `-`, and `_`, so a caller-supplied address cannot name a file, a remote
/// host, or a nested pipe path.
fn validate_address(address: &OsStr) -> Result<(), IpcError> {
    let text = address.to_str().ok_or(IpcError::InvalidTarget(
        "the address is not a local pipe name",
    ))?;
    let name = text
        .strip_prefix(PIPE_DIRECTORY)
        .ok_or(IpcError::InvalidTarget(
            "the address is not a local pipe name",
        ))?;
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_CHARS
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(IpcError::InvalidTarget(
            "the address is not a local pipe name",
        ))
    }
}

/// Open the pipe at `address`, waiting while its one instance is busy.
///
/// The client asks for exactly `FILE_READ_DATA | FILE_WRITE_DATA`, opens
/// overlapped, and sets the anonymous impersonation level so the server cannot
/// act as the client. It does not authenticate the server: the name is a
/// 128-bit secret known only to the program that started this one.
#[allow(unsafe_code)]
pub(super) fn connect(address: &OsStr, deadline: Instant) -> Result<Stream, IpcError> {
    validate_address(address)?;
    let name = wide_nul(address);
    loop {
        if Instant::now() >= deadline {
            return Err(IpcError::Deadline);
        }
        // SAFETY: `name` is NUL-terminated UTF-16 that outlives the call;
        // null attributes and template are valid and the handle is not
        // inheritable.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                CLIENT_ACCESS,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
                null_mut(),
            )
        };
        if let Some(handle) = own(raw) {
            return Ok(Stream {
                pipe: Arc::new(Pipe::new(handle)),
            });
        }
        let code = last_code();
        if code != ERROR_PIPE_BUSY {
            return Err(IpcError::os("connect", code_error(code)));
        }
        // SAFETY: `name` is NUL-terminated UTF-16. A failed or timed-out wait
        // is not an error: the next open reports the pipe's state.
        let _ = unsafe { WaitNamedPipeW(name.as_ptr(), wait_millis(deadline, BUSY_WAIT_MAX)) };
    }
}

#[cfg(test)]
mod tests;
