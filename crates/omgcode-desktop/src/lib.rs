//! OMGCode desktop shell (Tauri 2).
//!
//! The Rust side is the only authority. Both webviews render untrusted text and
//! reach Rust only through commands that are (1) gated by a per-window Tauri
//! capability and (2) checked again against the window policy in
//! [`policy`]. See ADR-0045 and ADR-0046.
//!
//! The data root and the model endpoint come from the process command line
//! ([`launch`]) and nowhere else.

pub mod commands;
pub mod error;
pub mod launch;
pub mod policy;
pub mod shutdown;
pub mod windows;

use std::process::ExitCode;
use std::sync::Arc;

use omgcode_app::desktop::{DesktopConfig, DesktopService};
use omgcode_app::{ChatClients, ChatSettings};
use tauri::{Manager, RunEvent};

use crate::launch::{LaunchArgs, USAGE};
use crate::shutdown::ShutdownGate;

/// Exit code for a command line that was not understood.
const EXIT_USAGE: u8 = 2;

/// A context size that is valid for any model, used only to validate the endpoint.
const PROBE_CONTEXT_TOKENS: u64 = 8192;

/// Builds and runs the application.
///
/// Exit codes: `0` after a normal close, `2` for a command-line usage error,
/// `1` for any other startup failure (endpoint rejected, data root or index
/// unusable, runtime could not start). Every failure happens before a window
/// opens and prints a fixed or provider message to stderr. In a release build
/// on Windows the process has no console, so stderr is visible only when it is
/// redirected.
#[must_use]
pub fn run() -> ExitCode {
    let Ok(args) = launch::parse_args(std::env::args_os().skip(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::from(EXIT_USAGE);
    };
    let service = match open_service(args) {
        Ok(service) => Arc::new(service),
        Err(message) => {
            eprintln!("omgcode-desktop: {message}");
            return ExitCode::FAILURE;
        }
    };
    run_app(service)
}

/// Validates the endpoint and opens the one service for this process.
fn open_service(args: LaunchArgs) -> Result<DesktopService, String> {
    // Validate the endpoint the way a chat turn would, so a malformed or
    // non-loopback endpoint fails now, with the provider's message. The clients
    // are discarded; no request is made and no token is read.
    let probe = ChatSettings::new(
        args.endpoint.clone(),
        "endpoint-check",
        PROBE_CONTEXT_TOKENS,
    );
    ChatClients::new(&probe, None).map_err(|error| error.to_string())?;
    let data_root = omgcode_app::data_root(args.data_dir).map_err(|error| error.to_string())?;
    DesktopService::open(DesktopConfig {
        data_root,
        endpoint: args.endpoint,
    })
    .map_err(|error| format!("{}: {}", error.code(), error.message()))
}

fn run_app(service: Arc<DesktopService>) -> ExitCode {
    let built = tauri::Builder::default()
        .manage(service)
        .manage(ShutdownGate::default())
        .setup(|app| {
            windows::create_windows(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::sessions::session_tree,
            commands::sessions::session_search,
            commands::sessions::session_transcript,
            commands::sessions::session_archive,
            commands::sessions::session_restore,
            commands::sessions::session_interrupt,
            commands::sessions::workspace_list,
            commands::sessions::session_create,
            commands::chat::chat_defaults,
            commands::chat::chat_send,
            commands::chat::chat_status,
            commands::chat::chat_cancel,
            commands::approvals::approvals_summary,
            commands::approvals_show_window,
            commands::approvals::approvals_list,
            commands::approvals::approval_detail,
            commands::approvals::approval_decide
        ])
        .build(tauri::generate_context!());
    match built {
        Ok(app) => {
            app.run(|handle, event| {
                // Any exit path that did not come from the bounded shutdown is
                // stopped, run through that shutdown once, and then allowed.
                if let RunEvent::ExitRequested { code, api, .. } = event
                    && !handle.state::<ShutdownGate>().is_done()
                {
                    api.prevent_exit();
                    shutdown::begin(handle, code.unwrap_or(0));
                }
            });
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("omgcode-desktop: the application could not start");
            ExitCode::FAILURE
        }
    }
}
