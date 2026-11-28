//! A confined child whose standard input and output stay live pipes.
//!
//! [`spawn_confined_streaming`](super::spawn_confined_streaming) starts the
//! program exactly as `run_confined` does. The platform launch code hands this
//! module the started tree, the caller's two pipe ends, and the standard-error
//! reader. A supervisor thread then runs the same `watch`, `settle` loop as a
//! blocking run: exit, deadline, tree limits, disk limit, cancellation, and
//! tree cleanup until the tree is observed empty.
//!
//! Ownership: the caller owns the stdin write end and the stdout read end once
//! it takes them; [`StreamingChild`] owns the supervisor thread, which owns
//! the tree and the standard-error reader thread's receiver. Nothing is shared
//! except the cancel flag and the "ended" flag, both atomics.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{
    AppliedConfinement, CancelSignal, CapturedOutput, DiskMonitor, MAX_DEADLINE, POLL_INTERVAL,
    ProcessError, ProcessLimits, ProcessTree, SUPERVISOR_SLACK, Termination, TreeUsage, receive,
    settle, spawn_reader, stop_tree, watch,
};

/// The platform's blocking write end of the child's standard input.
#[cfg(windows)]
pub(super) type NativeInput = std::fs::File;
/// The platform's blocking read end of the child's standard output.
#[cfg(windows)]
pub(super) type NativeOutput = std::fs::File;
/// The platform's blocking write end of the child's standard input.
#[cfg(not(windows))]
pub(super) type NativeInput = std::process::ChildStdin;
/// The platform's blocking read end of the child's standard output.
#[cfg(not(windows))]
pub(super) type NativeOutput = std::process::ChildStdout;

/// The write end of a streaming child's standard input.
///
/// Writes block while the pipe is full. Dropping it closes the pipe, which
/// the child sees as end of input. A write after the child ended fails with
/// a broken-pipe error.
#[derive(Debug)]
pub struct ChildInput(NativeInput);

impl Write for ChildInput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// The read end of a streaming child's standard output.
///
/// Reads block until the child writes or every writer has closed. It returns
/// `Ok(0)` at end of file, including when Windows reports the closed pipe as
/// broken. The pipe holds no unread data after the child and its descendants
/// are gone, so a reader always sees end of file once the tree has ended.
#[derive(Debug)]
pub struct ChildOutput(NativeOutput);

impl Read for ChildOutput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buffer) {
            // Windows reports a closed pipe as broken rather than end of file.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
            other => other,
        }
    }
}

/// How a streaming child ended. Every termination means the whole tree was
/// observed empty.
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct StreamingOutcome {
    pub termination: Termination,
    /// The confinement that applied to every process in the tree.
    pub confinement: AppliedConfinement,
    /// Standard error, retained up to `max_stderr_bytes`.
    pub stderr: CapturedOutput,
    pub elapsed: Duration,
    /// What a limited tree used; `None` when no tree limits applied.
    pub usage: Option<TreeUsage>,
}

/// One confined child with live standard input and output.
///
/// [`StreamingChild::finish`] is the explicit shutdown: it waits for the tree
/// to be observed empty and reports how it ended. `Drop` is only a fallback.
/// Dropping a process handle does not prove that the process stopped, so a
/// caller that needs that proof, or the outcome, must call `finish`. The
/// fallback raises cancellation and waits a bounded time for the supervisor;
/// it cannot report an unconfirmed cleanup and never panics.
#[derive(Debug)]
#[must_use = "call `finish` to stop the child and observe its tree empty"]
pub struct StreamingChild {
    stdin: Option<ChildInput>,
    stdout: Option<ChildOutput>,
    cancel: CancelSignal,
    ended: Arc<AtomicBool>,
    supervisor: Option<JoinHandle<Result<StreamingOutcome, ProcessError>>>,
    /// Bound on how long to wait for a supervisor that was told to stop.
    settle_bound: Duration,
}

impl StreamingChild {
    /// Take the write end of the child's standard input, once. Dropping it
    /// closes the pipe.
    pub fn take_stdin(&mut self) -> Option<ChildInput> {
        self.stdin.take()
    }

    /// Take the read end of the child's standard output, once.
    pub fn take_stdout(&mut self) -> Option<ChildOutput> {
        self.stdout.take()
    }

    /// A handle that terminates the child's tree at the supervisor's next
    /// poll when raised. The outcome then reports [`Termination::Cancelled`],
    /// unless the program had already exited or a limit applied.
    pub fn cancel_signal(&self) -> CancelSignal {
        self.cancel.clone()
    }

    /// Whether the supervisor observed the whole tree empty. It stays `false`
    /// when cleanup could not be confirmed.
    pub fn has_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// Close standard input if the caller still holds it, wait up to `grace`
    /// for the tree to end on its own, then terminate it; observe the tree
    /// empty, join the supervisor, and return the outcome.
    ///
    /// A tree that had to be terminated here reports
    /// [`Termination::Cancelled`] (or an earlier deadline or limit). `grace`
    /// is capped at the longest accepted run deadline. The wait for the
    /// supervisor is bounded by twice the limits' cleanup timeout plus a
    /// second.
    ///
    /// # Errors
    ///
    /// The supervision errors of `run_confined`, including
    /// [`ProcessError::CleanupUnconfirmed`] when the tree was not observed
    /// empty in time.
    pub fn finish(mut self, grace: Duration) -> Result<StreamingOutcome, ProcessError> {
        // Closing standard input is the polite request to end.
        drop(self.stdin.take());
        let wait = grace.min(MAX_DEADLINE);
        let until = Instant::now()
            .checked_add(wait)
            .unwrap_or_else(Instant::now);
        while !self.supervisor_done() && Instant::now() < until {
            thread::sleep(POLL_INTERVAL);
        }
        if !self.supervisor_done() {
            self.cancel.cancel();
        }
        let Some(supervisor) = self.supervisor.take() else {
            return Err(ProcessError::Supervision(io::Error::other(
                "the supervisor was already joined",
            )));
        };
        match join_bounded(supervisor, self.settle_bound) {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(ProcessError::Supervision(io::Error::other(
                "the supervisor thread panicked",
            ))),
            None => Err(ProcessError::CleanupUnconfirmed(
                "the supervisor did not finish in time",
            )),
        }
    }

    fn supervisor_done(&self) -> bool {
        self.has_ended() || self.supervisor.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for StreamingChild {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let Some(supervisor) = self.supervisor.take() else {
            return;
        };
        self.cancel.cancel();
        // The outcome and any error cannot be reported from `Drop`; `finish`
        // is the way to learn them. The supervisor terminates the tree, and a
        // tree that cannot be confirmed is left to its native kill-on-close.
        let _unreportable = join_bounded(supervisor, self.settle_bound);
    }
}

/// Wait for `handle` at most `bound`; `None` means it had not finished and is
/// left running.
fn join_bounded<T>(handle: JoinHandle<T>, bound: Duration) -> Option<thread::Result<T>> {
    let until = Instant::now()
        .checked_add(bound)
        .unwrap_or_else(Instant::now);
    while !handle.is_finished() {
        if Instant::now() >= until {
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
    Some(handle.join())
}

/// A started tree and the pipe ends the platform launch produced.
pub(super) struct Launched<T, S> {
    pub tree: T,
    pub stdin: NativeInput,
    pub stdout: NativeOutput,
    pub stderr: S,
    pub started: Instant,
    pub limits: ProcessLimits,
    pub applied: AppliedConfinement,
    pub disk: Option<DiskMonitor>,
}

/// Start the supervisor for a launched tree.
///
/// On failure the tree is terminated and observed empty before the error is
/// returned, except when the supervisor thread itself cannot start: the tree
/// then drops, which kills it (Job Object close on Windows, `Drop` on Linux),
/// and the error says so.
pub(super) fn start<T, S>(launched: Launched<T, S>) -> Result<StreamingChild, ProcessError>
where
    T: ProcessTree + Send + 'static,
    S: Read + Send + 'static,
{
    let Launched {
        mut tree,
        stdin,
        stdout,
        stderr,
        started,
        limits,
        applied,
        disk,
    } = launched;
    let stderr = match spawn_reader("omgcode-stderr", stderr, limits.max_stderr_bytes) {
        Ok(receiver) => receiver,
        Err(error) => {
            stop_tree(&mut tree, limits.cleanup_timeout)?;
            tree.cleanup_confirmed();
            return Err(ProcessError::Supervision(error));
        }
    };
    let cancel = CancelSignal::new();
    let ended = Arc::new(AtomicBool::new(false));
    let supervisor = {
        let cancel = cancel.clone();
        let ended = Arc::clone(&ended);
        thread::Builder::new()
            .name("omgcode-supervisor".to_owned())
            .spawn(move || {
                supervise_streaming(
                    tree, &stderr, started, limits, &cancel, applied, disk, &ended,
                )
            })
            .map_err(|error| {
                ProcessError::Supervision(io::Error::new(
                    error.kind(),
                    "the supervisor thread could not start; the tree was left to its kill-on-close",
                ))
            })?
    };
    Ok(StreamingChild {
        stdin: Some(ChildInput(stdin)),
        stdout: Some(ChildOutput(stdout)),
        cancel,
        ended,
        supervisor: Some(supervisor),
        settle_bound: limits
            .cleanup_timeout
            .saturating_mul(2)
            .saturating_add(SUPERVISOR_SLACK),
    })
}

/// The supervisor thread body: watch, then stop the tree and observe it empty.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site that moves each part into the thread"
)]
fn supervise_streaming(
    mut tree: impl ProcessTree,
    stderr: &Receiver<io::Result<CapturedOutput>>,
    started: Instant,
    limits: ProcessLimits,
    cancel: &CancelSignal,
    confinement: AppliedConfinement,
    disk: Option<DiskMonitor>,
    ended: &AtomicBool,
) -> Result<StreamingOutcome, ProcessError> {
    let watched = watch(&mut tree, started, limits.deadline, cancel, disk.as_ref());
    let result = settle(&mut tree, watched, started, limits, disk).and_then(|settled| {
        ended.store(true, Ordering::SeqCst);
        let stderr = receive(stderr, Instant::now() + limits.cleanup_timeout)?;
        Ok(StreamingOutcome {
            termination: settled.termination,
            confinement,
            stderr,
            elapsed: settled.elapsed,
            usage: settled.usage,
        })
    });
    if !matches!(result, Err(ProcessError::CleanupUnconfirmed(_))) {
        tree.cleanup_confirmed();
    }
    result
}
