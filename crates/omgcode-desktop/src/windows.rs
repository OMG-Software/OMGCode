//! Window creation, the navigation guard, close behaviour, and bringing a
//! window to the front.
//!
//! Every window is created in Rust so that the navigation and new-window
//! hooks are attached before the first page loads. No window may leave
//! the application's own origin: the webviews hold the IPC capability, so an
//! external page inside one would be a privilege problem.

use tauri::webview::NewWindowResponse;
use tauri::window::Color;
use tauri::{
    App, AppHandle, Manager, Url, UserAttentionType, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};

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

/// The window and webview background before the page paints. It matches `--bg` in
/// `ui/src/styles/tokens.css`, so a window never flashes white on open (ADR-0065).
const WINDOW_BACKGROUND: Color = Color(0x11, 0x11, 0x11, 0xff);

/// Creates the `main`, `approvals`, and `settings` windows.
///
/// `approvals` and `settings` start hidden, and closing either hides it. Devtools exist only in debug builds. File
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
    .background_color(WINDOW_BACKGROUND)
    .disable_drag_drop_handler()
    .devtools(debug)
    .on_navigation(move |url| navigation_allowed(url, debug))
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .build()?;
    let handle = app.handle().clone();
    main.on_window_event(move |event| {
        // Closing `main` ends the application; the hidden approvals and settings
        // windows must not keep the process alive. The close is held back so
        // every window can be hidden at once and the bounded shutdown can run off this thread.
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
    .inner_size(960.0, 720.0)
    .min_inner_size(600.0, 480.0)
    .background_color(WINDOW_BACKGROUND)
    .visible(false)
    .disable_drag_drop_handler()
    .devtools(debug)
    .on_navigation(move |url| navigation_allowed(url, debug))
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .build()?;
    hide_on_close(&approvals);

    // The settings window renders only operator-recorded configuration (ADR-0059).
    let settings = WebviewWindowBuilder::new(
        app,
        WindowLabel::Settings.as_str(),
        WebviewUrl::App("settings.html".into()),
    )
    .title("OMGCode \u{2014} Settings")
    .inner_size(960.0, 720.0)
    .min_inner_size(600.0, 480.0)
    .background_color(WINDOW_BACKGROUND)
    .visible(false)
    .disable_drag_drop_handler()
    .devtools(debug)
    .on_navigation(move |url| navigation_allowed(url, debug))
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .build()?;
    hide_on_close(&settings);
    Ok(())
}

/// Unminimize, show, and focus `window`.
///
/// Used when the operator asks for a window from `main`, a request that
/// follows their own click, so this process is in the foreground and
/// focusing needs no workaround. A window shown on its own uses
/// [`present_approvals`] instead.
///
/// # Errors
///
/// The runtime's error when a step cannot be sent to the event loop.
pub fn bring_to_front(window: &WebviewWindow) -> tauri::Result<()> {
    window.unminimize()?;
    window.show()?;
    window.set_focus()
}

/// Show the `approvals` window because a chat turn started waiting for a
/// decision (ADR-0105).
///
/// When the operator is working in this application, the window is brought to
/// the front. Otherwise it is shown and its taskbar button flashes until the
/// operator comes to it, and focus stays with the other application. Neither
/// path approves anything: the window never focuses **Approve**, no key
/// decides, and **Approve** is armed only one second after a review renders.
///
/// Runs on the turn's thread before the wait starts and only posts one
/// closure to the event loop. A failure is reported on standard error and the
/// turn waits either way.
pub fn present_approvals(app: &AppHandle) {
    let handle = app.clone();
    // The foreground check and the activation run in one event-loop callback,
    // where window calls take effect at once, which narrows the gap that a
    // check on the turn's thread left before the queued focus request ran.
    // It does not close it: `activate_if_foreground_owner` checks ownership
    // again, and the window flashes if another application took over.
    if app
        .run_on_main_thread(move || present_on_event_loop(&handle))
        .is_err()
    {
        eprintln!("omgcode-desktop: could not show the approvals window");
    }
}

fn present_on_event_loop(app: &AppHandle) {
    let Some(window) = app.get_webview_window(WindowLabel::Approvals.as_str()) else {
        eprintln!("omgcode-desktop: the approvals window is missing");
        return;
    };
    let shown = if operator_is_here(app) {
        raise_without_input(&window)
    } else {
        show_and_flash(&window)
    };
    if shown.is_err() {
        eprintln!("omgcode-desktop: could not show the approvals window");
    }
}

/// Show `window` and flash its taskbar button until the operator comes to it,
/// leaving the foreground where it is.
fn show_and_flash(window: &WebviewWindow) -> tauri::Result<()> {
    window.show()?;
    window.request_user_attention(Some(UserAttentionType::Critical))
}

/// Unminimize, show, and activate `window` without tao's `set_focus`, which
/// sends a simulated Alt key press to the foreground application when
/// Windows refuses the activation. If another application took the
/// foreground since the check, or Windows refuses, the window flashes
/// instead.
#[cfg(windows)]
fn raise_without_input(window: &WebviewWindow) -> tauri::Result<()> {
    window.unminimize()?;
    window.show()?;
    let activated = window.hwnd().is_ok_and(|hwnd| {
        omgcode_platform::foreground::activate_if_foreground_owner(hwnd.0.addr())
    });
    if activated {
        Ok(())
    } else {
        window.request_user_attention(Some(UserAttentionType::Critical))
    }
}

/// Unminimize, show, and focus `window`; outside Windows the toolkit's focus
/// request sends no input.
#[cfg(not(windows))]
fn raise_without_input(window: &WebviewWindow) -> tauri::Result<()> {
    bring_to_front(window)
}

/// Whether the operator is working in this application now. On Windows the
/// foreground window's owner decides, because a window's own focus flag is
/// clear while its webview holds the keyboard focus. Elsewhere a window of
/// this application must report focus.
fn operator_is_here(app: &AppHandle) -> bool {
    omgcode_platform::foreground::this_process_owns_foreground().unwrap_or_else(|| {
        WindowLabel::ALL.iter().any(|label| {
            app.get_webview_window(label.as_str())
                .is_some_and(|window| window.is_focused().unwrap_or(false))
        })
    })
}

/// Closing a secondary window hides it, so its state and the process survive.
fn hide_on_close(window: &WebviewWindow) {
    let hidden = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if hidden.hide().is_err() {
                eprintln!(
                    "omgcode-desktop: could not hide the {} window",
                    hidden.label()
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("test URL parses")
    }

    #[test]
    fn window_background_matches_the_theme_background() {
        let tokens = include_str!("../ui/src/styles/tokens.css");
        let Color(red, green, blue, alpha) = WINDOW_BACKGROUND;
        let expected = format!("--bg: #{red:02x}{green:02x}{blue:02x};");
        assert_eq!(alpha, 0xff, "the window background is opaque");
        assert!(
            tokens.lines().any(|line| line.trim() == expected),
            "tokens.css must declare `{expected}` so a window opens in its page colour"
        );
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
