//! Registered workspaces and session creation.
//!
//! A new session can be created only in a workspace the operator registered
//! with the trusted CLI. The caller names it by identifier; the path comes from
//! the settings store, never from the caller.

use super::DesktopService;
use super::error::DesktopError;
use super::sessions::refreshed_summary;
use super::state::folder_of_directory;
use super::wire::{SessionCreateRequest, SessionSummary, WorkspaceChoice, WorkspaceList};
use crate::create_session;

impl DesktopService {
    /// List the workspaces the operator registered, with their preferences.
    ///
    /// Without a settings store the list is empty and `development_root_set` is
    /// false. The store is opened and released inside this call.
    ///
    /// # Errors
    ///
    /// `busy` when the settings store stays held past a two-second wait,
    /// `control` when it is invalid.
    pub fn workspace_list(&self) -> Result<WorkspaceList, DesktopError> {
        let Some(store) = self.inner.open_control()? else {
            return Ok(WorkspaceList {
                development_root_set: false,
                workspaces: Vec::new(),
            });
        };
        let development_root_set = store.has_development_root();
        let workspaces = store
            .workspaces()
            .map(|workspace| {
                let permissions = workspace.permissions();
                WorkspaceChoice {
                    workspace_id: *workspace.id().as_uuid(),
                    folder_display: folder_of_directory(workspace.directory_path()),
                    model_tools: permissions.model_tools,
                    read: permissions.read,
                    write: permissions.write,
                    execute: permissions.execute,
                }
            })
            .collect();
        drop(store);
        Ok(WorkspaceList {
            development_root_set,
            workspaces,
        })
    }

    /// Create a session in a registered workspace.
    ///
    /// Core validates the title. The session is created exactly as the trusted
    /// CLI creates one, its journal is released, the index learns about it, and
    /// its summary is returned.
    ///
    /// # Errors
    ///
    /// `unknown_workspace` when the identifier is not registered,
    /// `invalid_request` for an unusable title or a workspace path this system
    /// cannot represent, `busy`, `control`, or `session`.
    pub fn session_create(
        &self,
        request: SessionCreateRequest,
    ) -> Result<SessionSummary, DesktopError> {
        let inner = &self.inner;
        let directory = {
            let store = inner
                .open_control()?
                .ok_or_else(DesktopError::unknown_workspace)?;
            let found = store
                .workspaces()
                .find(|workspace| *workspace.id().as_uuid() == request.workspace_id)
                .map(|workspace| workspace.directory_path());
            drop(store);
            found
        };
        let path = match directory {
            None => return Err(DesktopError::unknown_workspace()),
            Some(None) => {
                return Err(DesktopError::invalid_request(
                    "The workspace path is not usable on this system.",
                ));
            }
            Some(Some(path)) => path,
        };
        let journal = create_session(&inner.data_root, &path, &request.title)
            .map_err(|error| DesktopError::from_create(&error))?;
        let session_id = journal.snapshot().session_id;
        drop(journal);
        refreshed_summary(inner, session_id)
    }
}
