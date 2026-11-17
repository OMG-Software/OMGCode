//! Window creation, the navigation guard, and close behaviour.
//!
//! Both windows are created in Rust so that the navigation and new-window
//! hooks are attached before the first page loads. Neither window may leave
//! the application's own origin: the webviews hold the IPC capability, so an
//! external page inside one would be a privilege problem.

use tauri::webview::NewWindowResponse;
use tauri::{App, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::policy::WindowLabel;
use crate::shutdown;

/// The development server origin. Honoured in debug builds only.
const DEV_HOST: &str = "localhost";
const DEV_PORT: u16 = 1420;

/// Tauri's bundled-asset origin: `http://tauri.localhost` on Windows (the
/// default scheme; `useHttpsScheme` stays off because the CSP names
/// `http://ipc.localhost`) and `tauri://localhost` elsewhere.
const APP_HOST: &str = "localhost";
const WINDOWS_APP_HOST: &str = "tauri.localhost";

/// Decides whether a webview may navigate to `url`.
///
/// Allowed: the platform's bundled-asset origin, and `http://localhost:1420` in
/// debug builds. Everything else is denied, including `about:blank`, `file:`,
/// `data:`, `javascript:`, `blob:`, other loopback addresses and ports, and any
/// URL with credentials. The origin is compared on parsed scheme, host, and
/// port, never on a string prefix, so `tauri.localhost.evil.test` and
/// `http://tauri.localhost@evil.test/` are refused. `about:blank` is denied
/// because no page in this application needs it.
#[must_use]
pub fn navigation_allowed(url: &Url, debug: bool) -> bool {
    allowed_for(url, cfg!(windows), debug)
}

/// [`navigation_allowed`] with the platform made explicit so both can be tested.
fn allowed_for(url: &Url, windows: bool, debug: bool) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let port = url.port();
    match url.scheme() {
        "http" => {
            (windows && host == WINDOWS_APP_HOST && port.is_none())
                || (debug && host == DEV_HOST && port == Some(DEV_PORT))
        }
        "tauri" => !windows && host == APP_HOST && port.is_none(),
        _ => false,
    }
}

/// Creates the `main` and `approvals` windows.
///
/// `approvals` starts hidden. Devtools exist only in debug builds. File
/// drag-and-drop is disabled so a dropped file cannot navigate a webview.
///
/// # Errors
///
/// Returns the runtime's error when a window cannot be built.
pub fn create_windows(app: &App) -> tauri::Result<()> {
    let debug = cfg!(debug_assertions);

    let main = WebviewWindowBuilder::new(
        app,
        WindowLabel::Main.as_str(),
        WebviewUrl::App("index.html".into()),
    )
    .title("OMGCode")
    .inner_size(1200.0, 800.0)
    .min_inner_size(800.0, 560.0)
    .disable_drag_drop_handler()
    .devtools(debug)
    .on_navigation(move |url| navigation_allowed(url, debug))
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .build()?;
    let handle = app.handle().clone();
    main.on_window_event(move |event| {
        // Closing `main` ends the application; the hidden approvals window must
        // not keep the process alive. The close is held back so both windows can
        // be hidden at once and the bounded shutdown can run off this thread.
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            shutdown::begin(&handle, 0);
        }
    });

    let approvals = WebviewWindowBuilder::new(
        app,
        WindowLabel::Approvals.as_str(),
        WebviewUrl::App("approvals.html".into()),
    )
    .title("OMGCode \u{2014} Tool approvals")
    .inner_size(760.0, 720.0)
    .min_inner_size(560.0, 480.0)
    .visible(false)
    .disable_drag_drop_handler()
    .devtools(debug)
    .on_navigation(move |url| navigation_allowed(url, debug))
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .build()?;
    let hidden = approvals.clone();
    approvals.on_window_event(move |event| {
        // Closing hides the window so its state and the process survive.
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if hidden.hide().is_err() {
                eprintln!("omgcode-desktop: could not hide the approvals window");
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("test URL parses")
    }

    #[test]
    fn app_origin_is_allowed_per_platform() {
        assert!(allowed_for(
            &url("http://tauri.localhost/index.html"),
            true,
            false
        ));
        assert!(allowed_for(&url("http://tauri.localhost/"), true, true));
        assert!(allowed_for(
            &url("tauri://localhost/approvals.html"),
            false,
            false
        ));
        // The other platform's origin is not accepted.
        assert!(!allowed_for(&url("tauri://localhost/"), true, false));
        assert!(!allowed_for(&url("http://tauri.localhost/"), false, false));
        assert!(!allowed_for(&url("https://tauri.localhost/"), true, false));
    }

    #[test]
    fn dev_server_is_allowed_only_in_debug() {
        for windows in [true, false] {
            assert!(allowed_for(&url("http://localhost:1420/"), windows, true));
            assert!(!allowed_for(&url("http://localhost:1420/"), windows, false));
            assert!(!allowed_for(&url("http://localhost:1421/"), windows, true));
            assert!(!allowed_for(&url("http://localhost/"), windows, true));
            assert!(!allowed_for(&url("https://localhost:1420/"), windows, true));
        }
    }

    #[test]
    fn foreign_and_unusual_urls_are_denied() {
        for windows in [true, false] {
            for debug in [true, false] {
                for text in [
                    "https://example.com/",
                    "http://127.0.0.1:1420/",
                    "http://[::1]:1420/",
                    "file:///",
                    "file:///C:/Windows/win.ini",
                    "data:text/html,<p>x</p>",
                    "javascript:alert(1)",
                    "about:blank",
                    "blob:http://tauri.localhost/1",
                    "http://tauri.localhost.evil.test/",
                    "http://tauri.localhost@evil.test/",
                    "http://tauri.localhost:80@evil.test/",
                    "http://evil.test@tauri.localhost/",
                    "http://user:pw@tauri.localhost/",
                    "http://tauri.localhost:8080/",
                    "http://evil.test/tauri.localhost",
                    "tauri://localhost.evil.test/",
                    "tauri://evil@localhost/",
                    "ipc://localhost/app_info",
                ] {
                    assert!(
                        !allowed_for(&url(text), windows, debug),
                        "{text} must be denied (windows={windows}, debug={debug})"
                    );
                }
            }
        }
    }

    #[test]
    fn host_case_and_default_port_normalize() {
        assert!(allowed_for(&url("http://TAURI.LOCALHOST:80/"), true, false));
        assert!(allowed_for(&url("HTTP://Tauri.Localhost/"), true, false));
    }

    #[test]
    fn public_function_uses_the_build_platform() {
        let own = if cfg!(windows) {
            "http://tauri.localhost/"
        } else {
            "tauri://localhost/"
        };
        assert!(navigation_allowed(&url(own), false));
        assert!(!navigation_allowed(&url("https://example.com/"), true));
    }
}
