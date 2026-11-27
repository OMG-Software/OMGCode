//! What a run path the workspace's AppContainer refused is for.
//!
//! The Windows runner refuses a run before anything starts when the
//! workspace's container cannot use one of the run's paths at the level the
//! run needs. It reports the path and its place in the confinement; this
//! module maps that back to the role core gave the path, so the refusal can
//! say which grant is missing. [`UngrantedRunPath`]'s `Display` is part of
//! [`super::RunError`]'s text, which a model receives as a tool result, so it
//! names the role, the toolchain profile, and the access, never the host
//! path. Trusted operator surfaces read [`UngrantedRunPath::path`]. See
//! ADR-0106.

use std::fmt;
use std::path::{Path, PathBuf};

use omgcode_platform::process::{ConfinementEntry, UngrantedPath};
use serde::Serialize;

use crate::control::ContainerGrantAccess;

/// Where the operator grants a container access, in the words of the other
/// run refusals (ADR-0104).
const WHERE_TO_GRANT: &str =
    "under Settings, Advanced, Container grants, or with `omgcode workspace container-grant`";

/// The role core gave a refused path when it built the run's confinement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefusedPathRole {
    /// The workspace folder, which every run needs read-write.
    Workspace,
    /// A read-only root of the run's toolchain profile, numbered from 1 in
    /// the order the profile records its roots, as the `settings` window and
    /// `workspace profile-show` list them.
    ProfileRoot {
        /// The profile's recorded name.
        profile: String,
        /// The root's position in the profile, from 1.
        number: usize,
    },
    /// The workspace's package cache root.
    PackageCache,
    /// The program file the profile resolved.
    Program,
    /// A path core did not list itself. No current run has one; it keeps a
    /// future path refused with its level rather than misnamed.
    Other,
}

/// A run that did not start because the workspace's AppContainer cannot use
/// one of its paths at the level the run needs (Windows only).
///
/// `Display` names the role and the access, never the path, because it is
/// sent to the model. [`Self::path`] is for trusted operator surfaces such as
/// the CLI's `refused_path`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UngrantedRunPath {
    role: RefusedPathRole,
    needed: ContainerGrantAccess,
    granted: Option<ContainerGrantAccess>,
    path: PathBuf,
}

impl UngrantedRunPath {
    /// What the refused path is for.
    #[must_use]
    pub fn role(&self) -> &RefusedPathRole {
        &self.role
    }

    /// The access the run needs on the path.
    #[must_use]
    pub fn needed(&self) -> ContainerGrantAccess {
        self.needed
    }

    /// The access the container has now, from a grant or because every
    /// application package has it; `None` when it has neither.
    #[must_use]
    pub fn granted(&self) -> Option<ContainerGrantAccess> {
        self.granted
    }

    /// The refused host path. Show it only on trusted operator surfaces;
    /// never add it to text a model receives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for UngrantedRunPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let needed = self.needed;
        let held = match self.granted {
            None => "no access".to_owned(),
            Some(access) => format!("only {access} access"),
        };
        match &self.role {
            RefusedPathRole::Workspace => write!(
                formatter,
                "the workspace's sandbox has {held} to the workspace folder, which needs \
                 {needed}, although a grant is recorded; its access-control entry may have \
                 been changed outside OMGCode. Revoke the grant and grant it again under \
                 Settings, Advanced, Container grants, or with `omgcode workspace \
                 container-revoke` and `omgcode workspace container-grant`"
            ),
            RefusedPathRole::ProfileRoot { profile, number } => write!(
                formatter,
                "the workspace's sandbox has {held} to root {number} of toolchain profile \
                 `{profile}`, which needs {needed}. The settings window and `omgcode \
                 workspace profile-show {profile}` list the profile's roots in order; grant \
                 that folder {needed} {WHERE_TO_GRANT}"
            ),
            RefusedPathRole::PackageCache => write!(
                formatter,
                "the workspace's sandbox has {held} to the workspace's package cache root, \
                 which needs {needed}; grant it {needed} {WHERE_TO_GRANT}"
            ),
            RefusedPathRole::Program => write!(
                formatter,
                "the workspace's sandbox has {held} to the program file the profile \
                 resolved, which needs {needed}; grant the folder that holds it {needed} \
                 {WHERE_TO_GRANT}"
            ),
            RefusedPathRole::Other => write!(
                formatter,
                "the workspace's sandbox has {held} to a path the run lists, which needs \
                 {needed}"
            ),
        }
    }
}

/// The paths core listed in a run's confinement, kept so a refusal can name
/// the role of the path it refused. Paths are in the native form the
/// confinement holds, so a refused path compares equal to one of them.
#[derive(Debug)]
pub(super) struct ListedPaths {
    /// The run's toolchain profile name.
    pub(super) profile: String,
    /// The profile's read-only roots, in recorded order.
    pub(super) roots: Vec<PathBuf>,
    /// The workspace's package cache root, when the run lists one.
    pub(super) package_cache: Option<PathBuf>,
    /// The workspace folder.
    pub(super) workspace: PathBuf,
}

impl ListedPaths {
    /// Describe a refusal the runner reported for this run.
    pub(super) fn refusal(&self, ungranted: UngrantedPath) -> UngrantedRunPath {
        UngrantedRunPath {
            role: self.role(ungranted.entry, &ungranted.path),
            needed: ungranted.needed.into(),
            granted: ungranted.granted.map(Into::into),
            path: ungranted.path,
        }
    }

    /// Match by path rather than by index: the lists hold other entries in a
    /// platform-specific order, and only the path says which one was meant.
    fn role(&self, entry: ConfinementEntry, path: &Path) -> RefusedPathRole {
        match entry {
            ConfinementEntry::Program => RefusedPathRole::Program,
            ConfinementEntry::ReadWrite(_) if path == self.workspace => RefusedPathRole::Workspace,
            ConfinementEntry::ReadWrite(_) => RefusedPathRole::Other,
            ConfinementEntry::ReadOnly(_) => {
                if let Some(index) = self.roots.iter().position(|root| root == path) {
                    RefusedPathRole::ProfileRoot {
                        profile: self.profile.clone(),
                        // At most 16 roots, so this cannot saturate.
                        number: index.saturating_add(1),
                    }
                } else if self.package_cache.as_deref() == Some(path) {
                    RefusedPathRole::PackageCache
                } else {
                    RefusedPathRole::Other
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omgcode_platform::container::ContainerAccess;

    /// A user's home with distinctive folder names. Nothing here is opened,
    /// so the paths need not exist; none of the names is a word the
    /// refusal text uses.
    fn listed() -> ListedPaths {
        let home = PathBuf::from(if cfg!(windows) {
            r"C:\Users\omgcode-test-user"
        } else {
            "/home/omgcode-test-user"
        });
        ListedPaths {
            profile: "rust".into(),
            roots: vec![
                home.join("sysroot-alpha"),
                home.join(".rustup-beta").join("toolchain-gamma"),
            ],
            package_cache: Some(home.join("pkgstore-delta")),
            workspace: home.join("devroot-epsilon").join("project-zeta"),
        }
    }

    fn ungranted(
        entry: ConfinementEntry,
        path: PathBuf,
        needed: ContainerAccess,
        granted: Option<ContainerAccess>,
    ) -> UngrantedPath {
        UngrantedPath {
            entry,
            path,
            needed,
            granted,
        }
    }

    /// Every role, the level it needs, and the access the container had.
    fn cases(listed: &ListedPaths) -> Vec<(UngrantedPath, RefusedPathRole)> {
        vec![
            (
                ungranted(
                    ConfinementEntry::ReadOnly(1),
                    listed.roots[1].clone(),
                    ContainerAccess::ReadExecute,
                    None,
                ),
                RefusedPathRole::ProfileRoot {
                    profile: "rust".into(),
                    number: 2,
                },
            ),
            (
                ungranted(
                    ConfinementEntry::ReadOnly(5),
                    listed.package_cache.clone().expect("cache"),
                    ContainerAccess::ReadExecute,
                    None,
                ),
                RefusedPathRole::PackageCache,
            ),
            (
                ungranted(
                    ConfinementEntry::ReadWrite(0),
                    listed.workspace.clone(),
                    ContainerAccess::ReadWrite,
                    Some(ContainerAccess::ReadExecute),
                ),
                RefusedPathRole::Workspace,
            ),
            (
                ungranted(
                    ConfinementEntry::ReadWrite(0),
                    listed.workspace.clone(),
                    ContainerAccess::ReadWrite,
                    None,
                ),
                RefusedPathRole::Workspace,
            ),
            (
                ungranted(
                    ConfinementEntry::Program,
                    listed.roots[1].join("bin-eta").join("tool-theta.exe"),
                    ContainerAccess::ReadExecute,
                    None,
                ),
                RefusedPathRole::Program,
            ),
            (
                ungranted(
                    ConfinementEntry::ReadOnly(3),
                    listed.workspace.join("other-iota"),
                    ContainerAccess::ReadExecute,
                    None,
                ),
                RefusedPathRole::Other,
            ),
            (
                ungranted(
                    ConfinementEntry::ReadWrite(1),
                    listed.roots[0].clone(),
                    ContainerAccess::ReadWrite,
                    Some(ContainerAccess::ReadExecute),
                ),
                RefusedPathRole::Other,
            ),
        ]
    }

    #[test]
    fn each_refused_path_is_named_by_its_role_and_level() {
        let listed = listed();
        for (reported, role) in cases(&listed) {
            let needed = ContainerGrantAccess::from(reported.needed);
            let granted = reported.granted.map(ContainerGrantAccess::from);
            let path = reported.path.clone();
            let refusal = listed.refusal(reported);
            assert_eq!(refusal.role(), &role, "{path:?}");
            assert_eq!(refusal.needed(), needed, "{role:?}");
            assert_eq!(refusal.granted(), granted, "{role:?}");
            assert_eq!(refusal.path(), path, "{role:?}");
        }
    }

    /// The text goes to the model, so it must say what to grant without the
    /// host path or any part of the user's folder.
    #[test]
    fn the_refusal_text_names_the_grant_but_never_the_path() {
        let listed = listed();
        for (reported, role) in cases(&listed) {
            let needed = ContainerGrantAccess::from(reported.needed).to_string();
            let refusal = listed.refusal(reported);
            let text = refusal.to_string();
            assert!(
                text.contains(&format!("needs {needed}")),
                "{role:?}: {text}"
            );
            for component in refusal.path().components() {
                if let std::path::Component::Normal(part) = component {
                    let part = part.to_string_lossy();
                    assert!(
                        !text.contains(part.as_ref()),
                        "{role:?} leaked {part:?}: {text}"
                    );
                }
            }
            let expected = match &role {
                RefusedPathRole::ProfileRoot { profile, number } => {
                    format!("root {number} of toolchain profile `{profile}`")
                }
                RefusedPathRole::Workspace => "the workspace folder".to_owned(),
                RefusedPathRole::PackageCache => "package cache root".to_owned(),
                RefusedPathRole::Program => "the program file".to_owned(),
                RefusedPathRole::Other => "a path the run lists".to_owned(),
            };
            assert!(text.contains(&expected), "{role:?}: {text}");
            if role != RefusedPathRole::Other {
                assert!(
                    text.contains("Container grants") && text.contains("container-grant"),
                    "{role:?} does not say where to grant: {text}"
                );
            }
        }
    }

    #[test]
    fn the_refusal_text_says_what_access_the_container_has() {
        let listed = listed();
        let weaker = listed.refusal(ungranted(
            ConfinementEntry::ReadWrite(0),
            listed.workspace.clone(),
            ContainerAccess::ReadWrite,
            Some(ContainerAccess::ReadExecute),
        ));
        assert!(
            weaker.to_string().contains(
                "has only read-execute access to the workspace folder, which needs read-write"
            ),
            "{weaker}"
        );
        let missing = listed.refusal(ungranted(
            ConfinementEntry::ReadOnly(0),
            listed.roots[0].clone(),
            ContainerAccess::ReadExecute,
            None,
        ));
        assert!(
            missing.to_string().contains(
                "has no access to root 1 of toolchain profile `rust`, which needs read-execute"
            ),
            "{missing}"
        );
    }

    #[test]
    fn the_role_serializes_with_its_kind() {
        let role = RefusedPathRole::ProfileRoot {
            profile: "rust".into(),
            number: 2,
        };
        assert_eq!(
            serde_json::to_value(&role).expect("role JSON"),
            serde_json::json!({"kind": "profile_root", "profile": "rust", "number": 2})
        );
        assert_eq!(
            serde_json::to_value(RefusedPathRole::PackageCache).expect("role JSON"),
            serde_json::json!({"kind": "package_cache"})
        );
    }
}
