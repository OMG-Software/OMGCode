//! Holds a running Claude Code process to its recorded connection
//! (ADR-0109, as ADR-0061 does for runs).
//!
//! Before the process starts, the turn takes a shared lock on the
//! connection's turn lock file and checks every settings state since the
//! connection was resolved. While it runs, a watcher thread checks each later
//! state and ends the process tree when one withdraws the connection. The
//! session releases the lock only after the tree was observed ended, so a
//! revocation that waits for the exclusive lock sees the process gone.

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use omgcode_platform::process::CancelSignal;

use super::ClaudeCodeFailure;
use super::launch::failure;
use crate::control::{ClaudeCodeWatch, Recheck, WatchCursor};
use crate::provider::ProviderError;

/// How often the watcher checks whether the settings changed.
const WATCH_POLL: Duration = Duration::from_millis(100);

/// Take the connection's shared turn lock, then check every settings state
/// recorded since `watch` was resolved. The lock is taken first, so a change
/// recorded before a revocation's wait found the lock free is seen here.
///
/// # Errors
///
/// [`ClaudeCodeFailure::Withdrawn`] when a revocation holds the lock or a
/// state withdrew the connection, and [`ClaudeCodeFailure::Failed`] when the
/// lock file cannot be used.
pub(super) fn hold_turn(watch: &ClaudeCodeWatch) -> Result<(File, WatchCursor), ProviderError> {
    let lock = watch
        .lock_turn()
        .map_err(|_| failure(ClaudeCodeFailure::Failed))?
        .ok_or(failure(ClaudeCodeFailure::Withdrawn))?;
    let mut cursor = watch.cursor();
    match watch.recheck_waiting(&mut cursor) {
        Recheck::Allowed => Ok((lock, cursor)),
        Recheck::Busy | Recheck::Withdrawn(_) => Err(failure(ClaudeCodeFailure::Withdrawn)),
    }
}

/// The thread that ends a process tree once the settings withdraw it.
pub(super) struct Watcher {
    done: Arc<AtomicBool>,
    withdrawn: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Watch the settings from `cursor` on and raise `stop`, the process
    /// tree's cancel signal, when a state withdraws the connection.
    ///
    /// # Errors
    ///
    /// [`ClaudeCodeFailure::Failed`] when the thread cannot start.
    pub(super) fn start(
        watch: ClaudeCodeWatch,
        mut cursor: WatchCursor,
        stop: CancelSignal,
    ) -> Result<Self, ProviderError> {
        let done = Arc::new(AtomicBool::new(false));
        let withdrawn = Arc::new(AtomicBool::new(false));
        let thread = {
            let done = Arc::clone(&done);
            let withdrawn = Arc::clone(&withdrawn);
            thread::Builder::new()
                .name("claude-code-watch".into())
                .spawn(move || {
                    while !done.load(Ordering::SeqCst) {
                        // Busy settings are checked again on the next poll.
                        if let Recheck::Withdrawn(_) = watch.recheck(&mut cursor) {
                            // Set before the tree ends, so a session that sees
                            // the process gone also sees why.
                            withdrawn.store(true, Ordering::SeqCst);
                            stop.cancel();
                            return;
                        }
                        thread::park_timeout(WATCH_POLL);
                    }
                    // A state recorded after the last poll still counts, so
                    // the turn's result is accepted only once every state up
                    // to the stop was checked. Busy settings fail closed.
                    if !matches!(watch.recheck_waiting(&mut cursor), Recheck::Allowed) {
                        withdrawn.store(true, Ordering::SeqCst);
                        stop.cancel();
                    }
                })
                .map_err(|_| failure(ClaudeCodeFailure::Failed))?
        };
        Ok(Self {
            done,
            withdrawn,
            thread: Some(thread),
        })
    }

    /// Whether the settings withdrew the connection.
    pub(super) fn withdrawn(&self) -> bool {
        self.withdrawn.load(Ordering::SeqCst)
    }

    /// Stop watching and join the thread, after its final check of the
    /// settings. A thread that panicked counts as a withdrawal, so the turn
    /// fails closed.
    pub(super) fn stop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            if thread.join().is_err() {
                self.withdrawn.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}
