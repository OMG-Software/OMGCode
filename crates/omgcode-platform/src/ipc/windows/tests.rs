//! Pipe tests with a real AppContainer. The child is the platform test
//! executable, copied into a folder granted to the container and started with
//! a fixture mode, as the process-confinement tests do. Its `fixture` test
//! does nothing unless the mode variable is set.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING};

use super::*;
use crate::container::windows::tests::{identity, unique_name};
use crate::container::{ContainerAccess, ContainerName, grant, revoke};
use crate::ipc::{
    IpcError, ListenerTarget, LocalListener, LocalReader, LocalStream, LocalWriter, ShutdownHandle,
    connect,
};
use crate::process::{
    CancelSignal, Confinement, NetworkAccess, ProcessLimits, ProcessOutcome, ProcessSpec,
    Termination, UnixSocketAccess, run_confined,
};

const FIXTURE_ENV: &str = "OMGCODE_IPC_FIXTURE";
const ADDRESS_ENV: &str = "OMGCODE_IPC_ADDRESS";
const FIXTURE_NAME: &str = "ipc::windows::tests::fixture";
/// More than the pipe's 64 KiB buffer, so a write needs the peer to read.
const PAYLOAD_BYTES: usize = 512 * 1024;
const LONG: Duration = Duration::from_secs(60);
/// Grace period after a thread reports that it is about to block. User space
/// cannot observe that an overlapped read or `ConnectNamedPipe` is pending
/// without a hook in production code, so a test signals immediately before the
/// blocking call and then waits this long for the thread to reach it. A thread
/// that is slower only turns the test into one that raises the signal before
/// the operation starts, which production code also handles; the pre-raised
/// case has its own assertion.
const PENDING_GRACE: Duration = Duration::from_millis(100);

/// Child entry point. It does nothing unless a mode was selected.
#[test]
fn fixture() {
    let Ok(mode) = env::var(FIXTURE_ENV) else {
        return;
    };
    let address = env::var_os(ADDRESS_ENV).expect("pipe address");
    let code = match mode.as_str() {
        "exchange" => child_exchange(&address),
        "duplex" => child_duplex(&address),
        "hold" => child_hold(&address),
        "try-connect" => child_try_connect(&address),
        _ => 99,
    };
    std::io::stdout().flush().expect("flush stdout");
    std::process::exit(code);
}

fn child_connect(address: &OsStr) -> Option<(LocalReader, LocalWriter, ShutdownHandle)> {
    match connect(address, Instant::now() + Duration::from_secs(30)).and_then(LocalStream::split) {
        Ok(halves) => Some(halves),
        Err(error) => {
            println!("child connect failed: {error}");
            None
        }
    }
}

fn child_exchange(address: &OsStr) -> i32 {
    let Some((reader, mut writer, _shutdown)) = child_connect(address) else {
        return 2;
    };
    if writer.write_all(b"hello\n").is_err() {
        return 3;
    }
    let mut line = String::new();
    if BufReader::new(reader).read_line(&mut line).is_err() || line != "world\n" {
        println!("child read {line:?}");
        return 4;
    }
    0
}

/// Read the whole payload, then answer. The server writes the payload while
/// its own read of this answer is already blocked.
fn child_duplex(address: &OsStr) -> i32 {
    let Some((mut reader, mut writer, _shutdown)) = child_connect(address) else {
        return 2;
    };
    let mut payload = vec![0_u8; PAYLOAD_BYTES];
    if reader.read_exact(&mut payload).is_err() || payload.iter().any(|byte| *byte != b'x') {
        println!("child payload was wrong");
        return 3;
    }
    if writer.write_all(b"done\n").is_err() {
        return 4;
    }
    0
}

/// Announce, then read until the server ends the channel.
fn child_hold(address: &OsStr) -> i32 {
    let Some((mut reader, mut writer, _shutdown)) = child_connect(address) else {
        return 2;
    };
    if writer.write_all(b"ready\n").is_err() {
        return 3;
    }
    let mut buffer = [0_u8; 16];
    match reader.read(&mut buffer) {
        Ok(0) => 0,
        other => {
            println!("child expected end of stream, got {other:?}");
            4
        }
    }
}

fn child_try_connect(address: &OsStr) -> i32 {
    match connect(address, Instant::now() + Duration::from_secs(10)) {
        Ok(_) => println!("RESULT:connected"),
        Err(IpcError::Os { source, .. }) => {
            println!("RESULT:error:{}", source.raw_os_error().unwrap_or(-1));
        }
        Err(other) => println!("RESULT:other:{other}"),
    }
    0
}

/// A folder tree granted to one or more containers, with a copy of the test
/// executable to run inside them.
struct Scene {
    _root: tempfile::TempDir,
    bin: PathBuf,
    work: PathBuf,
    program: PathBuf,
    granted: Vec<ContainerName>,
}

impl Scene {
    fn new(container: &ContainerName) -> Self {
        let root = tempfile::tempdir().expect("scene root");
        let bin = root.path().join("bin");
        let work = root.path().join("work");
        fs::create_dir(&bin).expect("bin directory");
        fs::create_dir(&work).expect("work directory");
        let program = bin.join("ipc-probe.exe");
        fs::copy(env::current_exe().expect("test executable"), &program).expect("copy program");
        let mut scene = Self {
            _root: root,
            bin,
            work,
            program,
            granted: Vec::new(),
        };
        scene.grant_to(container);
        scene
    }

    fn grant_to(&mut self, container: &ContainerName) {
        for (path, access) in [
            (&self.bin, ContainerAccess::ReadExecute),
            (&self.work, ContainerAccess::ReadWrite),
        ] {
            grant(path, identity(path), container, access).expect("grant");
        }
        self.granted.push(container.clone());
    }

    fn spec(&self, mode: &str, address: &OsStr) -> ProcessSpec {
        let mut environment = vec![
            (OsString::from(FIXTURE_ENV), OsString::from(mode)),
            (OsString::from(ADDRESS_ENV), address.to_owned()),
            (
                OsString::from("LOCALAPPDATA"),
                env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA"),
            ),
        ];
        if let Some(root) = env::var_os("SystemRoot") {
            environment.push((OsString::from("SystemRoot"), root));
        }
        ProcessSpec {
            program: self.program.clone(),
            arguments: [
                "--exact",
                FIXTURE_NAME,
                "--nocapture",
                "--test-threads=1",
                "-q",
            ]
            .iter()
            .map(OsString::from)
            .collect(),
            working_directory: self.work.clone(),
            environment,
        }
    }

    /// Start the child in `container` on its own thread, because the run
    /// returns only after the child exits.
    fn start(
        &self,
        container: &ContainerName,
        mode: &str,
        address: &OsStr,
    ) -> JoinHandle<ProcessOutcome> {
        let spec = self.spec(mode, address);
        let confinement = Confinement {
            read_only: vec![self.bin.clone()],
            read_write: vec![self.work.clone()],
            unix_sockets: UnixSocketAccess::PairsOnly,
            network: NetworkAccess::Denied,
            container: Some(container.clone()),
        };
        let limits = ProcessLimits {
            deadline: LONG,
            ..ProcessLimits::default()
        };
        thread::spawn(move || {
            run_confined(&spec, &confinement, &limits, &CancelSignal::new()).expect("confined run")
        })
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        for container in &self.granted {
            for path in [&self.bin, &self.work] {
                // The temporary tree is removed next; a failed revoke only
                // leaves an entry on a directory that is about to go.
                let _ = revoke(path, identity(path), container);
            }
        }
    }
}

fn exited_ok(outcome: &ProcessOutcome) -> String {
    let stdout = String::from_utf8_lossy(&outcome.stdout.bytes).into_owned();
    assert_eq!(
        outcome.termination,
        Termination::Exited { code: 0 },
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&outcome.stderr.bytes)
    );
    stdout
}

/// Join `handle` or fail after `limit`, so a deadlock fails the test instead
/// of hanging it.
fn join_within<T>(handle: JoinHandle<T>, limit: Duration) -> T {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() {
        assert!(
            Instant::now() < deadline,
            "the thread did not finish in {limit:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    handle.join().expect("thread panicked")
}

fn listener_for(container: &ContainerName) -> LocalListener {
    LocalListener::bind(ListenerTarget::Container {
        container: container.clone(),
    })
    .expect("bind")
}

fn accept_in_thread(
    listener: LocalListener,
    cancel: &CancelSignal,
    wait: Duration,
) -> JoinHandle<Result<LocalStream, IpcError>> {
    let cancel = cancel.clone();
    thread::spawn(move || listener.accept(Instant::now() + wait, &cancel))
}

/// Accept on another thread. The receiver gets one message immediately before
/// the blocking `accept` call.
fn accept_in_thread_signalling(
    listener: LocalListener,
    cancel: &CancelSignal,
    wait: Duration,
) -> (
    JoinHandle<Result<LocalStream, IpcError>>,
    mpsc::Receiver<()>,
) {
    let cancel = cancel.clone();
    let (started, pending) = mpsc::channel();
    let handle = thread::spawn(move || {
        started.send(()).expect("report that accept is starting");
        listener.accept(Instant::now() + wait, &cancel)
    });
    (handle, pending)
}

/// Wait, bounded, for the signal sent just before a blocking call, then give
/// the call [`PENDING_GRACE`] to start blocking.
fn await_pending(pending: &mpsc::Receiver<()>) {
    pending
        .recv_timeout(LONG)
        .expect("the thread never reached its blocking call");
    thread::sleep(PENDING_GRACE);
}

#[test]
fn a_container_client_exchanges_a_line_each_way() {
    let name = unique_name();
    let scene = Scene::new(&name);
    let listener = listener_for(&name);
    let address = listener.address().to_owned();
    let accepting = accept_in_thread(listener, &CancelSignal::new(), LONG);
    let child = scene.start(&name, "exchange", &address);
    let stream = join_within(accepting, LONG).expect("accept the container client");
    let (reader, mut writer, _shutdown) = stream.split().expect("split");
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).expect("read");
    assert_eq!(line, "hello\n");
    writer.write_all(b"world\n").expect("write");
    let outcome = join_within(child, LONG);
    exited_ok(&outcome);
}

#[test]
fn a_blocked_read_does_not_block_a_write_on_the_same_stream() {
    let name = unique_name();
    let scene = Scene::new(&name);
    let listener = listener_for(&name);
    let address = listener.address().to_owned();
    let accepting = accept_in_thread(listener, &CancelSignal::new(), LONG);
    let child = scene.start(&name, "duplex", &address);
    let stream = join_within(accepting, LONG).expect("accept");
    let (reader, mut writer, _shutdown) = stream.split().expect("split");
    // The child sends nothing until it has the whole payload, so this read is
    // blocked for the whole write below.
    let (started, pending) = mpsc::channel();
    let reading = thread::spawn(move || {
        let mut line = String::new();
        started.send(()).expect("report that the read is starting");
        BufReader::new(reader).read_line(&mut line).map(|_| line)
    });
    // The write starts only once the read thread is at its blocking call.
    await_pending(&pending);
    let writing = thread::spawn(move || writer.write_all(&vec![b'x'; PAYLOAD_BYTES]));
    join_within(writing, LONG).expect("the write finished while a read was blocked");
    let line = join_within(reading, LONG).expect("read the answer");
    assert_eq!(line, "done\n");
    exited_ok(&join_within(child, LONG));
}

#[test]
fn shutdown_ends_a_blocked_read_and_the_peer_sees_the_close() {
    let name = unique_name();
    let scene = Scene::new(&name);
    let listener = listener_for(&name);
    let address = listener.address().to_owned();
    let accepting = accept_in_thread(listener, &CancelSignal::new(), LONG);
    let child = scene.start(&name, "hold", &address);
    let stream = join_within(accepting, LONG).expect("accept");
    let (mut reader, writer, shutdown) = stream.split().expect("split");
    let (ready_sender, ready) = mpsc::channel();
    let (pending_sender, pending) = mpsc::channel();
    let reading = thread::spawn(move || {
        let mut line = String::new();
        {
            let mut lines = BufReader::new(&mut reader);
            lines.read_line(&mut line).expect("ready line");
        }
        ready_sender.send(line).expect("report the line");
        // Nothing more will arrive: this read blocks until shutdown. The
        // signal goes out immediately before the blocking read.
        let mut buffer = [0_u8; 16];
        pending_sender
            .send(())
            .expect("report that the read is starting");
        reader.read(&mut buffer)
    });
    assert_eq!(ready.recv_timeout(LONG).expect("child ready"), "ready\n");
    // Shutdown also ends a read that starts later; the signal and grace make
    // the interrupted-read case the one that normally runs.
    await_pending(&pending);
    let started = Instant::now();
    shutdown.shutdown().expect("shutdown");
    let read = join_within(reading, Duration::from_secs(10)).expect("the read returns");
    assert_eq!(read, 0, "a shut-down read reports the end of the stream");
    assert!(started.elapsed() < Duration::from_secs(10));
    // Later calls fail or end, and shutdown is repeatable.
    shutdown.shutdown().expect("shutdown again");
    let mut writer = writer;
    assert_eq!(
        writer.write(b"x").expect_err("write after shutdown").kind(),
        io::ErrorKind::BrokenPipe
    );
    drop(writer);
    drop(shutdown);
    exited_ok(&join_within(child, LONG));
}

#[test]
fn a_client_in_another_container_cannot_open_the_pipe() {
    // Declared first, so its profile outlives the grants the scene revokes.
    let other = unique_name();
    let name = unique_name();
    let mut scene = Scene::new(&name);
    scene.grant_to(&other);
    let listener = listener_for(&name);
    let address = listener.address().to_owned();
    let cancel = CancelSignal::new();
    let accepting = accept_in_thread(listener, &cancel, LONG);
    let outcome = join_within(scene.start(&other, "try-connect", &address), LONG);
    // Only now stop waiting, so the pipe existed for the child's whole attempt.
    cancel.cancel();
    let accepted = join_within(accepting, Duration::from_secs(10));
    assert!(matches!(accepted, Err(IpcError::Cancelled)), "{accepted:?}");
    let stdout = exited_ok(&outcome);
    assert!(
        stdout.contains(&format!("RESULT:error:{ERROR_ACCESS_DENIED}")),
        "the other container is denied by the descriptor: {stdout}"
    );
}

#[test]
fn a_same_user_client_outside_the_container_is_disconnected() {
    let name = unique_name();
    let listener = listener_for(&name);
    let address = listener.address().to_owned();
    // This process passes the descriptor (same user) and must fail the token
    // check. `accept` keeps waiting after it and ends at the deadline.
    let client = thread::spawn(move || {
        let stream = connect(&address, Instant::now() + Duration::from_secs(10)).expect("connect");
        let (mut reader, mut writer, _shutdown) = stream.split().expect("split");
        let mut buffer = [0_u8; 8];
        // The server disconnects this client: the read ends, and the write
        // that follows has no reader.
        let read = reader.read(&mut buffer);
        (
            read.map_err(|error| error.kind()),
            writer.write(b"x").is_err(),
        )
    });
    let accepted = listener.accept(
        Instant::now() + Duration::from_secs(2),
        &CancelSignal::new(),
    );
    assert!(matches!(accepted, Err(IpcError::Deadline)), "{accepted:?}");
    let (read, write_failed) = join_within(client, Duration::from_secs(10));
    assert_eq!(read, Ok(0));
    assert!(write_failed);
}

#[test]
fn accept_gives_up_after_a_fixed_number_of_rejected_clients() {
    let name = unique_name();
    let listener = listener_for(&name);
    let address = listener.address().to_owned();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&stop);
    let clients = thread::spawn(move || {
        let mut connected = 0_u32;
        while !flag.load(Ordering::SeqCst) {
            // A busy pipe is retried inside `connect`; once the listener is
            // gone the open fails and the flag ends the loop.
            if connect(&address, Instant::now() + Duration::from_secs(5)).is_ok() {
                connected += 1;
            }
        }
        connected
    });
    let accepted = listener.accept(
        Instant::now() + Duration::from_secs(30),
        &CancelSignal::new(),
    );
    stop.store(true, Ordering::SeqCst);
    assert!(
        matches!(accepted, Err(IpcError::PeerRejected)),
        "{accepted:?}"
    );
    let connected = join_within(clients, Duration::from_secs(30));
    assert!(connected > 0, "clients did connect and were turned away");
}

#[test]
fn accept_stops_at_the_deadline() {
    let name = unique_name();
    let listener = listener_for(&name);
    let started = Instant::now();
    let accepted = listener.accept(started + Duration::from_millis(300), &CancelSignal::new());
    assert!(matches!(accepted, Err(IpcError::Deadline)), "{accepted:?}");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn accept_stops_when_cancelled() {
    let name = unique_name();
    let listener = listener_for(&name);
    let cancel = CancelSignal::new();
    let raiser = cancel.clone();
    let (accepting, pending) = accept_in_thread_signalling(listener, &cancel, LONG);
    await_pending(&pending);
    let started = Instant::now();
    raiser.cancel();
    let accepted = join_within(accepting, Duration::from_secs(10));
    assert!(matches!(accepted, Err(IpcError::Cancelled)), "{accepted:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    // A signal raised before the call stops it without waiting.
    let listener = listener_for(&name);
    let accepted = listener.accept(Instant::now() + LONG, &raiser);
    assert!(matches!(accepted, Err(IpcError::Cancelled)), "{accepted:?}");
}

/// A client that asks for `GENERIC_WRITE` also asks for the bit that creates
/// pipe instances, which the descriptor withholds.
#[test]
#[allow(unsafe_code)]
fn the_descriptor_withholds_pipe_instance_creation() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};

    let name = unique_name();
    let listener = listener_for(&name);
    let wide = wide_nul(listener.address());
    // SAFETY: `wide` is NUL-terminated UTF-16 that outlives the call; the
    // other arguments are plain values or null.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    let opened = own(handle);
    assert!(opened.is_none(), "GENERIC_WRITE must be denied");
    assert_eq!(last_code(), ERROR_ACCESS_DENIED);
    // The two specific rights are enough.
    let client = connect(listener.address(), Instant::now() + Duration::from_secs(5));
    assert!(client.is_ok(), "{client:?}");
}

#[test]
fn the_descriptor_text_names_two_principals_and_a_low_label() {
    let name = unique_name();
    let container = derive_sid(&name).expect("container SID");
    let user = current_user_sid().expect("user SID");
    let text = String::from_utf16(&security_descriptor(&user, &container).expect("descriptor")[..])
        .expect("utf-16");
    let text = text.trim_end_matches('\0');
    let user = sid_string(&user).expect("user text");
    let container = sid_string(&container).expect("container text");
    assert_eq!(
        text,
        format!("D:P(A;;0x100083;;;{user})(A;;0x100083;;;{container})S:(ML;;NW;;;LW)")
    );
}

#[test]
fn generated_names_are_random_and_fixed_in_shape() {
    let name = unique_name();
    let first = listener_for(&name);
    let second = listener_for(&name);
    assert_ne!(first.address(), second.address());
    for listener in [&first, &second] {
        let text = listener.address().to_str().expect("utf-8");
        let digits = text
            .strip_prefix(r"\\.\pipe\OMGCode-mcp-")
            .expect("fixed prefix");
        assert_eq!(digits.len(), 32);
        assert!(
            digits
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
        assert!(validate_address(listener.address()).is_ok());
    }
}

#[test]
fn clients_refuse_addresses_that_are_not_local_pipe_names() {
    for address in [
        r"C:\Windows\win.ini",
        r"\\server\pipe\x",
        r"\\.\pipe\a\b",
        r"\\.\pipe\..\x",
        r"\\.\pipe\",
        r"\\.\pipe\has space",
        r"\\.\PIPE\x",
        "",
    ] {
        let error = connect(OsStr::new(address), Instant::now() + Duration::from_secs(1))
            .expect_err(address);
        assert!(
            matches!(error, IpcError::InvalidTarget(_)),
            "{address}: {error:?}"
        );
    }
    let long = format!(r"\\.\pipe\{}", "a".repeat(MAX_NAME_CHARS + 1));
    assert!(validate_address(OsStr::new(&long)).is_err());
}

#[test]
fn this_process_is_not_in_a_container() {
    assert!(
        process_container_sid(std::process::id())
            .expect("own token")
            .is_none()
    );
}

#[test]
fn a_second_pipe_with_the_same_name_cannot_be_created() {
    let name = unique_name();
    let listener = listener_for(&name);
    let container = derive_sid(&name).expect("SID");
    let user = current_user_sid().expect("user");
    let descriptor = security_descriptor(&user, &container).expect("descriptor");
    let duplicate = create_pipe(listener.address(), &descriptor);
    assert!(
        duplicate.is_err(),
        "the first instance flag must hold the name"
    );
}
