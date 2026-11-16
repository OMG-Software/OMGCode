//! Trusted application storage location.

use std::path::PathBuf;

use directories::ProjectDirs;

use crate::error::{AppError, ErrorKind};

/// Choose trusted application storage.
///
/// A supplied override is returned as given; it must come from the trusted
/// operator, never from repository content. Without one, the per-user local
/// data directory for OMGCode is used. No directory is created or checked here.
/// Fails with `configuration` when the platform has no per-user data directory.
pub fn data_root(override_path: Option<PathBuf>) -> Result<PathBuf, AppError> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    ProjectDirs::from("org", "OMG-Software", "OMGCode")
        .map(|directories| directories.data_local_dir().to_owned())
        .ok_or_else(|| {
            AppError::new(
                ErrorKind::Configuration,
                "No per-user application data directory",
            )
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn an_override_is_returned_unchanged() {
        let path = PathBuf::from("trusted-override");
        assert_eq!(data_root(Some(path.clone())).expect("override"), path);
    }

    #[test]
    fn the_default_is_the_per_user_omgcode_directory() {
        // Hosts without a home directory legitimately fail closed.
        if let Ok(path) = data_root(None) {
            assert!(path.is_absolute());
            // The platform convention decides the spelling: Windows keeps
            // `OMG-Software\OMGCode`, while Linux XDG paths use `omgcode`.
            assert!(
                path.to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("omgcode")
            );
        }
    }
}
