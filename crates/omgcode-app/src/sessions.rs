//! Session creation with current workspace registry metadata.

use std::path::Path;

use omgcode_core::control::ControlStore;
use omgcode_core::session::SessionJournal;

use crate::error::{AppError, ErrorKind};

/// Create a session with current registry metadata when a trusted root is configured.
///
/// An absent or valid unconfigured store preserves legacy session creation. Corrupt
/// control state or an invalid workspace under a configured root fails closed with
/// `control`; journal failures are `session`. The control store is opened and
/// closed inside this call and is never held while the journal is created.
pub fn create_session(
    data_root: &Path,
    workspace: &Path,
    title: &str,
) -> Result<SessionJournal, AppError> {
    let control_error = |error: omgcode_core::control::ControlError| {
        AppError::new(ErrorKind::Control, error.to_string())
    };
    let registration_id = match ControlStore::open_existing(data_root).map_err(control_error)? {
        Some(mut store) if store.has_development_root() => {
            let registration = store
                .register_workspace(workspace, store.revision())
                .map_err(control_error)?;
            let id = registration.id();
            drop(store);
            Some(id)
        }
        Some(store) => {
            drop(store);
            None
        }
        None => None,
    };

    match registration_id {
        Some(id) => SessionJournal::create_with_workspace_id(data_root, workspace, title, id),
        None => SessionJournal::create(data_root, workspace, title),
    }
    .map_err(AppError::session)
}
