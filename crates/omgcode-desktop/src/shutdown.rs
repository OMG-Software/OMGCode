//! Bounded, idempotent shutdown.
//!
//! Closing `main` (or any other exit path) must not freeze the UI thread for
//! the service's grace period. The first request hides both windows at once,
//! then runs [`DesktopService::shutdown`] on a separate thread and exits the
//! application. A request that arrives while shutdown runs does nothing.
//!
//! A streamed model request is interrupted by the cancel request, so its turn
//! ends inside the grace period (ADR-0050). A turn still waiting when the grace
//! period ends is abandoned: a request that is not streamed cannot be
//! interrupted. Its journal keeps an open turn, which the operator can close
//! later with the interrupt command or the trusted CLI.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use omgcode_app::desktop::DesktopService;
use tauri::{AppHandle, Manager};

use crate::policy::WindowLabel;

/// How long running turns get to stop before the process exits anyway.
pub const GRACE: Duration = Duration::from_secs(5);

const IDLE: u8 = 0;
const RUNNING: u8 = 1;
const DONE: u8 = 2;

/// The shutdown progress, shared as Tauri state.
#[derive(Debug, Default)]
pub struct ShutdownGate {
    phase: AtomicU8,
}

impl ShutdownGate {
    /// Moves from idle to running. Returns `true` for exactly one caller.
    #[must_use]
    pub fn begin(&self) -> bool {
        self.phase
            .compare_exchange(IDLE, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Records that the bounded shutdown has finished.
    pub fn finish(&self) {
        self.phase.store(DONE, Ordering::Release);
    }

    /// Whether the bounded shutdown has finished, so an exit may proceed.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.phase.load(Ordering::Acquire) == DONE
    }
}

/// Starts the shutdown once. Later calls, from any thread, do nothing.
///
/// Both windows are hidden first so the user sees the application close at
/// once. The service shutdown and the final `exit(code)` run on one new thread.
pub fn begin(app: &AppHandle, code: i32) {
    let gate = app.state::<ShutdownGate>();
    if !gate.begin() {
        return;
    }
    for label in [WindowLabel::Main, WindowLabel::Approvals] {
        if let Some(window) = app.get_webview_window(label.as_str())
            && window.hide().is_err()
        {
            eprintln!("omgcode-desktop: could not hide a window during shutdown");
        }
    }
    let service = Arc::clone(&*app.state::<Arc<DesktopService>>());
    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("omgcode-shutdown".to_owned())
        .spawn(move || {
            let report = service.shutdown(GRACE);
            if report.still_running > 0 {
                eprintln!(
                    "omgcode-desktop: {} chat turn(s) were still waiting on the provider and \
                     were abandoned; interrupt each session to close its open turn",
                    report.still_running
                );
            }
            handle.state::<ShutdownGate>().finish();
            handle.exit(code);
        });
    if spawned.is_err() {
        // No thread: exit now rather than hang with hidden windows. Open turns stay open.
        eprintln!("omgcode-desktop: could not start the shutdown thread");
        gate.finish();
        app.exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_begin_wins_and_finish_is_observed() {
        // Given a fresh gate.
        let gate = ShutdownGate::default();
        assert!(!gate.is_done());

        // When shutdown is requested twice.
        assert!(gate.begin());
        assert!(!gate.begin());

        // Then it is not done until finished, and stays begun afterwards.
        assert!(!gate.is_done());
        gate.finish();
        assert!(gate.is_done());
        assert!(!gate.begin());
    }
}
