//! The `main` window's file tree (ADR-0083).
//!
//! [`DesktopService::workspace_entries`] lists ONE directory of the open
//! session's workspace. The workspace comes from the session's recorded,
//! registered workspace, never from the caller; the caller sends only relative
//! name components, which core validates before any file system access. The
//! listing is the operator's own view: it does not require the workspace's
//! `read` preference, is not a model action, is not journaled, and grants
//! nothing. It returns names and kinds only.

use omgcode_core::workspace_entries::{EntriesError, WorkspaceListing, list_workspace_entries};
use serde::Deserialize;
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::wire::SessionRequest;

/// Arguments of `workspace_entries`. `path` holds relative name components of
/// the directory to list; empty lists the workspace itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEntriesRequest {
    /// The open session, whose recorded workspace is listed.
    pub session_id: Uuid,
    /// Directory components below the workspace folder.
    pub path: Vec<String>,
}

impl DesktopService {
    /// List one directory of a session's registered workspace: names and kinds,
    /// directories first, bounded, with a `truncated` flag.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session, `unknown_workspace` when the
    /// session's workspace is not registered here, `invalid_request` for an
    /// unacceptable component or a directory that cannot be listed, `busy`,
    /// `control`, or `internal`.
    pub fn workspace_entries(
        &self,
        request: WorkspaceEntriesRequest,
    ) -> Result<WorkspaceListing, DesktopError> {
        let Some((store, workspace_id)) = self.session_workspace(SessionRequest {
            session_id: request.session_id,
        })?
        else {
            return Err(DesktopError::unknown_workspace());
        };
        let listing = list_workspace_entries(&store, workspace_id, &request.path);
        drop(store);
        listing.map_err(|error| DesktopError::from_entries(&error))
    }
}

impl DesktopError {
    /// Map a listing failure. Fixed text only: no path, no native error text.
    fn from_entries(error: &EntriesError) -> Self {
        match error {
            EntriesError::InvalidPath(_) => {
                Self::invalid_request("The folder path is not acceptable.")
            }
            EntriesError::Control(error) => Self::from_control(error),
            EntriesError::Changed => {
                Self::invalid_request("The workspace folder changed since it was registered.")
            }
            EntriesError::Native(_) => Self::invalid_request("The folder could not be listed."),
        }
    }
}
