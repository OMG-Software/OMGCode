//! Native dialogs for settings changes (ADR-0059 decisions 4 and 5, ADR-0060).
//!
//! [`NativePrompt`] implements the service's [`SettingsPrompt`] with the
//! operating system's folder picker and message box through `rfd`, called from
//! Rust only. No webview can open, answer, or read these dialogs: there is no
//! dialog command and no `dialog:` permission, and the chosen path goes from the
//! dialog to core without passing through any webview.
//!
//! The dialogs are modal to the `settings` window and block the calling thread,
//! so the settings handlers call the service on the blocking pool. Each dialog
//! runs on its own short-lived thread, joined before the call returns, so its
//! COM apartment on Windows does not depend on a pooled thread's history. A
//! dialog that cannot be shown returns no folder or no confirmation, so a
//! failure never applies a change.
//!
//! Platform notes:
//! - Windows: the folder picker is `IFileOpenDialog` and the confirmation is a
//!   Yes/No message box. Its default button is Yes, as `rfd` offers no choice.
//! - Linux: on first use `rfd` starts one long-lived thread that initialises
//!   GTK 3 and iterates the default GLib main context. It queues each dialog
//!   as an idle callback on that default context and blocks the caller until
//!   the dialog returns. GTK calls must stay on the thread that initialised
//!   GTK, but Tauri initialises GTK on its event loop thread, which also
//!   iterates the default context, so the callback may run on either thread.
//!   No window has been opened on Linux yet (ADR-0045), so this interaction
//!   is untested there.

use std::path::{Path, PathBuf};

use omgcode_app::desktop::{Confirmation, FolderPurpose, SettingsPrompt};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use tauri::Window;

/// Native dialogs, modal to one parent window.
pub struct NativePrompt {
    parent: Window,
}

impl NativePrompt {
    /// Dialogs modal to `parent`, which is the calling `settings` window.
    #[must_use]
    pub const fn new(parent: Window) -> Self {
        Self { parent }
    }
}

impl SettingsPrompt for NativePrompt {
    fn pick_folder(&self, purpose: FolderPurpose, start: Option<&Path>) -> Option<PathBuf> {
        on_dialog_thread(|| {
            let mut dialog = FileDialog::new()
                .set_title(purpose.title())
                .set_parent(&self.parent);
            if let Some(start) = start {
                dialog = dialog.set_directory(start);
            }
            dialog.pick_folder()
        })
        .flatten()
    }

    fn confirm(&self, confirmation: &Confirmation) -> bool {
        on_dialog_thread(|| {
            MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title(confirmation.title)
                .set_description(confirmation.message.as_str())
                .set_buttons(MessageButtons::YesNo)
                .set_parent(&self.parent)
                .show()
        })
        .is_some_and(|answer| is_confirmed(&answer))
    }
}

/// Run one dialog on a new thread and wait for it.
///
/// On Windows `rfd` initializes COM as single-threaded for each dialog and
/// fails if the thread already joined another apartment. A pooled thread may
/// have, so each dialog gets a fresh thread. The scope joins it before
/// returning. A thread that panics, or cannot be started, yields `None`, so
/// the caller treats it as a cancelled dialog and applies nothing.
fn on_dialog_thread<T: Send>(dialog: impl FnOnce() -> T + Send) -> Option<T> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("omgcode-dialog".to_owned())
            .spawn_scoped(scope, dialog)
            .ok()?
            .join()
            .ok()
    })
}

/// Only an explicit Yes confirms. No, closing the box, and anything else
/// decline.
const fn is_confirmed(answer: &MessageDialogResult) -> bool {
    matches!(answer, MessageDialogResult::Yes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_runs_on_its_own_named_thread_and_a_panic_declines() {
        let caller = std::thread::current().id();
        let (thread, name) = on_dialog_thread(|| {
            let current = std::thread::current();
            (current.id(), current.name().map(str::to_owned))
        })
        .expect("the dialog thread ran");
        assert_ne!(thread, caller);
        assert_eq!(name.as_deref(), Some("omgcode-dialog"));
        let panicked: Option<bool> = on_dialog_thread(|| panic!("dialog failed"));
        assert_eq!(panicked, None);
    }

    #[test]
    fn only_yes_confirms() {
        assert!(is_confirmed(&MessageDialogResult::Yes));
        for answer in [
            MessageDialogResult::No,
            MessageDialogResult::Ok,
            MessageDialogResult::Cancel,
            MessageDialogResult::Custom("Yes".to_owned()),
        ] {
            assert!(!is_confirmed(&answer), "{answer:?} must not confirm");
        }
    }
}
