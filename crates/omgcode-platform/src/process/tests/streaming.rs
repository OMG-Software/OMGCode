//! Streaming confined-child tests. The fixture is the test binary itself
//! (see `process::tests::fixture`), run under the same confinement the
//! platform's confinement tests use: a real AppContainer on Windows, Landlock
//! and seccomp on Linux. Each test that needs a live child proves it with a
//! round trip or a recorded process id, never with a sleep alone.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::super::{
    AppliedConfinement, ChildOutput, Confinement, NetworkAccess, ProcessError, ProcessLimits,
    ProcessSpec, StreamingChild, Termination, spawn_confined_streaming,
};
#[cfg(target_os = "linux")]
use super::confinement::Scene;
#[cfg(windows)]
use super::windows_confinement::Scene;
use super::{
    FIXTURE_ENV, FLOOD_BYTES, KillOnDrop, RECORD_ENV, exits_within, limits, record_pid, text,
};

/// Longest a test waits for something that must happen.
const PATIENCE: Duration = Duration::from_secs(30);

#[cfg(windows)]
fn confinement(scene: &Scene) -> Confinement {
    scene.confinement()
}

#[cfg(target_os = "linux")]
fn confinement(scene: &Scene) -> Confinement {
    use super::super::UnixSocketAccess;
    scene.confinement(UnixSocketAccess::PairsOnly, NetworkAccess::Denied)
}

/// The scene's program in fixture mode `mode`, recording to `record` when it
/// writes one.
fn fixture(scene: &Scene, mode: &str, record: &Path) -> ProcessSpec {
    let mut spec = scene.spec();
    for (name, value) in &mut spec.environment {
        if name == FIXTURE_ENV {
            *value = OsString::from(mode);
        }
    }
    spec.environment
        .push((OsString::from(RECORD_ENV), record.as_os_str().to_owned()));
    spec
}

fn start(scene: &Scene, mode: &str, limits: &ProcessLimits) -> StreamingChild {
    let record = scene.workspace.join("record.txt");
    spawn_confined_streaming(&fixture(scene, mode, &record), &confinement(scene), limits)
        .expect("start streaming child")
}

/// Forward the child's stdout lines from a helper thread, so a test never
/// blocks on a read. The channel closes when the pipe reaches end of file.
fn lines(output: ChildOutput) -> Receiver<String> {
    lines_with_handle(output).1
}

/// [`lines`] plus the reader thread's handle. The thread returns `Ok` only
/// when the pipe reached end of file; a read error is its `Err`, so a closed
/// channel can be told apart from a failed read.
fn lines_with_handle(output: ChildOutput) -> (JoinHandle<io::Result<()>>, Receiver<String>) {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if sender.send(line?).is_err() {
                // The test stopped listening; nothing more to read for it.
                return Ok(());
            }
        }
        Ok(())
    });
    (handle, receiver)
}

/// Wait for exactly `wanted`, skipping other lines such as the test harness's
/// own output.
fn expect_line(lines: &Receiver<String>, wanted: &str) {
    let until = Instant::now() + PATIENCE;
    loop {
        let wait = until.saturating_duration_since(Instant::now());
        match lines.recv_timeout(wait) {
            Ok(line) if line == wanted => return,
            Ok(_) => {}
            Err(error) => panic!("no line {wanted:?} arrived: {error}"),
        }
    }
}

/// The pid the fixture recorded, once it exists.
fn recorded_pid(scene: &Scene) -> u32 {
    let record = scene.workspace.join("record.txt");
    let until = Instant::now() + PATIENCE;
    loop {
        if let Ok(contents) = fs::read_to_string(&record) {
            return record_pid(&contents);
        }
        assert!(Instant::now() < until, "the fixture never recorded its pid");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_until_ended(child: &StreamingChild) {
    let until = Instant::now() + PATIENCE;
    while !child.has_ended() {
        assert!(
            Instant::now() < until,
            "the supervisor never saw the tree end"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn replies_arrive_before_stdin_closes_and_eof_follows_the_close() {
    let scene = Scene::new();
    let mut child = start(&scene, "stdin-echo", &limits());
    let mut stdin = child.take_stdin().expect("stdin");
    let reader = lines(child.take_stdout().expect("stdout"));
    assert!(child.take_stdin().is_none() && child.take_stdout().is_none());

    for word in ["alpha", "beta"] {
        writeln!(stdin, "{word}").expect("write line");
        stdin.flush().expect("flush line");
        // The reply comes while stdin is still open: the child is live.
        expect_line(&reader, &format!("echo:{word}"));
        assert!(!child.has_ended(), "the child ended with stdin open");
    }
    // Closing stdin is the only thing that lets the child reach end of file;
    // a leaked write end inside the child would keep it open forever.
    drop(stdin);
    expect_line(&reader, "eof");
    wait_until_ended(&child);
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    assert!(text(&outcome.stderr.bytes).contains("echo-ready"));
}

#[test]
fn finish_after_a_natural_exit_reports_exited_and_the_streams() {
    let scene = Scene::new();
    let mut child = start(&scene, "echo", &limits());
    let mut stdout = child.take_stdout().expect("stdout");
    let mut printed = Vec::new();
    stdout
        .read_to_end(&mut printed)
        .expect("read stdout to end");
    assert!(text(&printed).contains("out-marker"));
    wait_until_ended(&child);
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::Exited { code: 7 });
    assert!(text(&outcome.stderr.bytes).contains("err-marker"));
    assert!(matches!(
        outcome.confinement,
        AppliedConfinement::AppContainer { .. } | AppliedConfinement::Landlock { .. }
    ));
}

#[test]
fn cancelling_terminates_the_tree_and_ends_stdout() {
    let scene = Scene::new();
    let mut child = start(&scene, "ready-sleep", &limits());
    let pid = recorded_pid(&scene);
    let guard = KillOnDrop(pid);
    let (reader_thread, reader) = lines_with_handle(child.take_stdout().expect("stdout"));
    child.cancel_signal().cancel();
    wait_until_ended(&child);
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::Cancelled);
    assert!(exits_within(pid, Duration::from_secs(5)), "{pid} survived");
    // The pipe reaches end of file once the tree is gone. A disconnected
    // channel counts as end of file only if the reader thread ended without a
    // read error.
    let until = Instant::now() + PATIENCE;
    while reader.recv_timeout(Duration::from_millis(50))
        != Err(mpsc::RecvTimeoutError::Disconnected)
    {
        assert!(Instant::now() < until, "stdout never reached end of file");
    }
    while !reader_thread.is_finished() {
        assert!(Instant::now() < until, "the stdout reader never finished");
        thread::sleep(Duration::from_millis(10));
    }
    reader_thread
        .join()
        .expect("the stdout reader panicked")
        .expect("stdout ended with a read error, not end of file");
    drop(guard);
}

#[test]
fn the_deadline_terminates_the_tree() {
    let scene = Scene::new();
    let limits = ProcessLimits {
        deadline: Duration::from_millis(1500),
        ..limits()
    };
    let child = start(&scene, "ready-sleep", &limits);
    let pid = recorded_pid(&scene);
    let guard = KillOnDrop(pid);
    wait_until_ended(&child);
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::DeadlineExceeded);
    assert!(exits_within(pid, Duration::from_secs(5)), "{pid} survived");
    drop(guard);
}

#[test]
fn a_child_that_ignores_the_closed_stdin_is_terminated_after_the_grace() {
    let scene = Scene::new();
    let child = start(&scene, "ready-sleep", &limits());
    let pid = recorded_pid(&scene);
    let guard = KillOnDrop(pid);
    let started = Instant::now();
    let outcome = child.finish(Duration::from_millis(300)).expect("finish");
    assert_eq!(outcome.termination, Termination::Cancelled);
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(exits_within(pid, Duration::from_secs(5)), "{pid} survived");
    drop(guard);
}

#[test]
fn dropping_the_child_terminates_the_tree() {
    let scene = Scene::new();
    let child = start(&scene, "ready-sleep", &limits());
    let pid = recorded_pid(&scene);
    let guard = KillOnDrop(pid);
    drop(child);
    assert!(
        exits_within(pid, Duration::from_secs(5)),
        "{pid} outlived the dropped child"
    );
    drop(guard);
}

#[test]
fn descendants_never_outlive_a_streaming_child() {
    let scene = Scene::new();
    let mut child = start(&scene, "grandchild", &limits());
    let record = scene.workspace.join("record.txt");
    let _stdout = child.take_stdout();
    wait_until_ended(&child);
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    let contents = fs::read_to_string(&record).expect("grandchild record");
    let pid = record_pid(&contents);
    let guard = KillOnDrop(pid);
    assert!(contents.contains("escape=denied"), "{contents}");
    assert!(exits_within(pid, Duration::from_secs(5)), "{pid} survived");
    drop(guard);
}

#[test]
fn standard_error_is_bounded_and_the_rest_drained() {
    let scene = Scene::new();
    let limits = ProcessLimits {
        max_stderr_bytes: 1024,
        ..limits()
    };
    let mut child = start(&scene, "flood-stderr", &limits);
    let mut stdout = child.take_stdout().expect("stdout");
    let mut ignored = Vec::new();
    stdout
        .read_to_end(&mut ignored)
        .expect("read stdout to end");
    wait_until_ended(&child);
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    assert_eq!(outcome.stderr.bytes.len(), 1024);
    assert!(outcome.stderr.truncated());
    assert!(outcome.stderr.total_bytes >= u64::try_from(FLOOD_BYTES).expect("size"));
}

/// The network mode reaches the child's confinement and nothing is weakened:
/// on Linux, `HostServices` together with `Internet` is accepted and applied.
#[test]
fn internet_access_is_applied_to_a_streaming_child() {
    let scene = Scene::new();
    let mut confinement = confinement(&scene);
    confinement.network = NetworkAccess::Internet;
    #[cfg(target_os = "linux")]
    {
        use super::super::{UnixSocketAccess, network_support_paths};
        confinement.unix_sockets = UnixSocketAccess::HostServices;
        confinement
            .read_only
            .extend(network_support_paths().expect("network support paths"));
    }
    let record = scene.workspace.join("record.txt");
    let child =
        spawn_confined_streaming(&fixture(&scene, "echo", &record), &confinement, &limits())
            .expect("start streaming child");
    wait_until_ended(&child);
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::Exited { code: 7 });
    #[cfg(windows)]
    assert_eq!(
        outcome.confinement,
        AppliedConfinement::AppContainer {
            network: NetworkAccess::Internet
        }
    );
    #[cfg(target_os = "linux")]
    assert!(
        matches!(
            outcome.confinement,
            AppliedConfinement::Landlock {
                unix_sockets: super::super::UnixSocketAccess::HostServices,
                network: NetworkAccess::Internet,
                ..
            }
        ),
        "{:?}",
        outcome.confinement
    );
}

#[test]
fn a_streaming_start_with_a_bad_specification_starts_nothing() {
    let scene = Scene::new();
    let mut spec = scene.spec();
    spec.program = Path::new("relative.exe").to_path_buf();
    let error = spawn_confined_streaming(&spec, &confinement(&scene), &limits())
        .expect_err("relative program");
    assert!(matches!(error, ProcessError::InvalidSpec(_)), "{error}");
}

#[test]
fn a_streaming_start_ignores_the_stdout_limit_but_checks_the_stderr_limit() {
    let scene = Scene::new();
    let record = scene.workspace.join("record.txt");
    let spec = fixture(&scene, "ready-sleep", &record);
    // Standard output is read by the caller, so its capture limit is unused.
    let mut oversized_stdout = limits();
    oversized_stdout.max_stdout_bytes = usize::MAX;
    let child = spawn_confined_streaming(&spec, &confinement(&scene), &oversized_stdout)
        .expect("an out-of-range stdout limit is ignored");
    let pid = recorded_pid(&scene);
    let guard = KillOnDrop(pid);
    child.cancel_signal().cancel();
    let outcome = child.finish(Duration::from_secs(5)).expect("finish");
    assert_eq!(outcome.termination, Termination::Cancelled);
    drop(guard);
    // The standard-error limit still applies.
    let mut oversized_stderr = limits();
    oversized_stderr.max_stderr_bytes = usize::MAX;
    let error = spawn_confined_streaming(&spec, &confinement(&scene), &oversized_stderr)
        .expect_err("an out-of-range stderr limit");
    assert!(matches!(error, ProcessError::InvalidLimits(_)), "{error}");
}

/// A tree whose native calls fail, to prove that a failure before the tree is
/// seen empty is unconfirmed cleanup with its cause, not a supervision failure.
struct FailingTree {
    terminate_fails: bool,
    confirmed: bool,
}

impl super::super::ProcessTree for FailingTree {
    fn try_exit(&mut self) -> io::Result<Option<Termination>> {
        Ok(None)
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.terminate_fails {
            Err(io::Error::other("terminate failed"))
        } else {
            Ok(())
        }
    }

    fn is_empty(&mut self) -> io::Result<bool> {
        Err(io::Error::other("emptiness check failed"))
    }

    fn cleanup_confirmed(&mut self) {
        self.confirmed = true;
    }
}

#[test]
fn a_failure_before_the_tree_is_seen_empty_is_unconfirmed_cleanup_with_its_cause() {
    for (terminate_fails, cause) in [
        (true, "terminate failed"),
        (false, "emptiness check failed"),
    ] {
        let mut tree = FailingTree {
            terminate_fails,
            confirmed: false,
        };
        let error = super::super::stop_tree(&mut tree, Duration::from_millis(100))
            .expect_err("the tree was never seen empty");
        let ProcessError::CleanupUnconfirmed(cleanup) = &error else {
            panic!("expected unconfirmed cleanup, got {error:?}");
        };
        assert_eq!(
            cleanup.cause().map(ToString::to_string).as_deref(),
            Some(cause)
        );
        assert!(!tree.confirmed, "a failed stop must not confirm cleanup");
    }
}
