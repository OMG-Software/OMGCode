//! Turn an operator interrupt (Ctrl-C) into a run cancellation.
//!
//! A supervised tree runs in its own process group on Linux and in a job on
//! Windows, so a terminal interrupt does not reach it directly. Instead, the
//! handler installed here records the interrupt, and a watcher thread raises
//! the run's [`CancelSignal`]. The runner then terminates the whole tree and
//! reports `Cancelled`, so the caller can still record the outcome.
//! [`watch_interrupt`] calls any closure instead, so a CLI chat turn or
//! compaction can cancel its own token the same way (ADR-0051).
//!
//! Linux handles `SIGINT` and `SIGTERM`; Windows handles `CTRL_C_EVENT` and
//! `CTRL_BREAK_EVENT`. The handler stays installed for the life of the
//! process, so later interrupts are absorbed the same way. Each watcher is
//! owned by an [`InterruptWatch`], which stops and joins it. A forced kill,
//! such as `SIGKILL` or closing the console, still ends the caller without an
//! outcome record.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::process::CancelSignal;

/// Set by the native handler. Only an atomic store happens in the handler.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// How often the watcher checks for a recorded interrupt.
const WATCH_INTERVAL: Duration = Duration::from_millis(20);

/// Install the interrupt handler and raise `cancel` when an interrupt arrives.
///
/// The returned watch owns the watcher thread, which holds a clone of
/// `cancel`. Call [`InterruptWatch::stop`] when the run ends; dropping the
/// watch does the same. If the handler cannot be installed, nothing is
/// started and the error is returned; the caller decides whether to run
/// without interrupt handling.
pub fn cancel_on_interrupt(cancel: &CancelSignal) -> io::Result<InterruptWatch> {
    let cancel = cancel.clone();
    watch_interrupt(move || cancel.cancel())
}

/// Install the interrupt handler and call `on_interrupt` once when an
/// interrupt arrives.
///
/// This is [`cancel_on_interrupt`] for a caller whose cancellation is not a
/// [`CancelSignal`], such as a chat turn's cancel token (ADR-0051).
/// `on_interrupt` runs on the watcher thread, at most once, and must not
/// block. An interrupt that arrived before this call, while the handler was
/// already installed, is seen at once, because the recorded interrupt is
/// never cleared. The returned watch owns the thread and the closure; stop
/// it as described for [`cancel_on_interrupt`]. If the handler cannot be
/// installed or the thread cannot be started, `on_interrupt` is dropped
/// unused and the error is returned.
pub fn watch_interrupt(on_interrupt: impl FnOnce() + Send + 'static) -> io::Result<InterruptWatch> {
    install()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("omgcode-interrupt".to_owned())
        .spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    on_interrupt();
                    return;
                }
                thread::sleep(WATCH_INTERVAL);
            }
        })?;
    Ok(InterruptWatch {
        stop,
        thread: Some(thread),
    })
}

/// An interrupt watcher for one run. The handler itself stays installed.
#[must_use = "dropping the watch stops it at once"]
#[derive(Debug)]
pub struct InterruptWatch {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl InterruptWatch {
    /// Stop the watcher and wait for its thread, which ends within one poll
    /// interval. An error means the thread panicked.
    pub fn stop(mut self) -> io::Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| io::Error::other("the interrupt watcher panicked")),
            None => Ok(()),
        }
    }
}

impl Drop for InterruptWatch {
    fn drop(&mut self) {
        // Fallback when `stop` was not called: the join cannot block for
        // longer than one poll interval, and a panic in the thread is
        // already reported on its own.
        let _ = self.finish();
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn install() -> io::Result<()> {
    extern "C" fn on_signal(_signal: libc::c_int) {
        // Only an async-signal-safe atomic store happens here.
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: a zeroed `sigaction` is a valid starting value; the mask is
        // initialised with `sigemptyset` before use.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: `action.sa_mask` is a valid, writable signal set.
        if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `action` is fully initialised, the handler only performs an
        // atomic store, and a null old-action pointer is allowed.
        if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn install() -> io::Result<()> {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: `on_console_event` has the handler signature and stays valid
    // for the life of the process.
    if unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Absorb Ctrl-C and Ctrl-Break; let other events, such as closing the
/// console, take their default action.
#[cfg(windows)]
extern "system" fn on_console_event(event: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
        INTERRUPTED.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}

#[cfg(not(any(unix, windows)))]
fn install() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "interrupt handling is implemented only on Unix and Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// An interrupt delivered to this process raises the run's cancel signal.
    /// This is the only test that touches the process-wide handler.
    #[test]
    fn an_interrupt_raises_the_cancel_signal() {
        let idle = CancelSignal::new();
        let watch = cancel_on_interrupt(&idle).expect("install the handler");
        // Stopping a watcher ends its thread without raising its signal.
        watch.stop().expect("stop the idle watcher");
        assert!(!idle.is_cancelled());
        let cancel = CancelSignal::new();
        let watch = cancel_on_interrupt(&cancel).expect("install the handler");
        assert!(!cancel.is_cancelled());
        deliver_interrupt();
        let until = Instant::now() + Duration::from_secs(5);
        while !cancel.is_cancelled() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(cancel.is_cancelled(), "the watcher did not cancel the run");
        watch.stop().expect("stop the finished watcher");
        assert!(!idle.is_cancelled(), "a stopped watcher raised its signal");

        // The recorded interrupt is never cleared, so a callback watcher
        // started afterwards is called at once, exactly once.
        let (called, calls) = std::sync::mpsc::channel();
        let watch = watch_interrupt(move || {
            // The receiver outlives the bounded wait below; a failed send
            // would show up there as a missing call.
            let _ignored = called.send(());
        })
        .expect("start a callback watcher");
        calls
            .recv_timeout(Duration::from_secs(5))
            .expect("the callback watcher saw the earlier interrupt");
        watch.stop().expect("stop the callback watcher");
        assert!(calls.try_recv().is_err(), "the callback ran more than once");
    }

    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn deliver_interrupt() {
        // SAFETY: the handler for SIGINT was installed above and only stores
        // an atomic flag.
        assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0);
    }

    /// Generating a real console event would reach every process on the test
    /// console, so call the installed handler directly.
    #[cfg(windows)]
    fn deliver_interrupt() {
        use windows_sys::Win32::System::Console::CTRL_C_EVENT;
        assert_eq!(on_console_event(CTRL_C_EVENT), 1);
    }
}
