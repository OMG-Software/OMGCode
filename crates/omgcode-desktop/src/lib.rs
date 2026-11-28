//! OMGCode desktop shell (Tauri 2).
//!
//! The Rust side is the only authority. Every webview renders untrusted text and
//! reaches Rust only through commands that are (1) gated by a per-window Tauri
//! capability and (2) checked again against the window policy in
//! [`policy`]. See ADR-0045, ADR-0046, and ADR-0059.
//!
//! The data root, and either the model endpoint with the environment token or a
//! provider profile (whose recorded endpoint and stored credential are used
//! instead), come from the process command line ([`launch`]) and nowhere else.
//! With neither, each turn uses the active connection chosen in the `settings`
//! window, or the default endpoint while none is chosen (ADR-0064).

pub mod commands;
pub mod dialogs;
pub mod error;
pub mod launch;
pub mod policy;
pub mod shutdown;
pub mod windows;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use omgcode_app::desktop::{DesktopConfig, DesktopService};
use omgcode_app::{
    ChatClients, ChatSettings, ConnectionSource, LocalConnection, TokenSource,
    resolve_local_connection,
};
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
/// unusable, a selected provider profile unusable (unknown, no recorded endpoint,
/// or an unreadable credential), runtime could
/// not start). Every failure happens before a window
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
    let (data_root, endpoint, token_source) = match (args.provider_profile, args.endpoint) {
        (None, Some(endpoint)) => {
            // Validate the endpoint the way a chat turn would, so a malformed
            // or non-loopback endpoint fails now, with the provider's message.
            check_endpoint(&endpoint, None)?;
            let data_root =
                omgcode_app::data_root(args.data_dir).map_err(|error| error.to_string())?;
            (data_root, endpoint, TokenSource::Environment)
        }
        (None, None) => {
            // No connection argument: each turn reads the active connection
            // chosen in settings, and uses the default while none is chosen.
            // An unusable selection is reported by the turn, not here, so the
            // settings window stays reachable to change it.
            check_endpoint(launch::DEFAULT_ENDPOINT, None)?;
            let data_root =
                omgcode_app::data_root(args.data_dir).map_err(|error| error.to_string())?;
            (
                data_root,
                launch::DEFAULT_ENDPOINT.to_owned(),
                TokenSource::Active,
            )
        }
        (Some(name), _) => {
            // A selected profile is the only source of the endpoint, and the
            // launch default in `args.endpoint` is not used (ADR-0048). The
            // profile is resolved once now, so an unknown profile, a profile
            // of another provider, a profile with no recorded endpoint, or an
            // unreadable credential stops the launch before a window opens.
            let data_root =
                omgcode_app::data_root(args.data_dir).map_err(|error| error.to_string())?;
            let connection =
                resolve_local_connection(&data_root, &ConnectionSource::Profile(name.clone()))
                    .map_err(|error| error.to_string())?;
            // The token is held only through the check, because a cloud
            // endpoint is refused without one, and then dropped; each turn
            // resolves it again. The endpoint is the one the profile recorded,
            // and it is validated like any other so a hand-edited journal
            // cannot slip past.
            check_connection(&connection)?;
            (data_root, connection.endpoint, TokenSource::Profile(name))
        }
    };
    DesktopService::open(DesktopConfig {
        data_root,
        endpoint,
        token_source,
        stream: args.stream,
        claude_code_relay: claude_code_relay(),
    })
    .map_err(|error| format!("{}: {}", error.code(), error.message()))
}

/// The `omgcode` executable in this executable's folder, which a Claude Code
/// turn runs as its MCP relay (ADR-0111), or `None` when there is none. Every
/// release archive ships both executables in one folder, so
/// `omgcode workspace claude-code-grant` run from it grants this folder.
fn claude_code_relay() -> Option<PathBuf> {
    let desktop = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()?;
    let relay = desktop.parent()?.join(if cfg!(windows) {
        "omgcode.exe"
    } else {
        "omgcode"
    });
    relay.is_file().then_some(relay)
}

/// Validate an endpoint the way a chat turn would. The clients are discarded;
/// no request is made. A token is passed only so a cloud endpoint, which core
/// refuses without one, validates; it is never stored or sent.
fn check_endpoint(endpoint: &str, token: Option<&str>) -> Result<(), String> {
    let probe = ChatSettings::new(endpoint, "endpoint-check", PROBE_CONTEXT_TOKENS);
    ChatClients::new(&probe, token)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Validate a resolved profile connection the way a chat turn would, with a
/// plan session's clients for a ChatGPT profile (ADR-0093). No request is
/// made and no token is read.
fn check_connection(connection: &LocalConnection) -> Result<(), String> {
    let probe = ChatSettings::new(&connection.endpoint, "endpoint-check", PROBE_CONTEXT_TOKENS);
    ChatClients::for_connection(&probe, connection)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn run_app(service: Arc<DesktopService>) -> ExitCode {
    let observed = Arc::clone(&service);
    let built = tauri::Builder::default()
        .manage(service)
        .manage(ShutdownGate::default())
        .setup(move |app| {
            windows::create_windows(app)?;
            // A turn that starts waiting for an approval shows the approvals
            // window (ADR-0105). The service keeps this observer and the handle
            // reaches the service through managed state; that cycle ends when
            // `DesktopService::shutdown` drops the observer.
            let handle = app.handle().clone();
            if observed
                .set_approval_wait_observer(Arc::new(move || windows::present_approvals(&handle)))
                .is_err()
            {
                eprintln!("omgcode-desktop: the approvals window will not open on its own");
            }
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
            commands::chat::chat_subscribe,
            commands::approvals::approvals_summary,
            commands::approvals_show_window,
            commands::approvals::approvals_list,
            commands::approvals::approval_detail,
            commands::approvals::approval_decide,
            commands::settings_show_window,
            commands::settings_show_workspace,
            commands::chat::chat_preferences_save,
            commands::chat::session_skills,
            commands::chat::session_context,
            commands::sessions::workspace_entries,
            commands::git::workspace_git_status,
            commands::chat::connection_status,
            commands::chat::connection_select,
            commands::chat::session_model_select,
            commands::chat::session_effort_select,
            commands::chat::chat_models,
            commands::chat::session_context_breakdown,
            commands::settings::settings_overview,
            commands::settings::settings_choose_root,
            commands::settings::settings_register_workspace,
            commands::settings::settings_set_permissions,
            commands::settings::settings_choose_package_cache,
            commands::settings::settings_clear_package_cache,
            commands::settings::settings_unregister_workspace,
            commands::settings::settings_select_connection,
            commands::settings::settings_add_connection,
            commands::settings::settings_remove_connection,
            commands::settings::settings_sign_in_chatgpt,
            commands::settings::settings_cancel_chatgpt_sign_in,
            commands::settings::settings_sign_out_chatgpt,
            commands::settings::settings_set_api_key,
            commands::settings::settings_clear_scope,
            commands::settings::settings_repair_data_folder,
            commands::settings::settings_save_chat_defaults,
            commands::settings::settings_set_workspace_instructions,
            commands::settings::settings_import_skill,
            commands::settings::settings_set_skill_state,
            commands::settings::settings_remove_skill,
            commands::sessions::session_processes,
            commands::settings::settings_add_toolchain_profile,
            commands::settings::settings_remove_toolchain_profile,
            commands::settings::settings_add_package_profile,
            commands::settings::settings_remove_package_profile,
            commands::settings::settings_grant_container_folder,
            commands::settings::settings_add_claude_code,
            commands::settings::settings_grant_claude_code,
            commands::settings::settings_revoke_claude_code,
            commands::settings::settings_revoke_container_grant,
            commands::settings::settings_add_web_fetch_host,
            commands::settings::settings_remove_web_fetch_host,
            commands::sessions::session_web_audit,
            commands::settings::settings_discover_toolchains,
            commands::settings::settings_add_discovered_profiles,
            commands::settings::settings_assign_toolchain_profile,
            commands::settings::settings_unassign_toolchain_profile
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A selected profile is checked at launch, before the service or any
    /// window exists, and the check creates nothing in the data root.
    #[test]
    fn a_selected_profile_with_no_settings_store_stops_the_launch() {
        let root = std::env::temp_dir().join(format!(
            "omgcode-desktop-launch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let failure = open_service(LaunchArgs {
            data_dir: Some(root.clone()),
            endpoint: None,
            provider_profile: Some("lms".to_owned()),
            stream: true,
        })
        .err()
        .expect("no settings store exists");
        assert!(
            failure.starts_with("credential: no settings store exists"),
            "{failure}"
        );
        assert!(!root.exists(), "the launch check created the data root");
    }

    /// Without a profile the endpoint is validated first, so a bad endpoint is
    /// the reported failure and no data root is touched.
    #[test]
    fn a_bad_endpoint_without_a_profile_is_refused() {
        let failure = open_service(LaunchArgs {
            data_dir: Some(std::env::temp_dir().join("omgcode-desktop-never-created")),
            endpoint: Some("http://example.com:1234/v1".to_owned()),
            provider_profile: None,
            stream: true,
        })
        .err()
        .expect("a non-loopback endpoint is refused");
        assert!(failure.starts_with("provider:"), "{failure}");
    }

    /// With a profile the launch endpoint is not used at all (ADR-0048): a bad
    /// value there is ignored and the profile decides, here by having no
    /// settings store.
    #[test]
    fn a_selected_profile_ignores_the_launch_endpoint() {
        let root = std::env::temp_dir().join("omgcode-desktop-never-created-2");
        let failure = open_service(LaunchArgs {
            data_dir: Some(root.clone()),
            endpoint: Some("http://example.com:1234/v1".to_owned()),
            provider_profile: Some("lms".to_owned()),
            stream: true,
        })
        .err()
        .expect("no settings store exists");
        assert!(
            failure.starts_with("credential: no settings store exists"),
            "{failure}"
        );
        assert!(!root.exists());
    }

    const CLOUD: &str = "https://ollama.com/v1";

    #[test]
    fn a_cloud_endpoint_is_accepted_with_a_token_and_refused_without_one() {
        // Given the Ollama Cloud endpoint a profile launch resolves.
        // When the check runs with the profile's token, then it passes.
        assert_eq!(check_endpoint(CLOUD, Some("test-token")), Ok(()));
        // When it runs without a token (the --endpoint and default branches),
        // then the cloud endpoint is still refused.
        assert!(check_endpoint(CLOUD, None).is_err());
    }

    #[test]
    fn a_loopback_endpoint_needs_no_token() {
        assert_eq!(check_endpoint("http://127.0.0.1:1234/v1", None), Ok(()));
    }
}
