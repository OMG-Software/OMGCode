//! Add, grant, and revoke a Claude Code connection from the `settings`
//! window (ADR-0109, ADR-0111).
//!
//! Each change follows the window's rule for a change that widens access:
//! Rust opens the native dialogs, core checks what the operator chose before
//! the confirmation, one native confirmation names every folder, and core
//! records the change with the revision the window read. The webview sends
//! only a revision and a connection name.
//!
//! OMGCode never signs in to Claude. The window tells the operator to sign in
//! from a terminal with `CLAUDE_CONFIG_DIR` set to the chosen folder; nothing
//! here runs `claude`, reads the folder's files, or passes a credential.

use std::path::Path;

use omgcode_core::control::{
    CLAUDE_CODE_TURN_STOP_WAIT, CONTAINER_GRANTS_SUPPORTED, ClaudeCodeDeclaration, ControlError,
    claude_code_runtime_folder, wait_for_claude_code_turns,
};
use omgcode_core::turn::display_safe_line;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::settings_changes::{
    AlsoWithdrawn, ChangeGuard, Confirmation, FilePurpose, FolderPurpose, MAX_NAME_INPUT_BYTES,
    SettingsPrompt, check_revision, confirm, shown,
};
use super::wire::{ClaudeCodeAddRequest, ClaudeCodeConnectionRequest, SettingsOverview};

impl DesktopService {
    /// `settings` only. Record a Claude Code connection (ADR-0111): Rust opens
    /// a native file dialog for the `claude` program and a native folder
    /// dialog for the folder the operator signed in to, core checks both,
    /// and one native confirmation names them before core records the
    /// connection. Nothing is started and the folder's files are not read.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a name that is too long; `cancelled`;
    /// `stale_revision`; `settings_change_running`; `busy`; `control` with
    /// core's reason (a duplicate or invalid name, a program that is not a
    /// regular file or, on Windows, not an `.exe`, or a folder that is too
    /// broad or overlaps application storage, the development root, or a
    /// workspace).
    pub fn settings_add_claude_code(
        &self,
        request: ClaudeCodeAddRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        if request.name.len() > MAX_NAME_INPUT_BYTES {
            return Err(DesktopError::invalid_request("A value is too long."));
        }
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
        }
        let program = prompt
            .pick_file(FilePurpose::ClaudeCodeProgram, None)
            .ok_or_else(DesktopError::cancelled)?;
        let config_folder = prompt
            .pick_folder(FolderPurpose::ClaudeCodeSignIn, None)
            .ok_or_else(DesktopError::cancelled)?;
        let declaration = ClaudeCodeDeclaration {
            name: request.name,
            program,
            config_folder,
        };
        // Core's checks run before the operator is asked, so a refused
        // program or folder never reaches the confirmation.
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            store
                .check_claude_code_profile(&declaration)
                .map_err(|error| DesktopError::from_control(&error))?;
        }
        confirm(prompt, &add_confirmation(&declaration))?;
        self.apply_checked(|store| {
            store
                .add_claude_code_profile(&declaration, request.revision)
                .map(|_| ())
                .map_err(|error| DesktopError::from_control(&error))
        })
    }

    /// `settings` only. Windows only. Grant a Claude Code connection's own
    /// AppContainer the folders its process needs, as `omgcode workspace
    /// claude-code-grant` does (ADR-0109, ADR-0111): read and execute on the
    /// `claude` program's folder, on the connection's runtime folder, and on
    /// the folder of this process's relay, and read and write on the sign-in
    /// folder. One native confirmation names the four folders.
    ///
    /// # Errors
    ///
    /// `unsupported_platform` outside Windows; `invalid_request` when no
    /// `omgcode` executable sits beside the desktop application; `cancelled`;
    /// `stale_revision`; `settings_change_running`; `busy`; `control` with
    /// core's reason (an unknown or other profile, a recorded folder that
    /// changed, a folder that is too broad or overlaps storage, the
    /// development root, or a workspace, or a grant that is pending or
    /// revoking); `container_access_failed` when a grant was recorded but its
    /// access list could not be changed (it stays pending; revoke it).
    pub fn settings_grant_claude_code(
        &self,
        request: ClaudeCodeConnectionRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        if !CONTAINER_GRANTS_SUPPORTED {
            return Err(DesktopError::unsupported_platform());
        }
        let relay_folder = self
            .inner
            .claude_code_relay
            .as_deref()
            .and_then(Path::parent)
            .ok_or_else(|| {
                DesktopError::invalid_request(
                    "No omgcode executable sits beside the desktop application, so there is no relay folder to grant.",
                )
            })?
            .to_path_buf();
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        let confirmation = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let (profile_id, setup) = store
                .claude_code_setup(&request.name)
                .map_err(|error| DesktopError::from_control(&error))?;
            grant_confirmation(
                &request.name,
                &setup.program_display(),
                &display_safe_line(&setup.config_folder_display()),
                &shown(&claude_code_runtime_folder(
                    &self.inner.data_root,
                    profile_id,
                )),
                &shown(&relay_folder),
            )
        };
        confirm(prompt, &confirmation)?;
        self.apply_checked(|store| {
            store
                .grant_claude_code(&request.name, &relay_folder, request.revision)
                .map(|_| ())
                .map_err(|error| DesktopError::from_grant(&error))
        })
    }

    /// `settings` only. Revoke every grant of a Claude Code connection and
    /// delete its AppContainer profile, as `omgcode workspace
    /// claude-code-revoke` does, after a native confirmation; then wait up to
    /// [`CLAUDE_CODE_TURN_STOP_WAIT`] until every running turn of the
    /// connection was observed ended (ADR-0109, ADR-0111). The connection
    /// stays recorded. Linux records no grants, so there nothing changes.
    ///
    /// # Errors
    ///
    /// `cancelled`; `stale_revision`; `settings_change_running`; `busy`;
    /// `control` with core's reason, also for a revocation that stopped
    /// after it recorded the withdrawal, whose message says whether the
    /// running turns ended; `container_access_failed`; and core's turn-wait
    /// errors, such as an unconfirmed cleanup.
    pub fn settings_revoke_claude_code(
        &self,
        request: ClaudeCodeConnectionRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            store
                .claude_code_setup(&request.name)
                .map_err(|error| DesktopError::from_control(&error))?;
        }
        confirm(prompt, &revoke_confirmation(&request.name))?;
        let mut withdrawn: Option<Uuid> = None;
        let mut stopped: Option<ControlError> = None;
        let outcome = self.apply_reporting(|store| {
            // The same revision the operator confirmed, checked under the
            // lock this change holds: core's revocation needs none.
            check_revision(store, request.revision)?;
            match store.revoke_claude_code_grants(&request.name) {
                Ok(revocation) => {
                    withdrawn = revocation.turns_withdrawn.then_some(revocation.profile_id);
                    Ok((AlsoWithdrawn::default(), None))
                }
                Err(failure) if failure.withdrawn.is_some() => {
                    withdrawn = failure.withdrawn;
                    stopped = Some(failure.error);
                    Ok((AlsoWithdrawn::default(), None))
                }
                Err(failure) => Err(DesktopError::from_grant(&failure.error)),
            }
        });
        // The settings are released here, so the running turns' watchers can
        // read the withdrawal and end their processes.
        let turns = withdrawn.map_or(Ok(()), |profile_id| {
            wait_for_claude_code_turns(
                &self.inner.data_root,
                profile_id,
                CLAUDE_CODE_TURN_STOP_WAIT,
            )
        });
        match (stopped, turns) {
            (Some(error), turns) => {
                Err(DesktopError::claude_code_revocation_stopped(&error, turns))
            }
            (None, Err(error)) => Err(DesktopError::from_control(&error)),
            (None, Ok(())) => outcome,
        }
    }
}

/// The confirmation for recording a Claude Code connection.
fn add_confirmation(declaration: &ClaudeCodeDeclaration) -> Confirmation {
    Confirmation {
        title: "Add a Claude Code connection",
        message: format!(
            "Add the Claude Code connection {}?\n\nProgram: {}\nSign-in folder: {}\n\nOMGCode records the program and the folder, and starts the program only for a session that chooses this connection. Sign in first from a terminal: set CLAUDE_CONFIG_DIR to that folder and run claude auth login. OMGCode never reads the sign-in.",
            display_safe_line(&declaration.name),
            display_safe_line(&declaration.program.to_string_lossy()),
            shown(&declaration.config_folder),
        ),
    }
}

/// The confirmation for granting a Claude Code connection's container its
/// folders. Every value is display-safe.
fn grant_confirmation(
    name: &str,
    program: &str,
    sign_in: &str,
    runtime: &str,
    relay: &str,
) -> Confirmation {
    Confirmation {
        title: "Grant folders to the Claude Code connection",
        message: format!(
            "Let the sandbox of the Claude Code connection {} use these folders?\n\nRead and execute: the folder that holds {program}\nRead and write: the sign-in folder {sign_in}\nRead and execute: its runtime folder {runtime}\nRead and execute: the OMGCode folder {relay}\n\nThe claude program runs in its own AppContainer with this access and no other folder grant. Revoking removes it.",
            display_safe_line(name),
        ),
    }
}

/// The confirmation for revoking a Claude Code connection's grants.
fn revoke_confirmation(name: &str) -> Confirmation {
    Confirmation {
        title: "Revoke the Claude Code connection's folders",
        message: format!(
            "Revoke every folder grant of the Claude Code connection {} and delete its sandbox?\n\nIts running chat turns end; this waits up to 30 seconds for their processes to stop. The connection stays recorded and can be granted again.",
            display_safe_line(name),
        ),
    }
}

impl DesktopError {
    /// A Claude Code revocation that stopped with `error` after it recorded,
    /// or may have recorded, the withdrawal, given how the wait for the
    /// connection's turns ended, as the CLI reports it. A retry clears no
    /// turn record, so a wait that found one says so.
    fn claude_code_revocation_stopped(
        error: &ControlError,
        turns: Result<(), ControlError>,
    ) -> Self {
        let (turns, next) = match turns {
            Ok(()) => (
                "every running turn of the connection ended".to_owned(),
                "Revoke again.",
            ),
            Err(unconfirmed @ ControlError::ClaudeCodeCleanupUnconfirmed(_)) => (
                format!("its running turns were not observed ended: {unconfirmed}"),
                "A retry cannot clear that record, so revoke again only after that check and the trusted CLI's acknowledgement.",
            ),
            Err(error) => (
                format!("its running turns were not observed ended: {error}"),
                "Revoke again.",
            ),
        };
        Self::control_message(format!(
            "The revocation stopped after it recorded, or may have recorded, the connection's withdrawal: {error}; {turns}. {next}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grant_confirmation_names_all_four_folders() {
        let confirmation = grant_confirmation(
            "claude",
            "C:\\Tools\\claude.exe",
            "C:\\Claude\\sign-in",
            "C:\\Data\\claude-code\\run",
            "C:\\OMGCode",
        );
        for folder in [
            "C:\\Tools\\claude.exe",
            "C:\\Claude\\sign-in",
            "C:\\Data\\claude-code\\run",
            "C:\\OMGCode",
        ] {
            assert!(confirmation.message.contains(folder), "{folder}");
        }
    }

    #[test]
    fn a_stopped_revocation_says_whether_its_turns_ended() {
        let error = ControlError::ProviderRefused("on purpose");
        let ended = DesktopError::claude_code_revocation_stopped(&error, Ok(()));
        assert_eq!(ended.code(), "control");
        assert!(
            ended.message().contains("every running turn"),
            "{}",
            ended.message()
        );
        let waited = DesktopError::claude_code_revocation_stopped(
            &error,
            Err(ControlError::ProviderRefused("still running")),
        );
        assert!(
            waited.message().contains("not observed ended"),
            "{}",
            waited.message()
        );
    }
}
