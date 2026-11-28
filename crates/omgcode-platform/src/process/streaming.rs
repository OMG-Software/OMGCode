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
//! except the cancel flag and the "emptied" flag, both atomics.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{
    AppliedConfinement, CancelSignal, CapturedOutput, DISK_ACCOUNTING_BOUND, DiskMonitor,
    MAX_DEADLINE, POLL_INTERVAL, ProcessError, ProcessLimits, ProcessTree, SUPERVISOR_SLACK,
    Termination, TreeUsage, UnconfirmedCleanup, receive, settle, spawn_reader, stop_tree, watch,
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
    /// Set by the supervisor as soon as it observes the tree empty, before the
    /// usage, final disk accounting, and standard-error capture.
    emptied: Arc<AtomicBool>,
    supervisor: Option<JoinHandle<Result<StreamingOutcome, ProcessError>>>,
    /// Bound on how long to wait for a supervisor that was told to stop, up to
    /// the tree being observed empty.
    settle_bound: Duration,
    /// Further bound, counted from the tree being observed empty, for the
    /// final disk accounting and the standard-error capture.
    accounting_bound: Duration,
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
    /// when cleanup could not be confirmed, and turns `true` before the usage,
    /// final disk accounting, and standard-error capture, also when one of
    /// those then fails.
    pub fn has_ended(&self) -> bool {
        self.emptied.load(Ordering::SeqCst)
    }

    /// A handle that reports [`Self::has_ended`] also after `finish` or
    /// `Drop` consumed the child, so a caller can tell an accounting failure
    /// after an observed end from an unconfirmed cleanup.
    pub fn tree_end(&self) -> TreeEnd {
        TreeEnd(Arc::clone(&self.emptied))
    }

    /// Close standard input if the caller still holds it, wait up to `grace`
    /// for the tree to end on its own, then terminate it; observe the tree
    /// empty, join the supervisor, and return the outcome.
    ///
    /// A tree that had to be terminated here reports
    /// [`Termination::Cancelled`] (or an earlier deadline or limit). `grace`
    /// is capped at the longest accepted run deadline. The wait for the
    /// supervisor is bounded by twice the limits' cleanup timeout plus a
    /// second. Once the tree is observed empty, the final disk accounting and
    /// standard-error capture get their own further bound, so a slow disk
    /// scan is reported as a supervision failure, not as unconfirmed cleanup.
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
        match join_supervisor(
            supervisor,
            &self.emptied,
            self.settle_bound,
            self.accounting_bound,
        ) {
            Joined::Finished(Ok(result)) => result,
            Joined::Finished(Err(_)) => Err(ProcessError::Supervision(io::Error::other(
                "the supervisor thread panicked",
            ))),
            Joined::CleanupTimedOut => Err(ProcessError::CleanupUnconfirmed(
                UnconfirmedCleanup::new("the supervisor did not finish in time"),
            )),
            Joined::AccountingTimedOut => Err(ProcessError::Supervision(io::Error::other(
                "the tree was observed empty, but the final accounting did not finish in time",
            ))),
        }
    }

    fn supervisor_done(&self) -> bool {
        self.has_ended() || self.supervisor.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

/// Whether the supervisor of one [`StreamingChild`] observed its whole tree
/// empty, readable after the child is gone.
#[derive(Clone, Debug)]
pub struct TreeEnd(Arc<AtomicBool>);

impl TreeEnd {
    /// `true` once the supervisor observed the whole tree empty. It never
    /// turns `false` again.
    pub fn observed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
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

/// How waiting for the supervisor thread ended.
enum Joined<T> {
    /// The thread ended, with its result or its panic.
    Finished(thread::Result<T>),
    /// The tree was not observed empty within the cleanup bound.
    CleanupTimedOut,
    /// The tree was observed empty, but the accounting after it did not end
    /// within its own bound.
    AccountingTimedOut,
}

/// Wait for the supervisor: at most `cleanup_bound` for it to observe the tree
/// empty, then at most `accounting_bound` more, counted from that observation
/// and never shortening the first bound, for what follows. The thread is left
/// running when a bound passes.
fn join_supervisor<T>(
    handle: JoinHandle<T>,
    emptied: &AtomicBool,
    cleanup_bound: Duration,
    accounting_bound: Duration,
) -> Joined<T> {
    let mut until = Instant::now()
        .checked_add(cleanup_bound)
        .unwrap_or_else(Instant::now);
    let mut counted = false;
    while !handle.is_finished() {
        let now = Instant::now();
        let observed = emptied.load(Ordering::SeqCst);
        if observed && !counted {
            counted = true;
            if let Some(later) = now.checked_add(accounting_bound) {
                until = until.max(later);
            }
        }
        if now >= until {
            return if observed {
                Joined::AccountingTimedOut
            } else {
                Joined::CleanupTimedOut
            };
        }
        thread::sleep(POLL_INTERVAL);
    }
    Joined::Finished(handle.join())
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
/// returned, including when the supervisor thread cannot start: the tree owner
/// stays here until the thread has started, and a failed start terminates and
/// reaps it synchronously. A tree that cannot be observed empty is reported as
/// [`ProcessError::CleanupUnconfirmed`] and left to its drop fallback.
pub(super) fn start<T, S>(launched: Launched<T, S>) -> Result<StreamingChild, ProcessError>
where
    T: ProcessTree + Send + 'static,
    S: Read + Send + 'static,
{
    let has_disk = launched.disk.is_some();
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
        Err(error) => return Err(terminate_unsupervised(&mut tree, limits, error)),
    };
    let cancel = CancelSignal::new();
    let emptied = Arc::new(AtomicBool::new(false));
    // The thread receives the tree only after it exists, so a failed start
    // leaves the owner here to terminate and reap it.
    let (hand_over, received) = mpsc::sync_channel::<T>(1);
    let spawned = {
        let cancel = cancel.clone();
        let emptied = Arc::clone(&emptied);
        thread::Builder::new()
            .name("omgcode-supervisor".to_owned())
            .spawn(move || {
                let Ok(tree) = received.recv() else {
                    return Err(ProcessError::Supervision(io::Error::other(
                        "the supervisor never received the tree",
                    )));
                };
                supervise_streaming(
                    tree, &stderr, started, limits, &cancel, applied, disk, &emptied,
                )
            })
    };
    let supervisor = match spawned {
        Ok(supervisor) => supervisor,
        Err(error) => return Err(terminate_unsupervised(&mut tree, limits, error)),
    };
    if let Err(unsent) = hand_over.send(tree) {
        // The thread ended before it took the tree, so nothing supervises it.
        let mut tree = unsent.0;
        return Err(terminate_unsupervised(
            &mut tree,
            limits,
            io::Error::other("the supervisor thread ended before it took the tree"),
        ));
    }
    Ok(StreamingChild {
        stdin: Some(ChildInput(stdin)),
        stdout: Some(ChildOutput(stdout)),
        cancel,
        emptied,
        supervisor: Some(supervisor),
        settle_bound: limits
            .cleanup_timeout
            .saturating_mul(2)
            .saturating_add(SUPERVISOR_SLACK),
        accounting_bound: if has_disk {
            DISK_ACCOUNTING_BOUND
        } else {
            Duration::ZERO
        },
    })
}

/// Terminate and reap a tree no supervisor owns. A tree observed empty is
/// confirmed and `cause` is reported as a supervision failure; otherwise the
/// unconfirmed cleanup is returned and the tree's drop fallback stays armed.
fn terminate_unsupervised(
    tree: &mut impl ProcessTree,
    limits: ProcessLimits,
    cause: io::Error,
) -> ProcessError {
    match stop_tree(tree, limits.cleanup_timeout) {
        Ok(()) => {
            tree.cleanup_confirmed();
            ProcessError::Supervision(cause)
        }
        Err(unconfirmed) => unconfirmed,
    }
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
    emptied: &AtomicBool,
) -> Result<StreamingOutcome, ProcessError> {
    let watched = watch(&mut tree, started, limits.deadline, cancel, disk.as_ref());
    let settled = settle(&mut tree, watched, started, limits, disk, || {
        emptied.store(true, Ordering::SeqCst);
    });
    let result = settled.and_then(|settled| {
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

#[cfg(test)]
mod supervision_tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{StreamingChild, supervise_streaming};
    use crate::process::{
        AppliedConfinement, CancelSignal, CapturedOutput, ProcessError, ProcessLimits, ProcessTree,
        Termination, TreeUsage,
    };

    /// A tree that exited and is empty at once, whose usage `fails`.
    struct ExitedTree {
        usage_fails: bool,
        empty: bool,
    }

    impl ProcessTree for ExitedTree {
        fn try_exit(&mut self) -> io::Result<Option<Termination>> {
            Ok(Some(Termination::Exited { code: 0 }))
        }

        fn terminate(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn is_empty(&mut self) -> io::Result<bool> {
            Ok(self.empty)
        }

        fn usage(&mut self) -> io::Result<Option<TreeUsage>> {
            if self.usage_fails {
                Err(io::Error::other("usage failed"))
            } else {
                Ok(None)
            }
        }
    }

    /// `tree` under a supervisor, once the supervisor has returned.
    fn supervised(tree: ExitedTree) -> StreamingChild {
        let limits = ProcessLimits {
            cleanup_timeout: Duration::from_millis(100),
            ..ProcessLimits::default()
        };
        let (sender, stderr) = mpsc::channel();
        sender
            .send(Ok(CapturedOutput::default()))
            .expect("standard error");
        let cancel = CancelSignal::new();
        let emptied = Arc::new(AtomicBool::new(false));
        let supervisor = {
            let cancel = cancel.clone();
            let emptied = Arc::clone(&emptied);
            thread::spawn(move || {
                supervise_streaming(
                    tree,
                    &stderr,
                    Instant::now(),
                    limits,
                    &cancel,
                    AppliedConfinement::Unconfined,
                    None,
                    &emptied,
                )
            })
        };
        let until = Instant::now() + Duration::from_secs(10);
        while !supervisor.is_finished() {
            assert!(Instant::now() < until, "the supervisor did not return");
            thread::sleep(Duration::from_millis(10));
        }
        StreamingChild {
            stdin: None,
            stdout: None,
            cancel,
            emptied,
            supervisor: Some(supervisor),
            settle_bound: Duration::from_secs(5),
            accounting_bound: Duration::ZERO,
        }
    }

    #[test]
    fn a_tree_observed_empty_has_ended_though_its_accounting_failed() {
        let child = supervised(ExitedTree {
            usage_fails: true,
            empty: true,
        });
        assert!(child.has_ended());
        let end = child.tree_end();
        let error = child.finish(Duration::ZERO).expect_err("usage failed");
        assert!(matches!(error, ProcessError::Supervision(_)), "{error:?}");
        assert!(end.observed(), "the end outlives the child");
    }

    #[test]
    fn a_tree_never_observed_empty_has_not_ended() {
        let child = supervised(ExitedTree {
            usage_fails: false,
            empty: false,
        });
        assert!(!child.has_ended());
        let end = child.tree_end();
        let error = child.finish(Duration::ZERO).expect_err("never empty");
        assert!(
            matches!(error, ProcessError::CleanupUnconfirmed(_)),
            "{error:?}"
        );
        assert!(!end.observed());
    }

    #[test]
    fn a_tree_that_ended_cleanly_has_ended() {
        let child = supervised(ExitedTree {
            usage_fails: false,
            empty: true,
        });
        assert!(child.has_ended());
        let outcome = child.finish(Duration::ZERO).expect("outcome");
        assert_eq!(outcome.termination, Termination::Exited { code: 0 });
    }
}
