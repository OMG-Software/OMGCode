//! Add, grant, and revoke a Claude Code connection from the `settings`
//! window (ADR-0109, ADR-0111).
//!
//! Add finds the operator's `claude` on `PATH` or in the native installer's
//! folder, and asks for it in a native file dialog only when none is there.
//! It creates the connection's own sign-in folder, new and empty. Core checks
//! both before one native confirmation, which names them and, on Windows,
//! the folders the connection's sandbox is granted. Core then records the
//! connection and grants it, and on Windows Claude's own sign-in starts in a
//! window of its own. Each other change follows the window's rule for a
//! change that widens access: Rust opens the native dialogs, core checks
//! what the operator chose, one native confirmation names every folder, and
//! core records the change with the revision the window read. The webview
//! sends only a revision and a connection name.
//!
//! OMGCode never signs in to Claude itself: `claude auth login` does, in the
//! browser. Nothing here reads the folder's files or passes a credential.
//! The sign-in starts only for the folder Add just created, before any turn
//! of the connection's sandbox could write it, because the sign-in runs
//! outside that sandbox.

use std::path::{Path, PathBuf};

use omgcode_core::control::{
    CLAUDE_CODE_TURN_STOP_WAIT, CONTAINER_GRANTS_SUPPORTED, ClaudeCodeDeclaration, ControlError,
    claude_code_program_candidates, claude_code_runtime_folder, wait_for_claude_code_turns,
};
use omgcode_core::turn::display_safe_line;
use uuid::Uuid;

use super::DesktopService;
use super::claude_code_sign_in::{
    ClaudeCodeSignIn, NewSignInFolder, SIGN_IN_WINDOW, terminal_command,
};
use super::error::DesktopError;
use super::settings_changes::{
    AlsoWithdrawn, ChangeGuard, Confirmation, FilePurpose, MAX_NAME_INPUT_BYTES, SettingsPrompt,
    check_revision, confirm, shown,
};
use super::wire::{ClaudeCodeAddRequest, ClaudeCodeConnectionRequest, SettingsOverview};

impl DesktopService {
    /// `settings` only. Add a Claude Code connection (ADR-0111): find the
    /// operator's `claude`, or ask for it in a native file dialog when none
    /// is found; create the connection's own empty sign-in folder; and after
    /// one native confirmation, record the connection through core's
    /// `add_claude_code_profile` and, on Windows, grant its sandbox its four
    /// folders through `grant_claude_code`. On Windows Claude's own sign-in
    /// (`claude auth login`) then starts in a window of its own through
    /// [`SettingsPrompt::start_claude_code_sign_in`]; elsewhere the
    /// confirmation names the command to run in a terminal. A cancelled or
    /// refused add removes the folder it created.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a name that is too long, when no per-user
    /// folder is known for the sign-in, or on Windows when no `omgcode`
    /// executable sits beside the desktop application; `cancelled`;
    /// `stale_revision`; `settings_change_running`; `busy`; `control` with
    /// core's reason (a duplicate or invalid name, a program that is not a
    /// regular file or, on Windows, not an `.exe`, or a folder that is too
    /// broad or overlaps application storage, the development root, or a
    /// workspace), or, after the connection was recorded, a grant that
    /// failed or a sign-in that did not start, each with the command that
    /// signs in from a terminal.
    pub fn settings_add_claude_code(
        &self,
        request: ClaudeCodeAddRequest,
        prompt: &dyn SettingsPrompt,
    ) -> Result<SettingsOverview, DesktopError> {
        if request.name.len() > MAX_NAME_INPUT_BYTES {
            return Err(DesktopError::invalid_request("A value is too long."));
        }
        let relay_folder = if CONTAINER_GRANTS_SUPPORTED {
            Some(self.claude_code_relay_folder()?)
        } else {
            None
        };
        let root = self.inner.claude_code.sign_in_root.clone().ok_or_else(|| {
            DesktopError::invalid_request(
                "No per-user folder is known for the connection's sign-in.",
            )
        })?;
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
        }
        let folder = NewSignInFolder::create(&root).map_err(|_| {
            DesktopError::control_message(
                "The connection's sign-in folder could not be created.".to_owned(),
            )
        })?;
        let declaration =
            self.claude_code_declaration(request.name, request.revision, folder.path(), prompt)?;
        confirm(
            prompt,
            &add_confirmation(&declaration, relay_folder.as_deref(), &self.inner.data_root),
        )?;
        let mut added = false;
        let mut program: Option<PathBuf> = None;
        let outcome = self.apply_reporting(|store| {
            let profile = store
                .add_claude_code_profile(&declaration, request.revision)
                .map_err(|error| DesktopError::from_control(&error))?;
            added = true;
            program = profile
                .claude_code_setup()
                .and_then(|setup| setup.live_program().ok());
            // A failed grant leaves the connection recorded; the error is
            // returned once the change is finished.
            let ungranted = relay_folder.as_deref().and_then(|relay| {
                store
                    .grant_claude_code(&declaration.name, relay, store.revision())
                    .err()
                    .map(|error| DesktopError::from_grant(&error))
            });
            Ok((AlsoWithdrawn::default(), ungranted))
        });
        if !added {
            // Dropping the folder removes it, because nothing records it.
            return outcome;
        }
        let folder = folder.keep();
        let command = terminal_command(program.as_deref().unwrap_or(&declaration.program), &folder);
        let overview = outcome.map_err(|error| {
            DesktopError::control_message(format!(
                "The connection was added, but its folders were not granted: {} Revoke its folders, grant them again, and then sign in from a terminal: {command}",
                error.message(),
            ))
        })?;
        if !SIGN_IN_WINDOW {
            return Ok(overview);
        }
        program
            .ok_or_else(|| "the recorded program could not be checked again".to_owned())
            .and_then(|program| {
                let sign_in =
                    ClaudeCodeSignIn::new(&program, &folder, &|name| std::env::var_os(name));
                prompt.start_claude_code_sign_in(&sign_in)
            })
            .map(|()| overview)
            .map_err(|reason| {
                DesktopError::control_message(format!(
                    "The connection was added, but Claude's sign-in did not open: {}. Sign in from a terminal: {command}",
                    display_safe_line(&reason),
                ))
            })
    }

    /// The declaration of `name` with the new sign-in `folder` and a program
    /// core accepts, checked against `revision`: the first program found on
    /// `PATH` or in the installer's folder that core accepts, or, when no
    /// program file is found there at all, the one the operator chooses in a
    /// native file dialog. When files are found but core refuses each, the
    /// first refusal is returned, so an invalid name or folder is reported
    /// rather than hidden behind a dialog.
    fn claude_code_declaration(
        &self,
        name: String,
        revision: u64,
        folder: &Path,
        prompt: &dyn SettingsPrompt,
    ) -> Result<ClaudeCodeDeclaration, DesktopError> {
        let host = &self.inner.claude_code;
        let mut declaration = ClaudeCodeDeclaration {
            name,
            program: PathBuf::new(),
            config_folder: folder.to_owned(),
        };
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, revision)?;
            let mut refused: Option<ControlError> = None;
            for candidate in
                claude_code_program_candidates(host.search_path.as_deref(), host.home.as_deref())
            {
                if !candidate.is_file() {
                    continue;
                }
                declaration.program = candidate;
                match store.check_claude_code_profile(&declaration) {
                    Ok(()) => return Ok(declaration),
                    Err(error) => {
                        refused.get_or_insert(error);
                    }
                }
            }
            if let Some(error) = refused {
                return Err(DesktopError::from_control(&error));
            }
        }
        declaration.program = prompt
            .pick_file(FilePurpose::ClaudeCodeProgram, None)
            .ok_or_else(DesktopError::cancelled)?;
        let store = self.inner.open_or_create_control()?;
        check_revision(&store, revision)?;
        store
            .check_claude_code_profile(&declaration)
            .map_err(|error| DesktopError::from_control(&error))?;
        Ok(declaration)
    }

    /// The folder of the `omgcode` executable beside the desktop
    /// application, which a Claude Code turn runs as its relay.
    fn claude_code_relay_folder(&self) -> Result<PathBuf, DesktopError> {
        self.inner
            .claude_code
            .relay
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                DesktopError::invalid_request(
                    "No omgcode executable sits beside the desktop application, so there is no relay folder to grant.",
                )
            })
    }

    /// `settings` only. Windows only. Grant a Claude Code connection's own
    /// AppContainer the folders its process needs, as `omgcode workspace
    /// claude-code-grant` does (ADR-0109, ADR-0111): read and execute on the
    /// `claude` program's folder, on the connection's runtime folder, and on
    /// the folder of this process's relay, and read and write on the sign-in
    /// folder. One native confirmation names the four folders. Add grants
    /// them already; this grants them again after a revocation.
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
        let relay_folder = self.claude_code_relay_folder()?;
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

/// The confirmation for adding a Claude Code connection: its program, its
/// new sign-in folder, on Windows the folders its sandbox is granted (the
/// relay folder `relay` and a runtime folder under `data_root`), and how the
/// operator signs in. Every value is display-safe.
fn add_confirmation(
    declaration: &ClaudeCodeDeclaration,
    relay: Option<&Path>,
    data_root: &Path,
) -> Confirmation {
    let setup = match relay {
        Some(relay) => {
            let runtime = claude_code_runtime_folder(data_root, Uuid::nil());
            format!(
                "Its sandbox may read and execute the folder that holds the program, the OMGCode folder {}, and its own runtime folder under {}, and read and write the sign-in folder. No other folder is granted.\n\nThen Claude's own sign-in (claude auth login) opens in a new window, outside the sandbox. Finish it in your browser.",
                shown(relay),
                shown(runtime.parent().unwrap_or(data_root)),
            )
        }
        None => format!(
            "After adding, sign in from a terminal:\n{}",
            terminal_command(&declaration.program, &declaration.config_folder),
        ),
    };
    Confirmation {
        title: "Add a Claude Code connection",
        message: format!(
            "Add the Claude Code connection {}?\n\nProgram: {}\nSign-in folder (new and empty): {}\n\n{setup}\n\nOMGCode starts the program only for that sign-in and for a session that chooses this connection. OMGCode never reads the sign-in.",
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
    fn the_add_confirmation_names_the_grants_or_the_terminal_command() {
        let root = std::env::temp_dir();
        let declaration = ClaudeCodeDeclaration {
            name: "claude".to_owned(),
            program: root.join("bin").join("claude"),
            config_folder: root.join("sign-in"),
        };
        let granted = add_confirmation(
            &declaration,
            Some(&root.join("omgcode")),
            &root.join("data"),
        );
        for part in [
            "claude auth login",
            "omgcode",
            "claude-code",
            "read and write",
            "sign-in",
        ] {
            assert!(
                granted.message.contains(part),
                "{part}: {}",
                granted.message
            );
        }
        assert!(granted.message.contains("OMGCode never reads the sign-in"));
        let terminal = add_confirmation(&declaration, None, &root.join("data"));
        assert!(
            terminal.message.contains("CLAUDE_CONFIG_DIR"),
            "{}",
            terminal.message
        );
        assert!(
            !terminal.message.contains("sandbox may read"),
            "{}",
            terminal.message
        );
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
