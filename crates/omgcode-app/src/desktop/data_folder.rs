//! Removing extra grants from the application data folder (ADR-0073).
//!
//! The credential store refuses a data folder whose DACL grants a principal
//! other than the current user, SYSTEM, and Administrators, which the window
//! sees as `credential_storage_not_private`. This module lets the operator
//! remove those grants from the `settings` window:
//!
//! 1. It lists the grants from the folder itself. The request carries no path
//!    and no list, so a compromised window cannot choose what is changed.
//! 2. It shows the folder and each account in a native confirmation that no
//!    webview can open, answer, or read.
//! 3. Only after an explicit Yes does it pass the listed grants back to the
//!    platform, which reads the folder again and refuses with
//!    `data_folder_changed` if the grants differ.
//!
//! Nothing here runs without the operator, and nothing is recorded in the
//! control journal: the change is to the folder's DACL, not to a setting.

use std::fmt::Write as _;
use std::path::Path;

use omgcode_core::control::{
    ExtraGrant, RootAccess, RootRepairError, inspect_root_access, remove_extra_root_grants,
};
use omgcode_core::turn::display_safe_line;

use super::DesktopService;
use super::error::DesktopError;
use super::settings_changes::{ChangeGuard, Confirmation, SettingsPrompt};
use super::wire::SettingsOverview;

/// The fixed confirmation title.
const TITLE: &str = "Remove extra folder permissions";

impl DesktopService {
    /// List the data folder's extra grants, ask the operator to remove them
    /// in a native confirmation, and remove exactly those grants on Yes
    /// (ADR-0073). A folder that grants nothing extra is left unchanged.
    ///
    /// # Errors
    ///
    /// `settings_change_running`; `data_folder_unreadable` when the folder's
    /// permissions cannot be read; `data_folder_not_repairable` with a fixed
    /// reason; `cancelled` when the operator declines; `data_folder_changed`
    /// when the grants changed after the confirmation; and
    /// `data_folder_repair_failed` when opening the folder for the change,
    /// writing its permissions, or checking them afterwards fails, in which
    /// case the folder may or may not have changed.
    pub fn settings_repair_data_folder(
        &self,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let root = &self.inner.data_root;
        let grants = match inspect_root_access(root) {
            Ok(RootAccess::Private) => return self.settings_overview(),
            Ok(RootAccess::Repairable(grants)) => grants,
            Ok(RootAccess::NotRepairable(reason)) => {
                return Err(DesktopError::data_folder_not_repairable(reason));
            }
            Err(_) => return Err(DesktopError::data_folder_unreadable()),
        };
        if !prompt.confirm(&repair_confirmation(root, &grants)) {
            return Err(DesktopError::cancelled());
        }
        match remove_extra_root_grants(root, &grants) {
            Ok(()) => self.settings_overview(),
            Err(RootRepairError::Changed) => Err(DesktopError::data_folder_changed()),
            Err(RootRepairError::NotRepairable(reason)) => {
                Err(DesktopError::data_folder_not_repairable(reason))
            }
            Err(RootRepairError::Io(_)) => Err(DesktopError::data_folder_repair_failed()),
        }
    }
}

/// The native confirmation, composed from fixed text, the escaped folder
/// path, and each grant's escaped account name and SID.
fn repair_confirmation(root: &Path, grants: &[ExtraGrant]) -> Confirmation {
    let mut message = format!(
        "OMGCode keeps API keys in its application data folder:\n{}\n\n\
         That folder also gives these accounts access, so OMGCode will not store keys there:\n",
        display_safe_line(&root.display().to_string())
    );
    for grant in grants {
        let sid = display_safe_line(&grant.sid);
        // Writing to a `String` cannot fail.
        let _ = match &grant.account {
            Some(account) => writeln!(message, "  - {} ({sid})", display_safe_line(account)),
            None => writeln!(message, "  - {sid}"),
        };
    }
    message.push_str(
        "\nRemove them? The folder will also stop inheriting permissions from its parent, \
         so they are not added back. Your account keeps its access, and any entry the \
         folder already has for SYSTEM or Administrators is kept; no entry is added. \
         Programs that run as the removed accounts lose access to OMGCode's data.",
    );
    Confirmation {
        title: TITLE,
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn the_confirmation_names_the_folder_and_every_account_escaped() {
        let confirmation = repair_confirmation(
            Path::new(r"C:\Users\someone\AppData\Local\OMG-Software\OMGCode\data"),
            &[
                ExtraGrant {
                    sid: "S-1-5-21-1-2-3-1004".to_owned(),
                    account: Some("HOST\\Sandbox\u{1b}[31mUsers".to_owned()),
                },
                ExtraGrant {
                    sid: "S-1-15-3-1".to_owned(),
                    account: None,
                },
            ],
        );
        assert_eq!(confirmation.title, TITLE);
        let text = &confirmation.message;
        assert!(text.contains(r"OMGCode\data"), "{text}");
        assert!(text.contains("(S-1-5-21-1-2-3-1004)"), "{text}");
        assert!(text.contains("  - S-1-15-3-1\n"), "{text}");
        assert!(
            !text.contains('\u{1b}'),
            "control characters are escaped: {text}"
        );
        assert!(text.contains("Remove them?"), "{text}");
        // The repair adds no entry, so the dialog promises SYSTEM and
        // Administrators nothing the folder does not already grant them.
        assert!(text.contains("no entry is added"), "{text}");
        assert!(!text.contains("SYSTEM, and Administrators keep"), "{text}");
    }
}
