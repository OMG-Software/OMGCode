//! Claude Code connections (ADR-0109).
//!
//! A `claude_code` provider profile records the operator's own `claude`
//! program and the dedicated folder the operator signed in to with
//! `claude auth login`. OMGCode never opens a file in that folder: the
//! record is identity evidence used to set `CLAUDE_CONFIG_DIR` for the child
//! and, on Windows, to grant the connection's own AppContainer access to it.
//!
//! On Windows the child runs in an AppContainer named for the profile, never
//! in a workspace container, so a model-run program in a workspace cannot
//! reach the sign-in folder. Each grant that container needs is recorded
//! before and after its ACL change, like a workspace grant, and every grant
//! is revoked before the profile can be removed.

use std::io;
use std::path::{Path, PathBuf};

use omgcode_platform::container::{self as platform, ContainerName};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::container::{ContainerGrantAccess, ContainerGrantState, valid_sid};
use super::provider::{MAX_PROVIDER_PROFILES, ProviderAuth, ProviderKind, ProviderProfile};
use super::{Action, ControlError, ControlSnapshot, ControlStore, valid_profile_name, valid_uuid};
use crate::session::NativePath;
use crate::workspace::DirectoryRecord;

/// Prefix of every Claude Code connection container name.
const CONTAINER_PREFIX: &str = "OMGCode.ClaudeCode.";
/// The folder in application storage that holds each connection's runtime
/// folder.
const RUNTIME_PARENT: &str = "claude-code";
/// One grant per role, so at most four; the bound also limits replay.
const MAX_GRANTS_PER_PROFILE: usize = 4;
/// Every Claude Code grant across all profiles.
const MAX_GRANTS: usize = 64;

/// The program and sign-in folder a `claude_code` profile records.
///
/// The paths are descriptive configuration. A launch rechecks the live
/// identity of each folder; this record authorizes no file access.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeCodeSetup {
    /// The canonical path of the operator's `claude` program.
    program: NativePath,
    /// The folder that holds the program, granted read-execute on Windows.
    program_folder: DirectoryRecord,
    /// The operator's dedicated Claude configuration folder, passed to the
    /// child as `CLAUDE_CONFIG_DIR`. OMGCode never opens a file in it.
    config_folder: DirectoryRecord,
}

impl ClaudeCodeSetup {
    /// The recorded program path for this host.
    ///
    /// # Errors
    ///
    /// When the recorded encoding is not this host's or the path is not
    /// absolute.
    pub fn program(&self) -> io::Result<PathBuf> {
        let path = self
            .program
            .to_path_buf()
            .map_err(|_| invalid("the recorded program path does not match this host"))?;
        if !path.is_absolute() {
            return Err(invalid("the recorded program path is not absolute"));
        }
        Ok(path)
    }

    /// The program path as display text, for people only.
    pub fn program_display(&self) -> String {
        self.program.to_path_buf().map_or_else(
            |_| String::from("(path not representable on this system)"),
            |path| crate::turn::display_safe_line(&path.to_string_lossy()),
        )
    }

    /// The configuration folder as display text, for people only.
    pub fn config_folder_display(&self) -> String {
        self.config_folder.display()
    }

    /// The configuration folder's live path, after checking that it is still
    /// the recorded folder.
    ///
    /// # Errors
    ///
    /// [`ControlError::DirectoryChanged`] when the folder moved or was
    /// replaced, or the I/O error that stopped the check.
    pub fn live_config_folder(&self) -> Result<PathBuf, ControlError> {
        if !self.config_folder.is_current()? {
            return Err(ControlError::DirectoryChanged);
        }
        Ok(self.config_folder.live_path()?)
    }

    /// The program's live path, after checking that its folder is still the
    /// recorded folder and the program is still a regular file in it.
    ///
    /// # Errors
    ///
    /// [`ControlError::DirectoryChanged`] when the folder or program changed,
    /// or the I/O error that stopped the check.
    pub fn live_program(&self) -> Result<PathBuf, ControlError> {
        if !self.program_folder.is_current()? {
            return Err(ControlError::DirectoryChanged);
        }
        let program = self.program()?;
        let folder = self.program_folder.live_path()?;
        // The program was canonical when recorded; a link or junction placed
        // at its path since then resolves elsewhere and is refused.
        let canonical = std::fs::canonicalize(&program)?;
        if canonical != program
            || canonical.parent() != Some(folder.as_path())
            || !std::fs::metadata(&canonical)?.is_file()
        {
            return Err(ControlError::DirectoryChanged);
        }
        Ok(canonical)
    }

    /// Check persisted fields without touching the filesystem.
    fn validate_shape(&self) -> bool {
        let program_valid = match &self.program {
            NativePath::UnixBytes(units) => !units.is_empty() && !units.contains(&0),
            NativePath::WindowsUtf16(units) => !units.is_empty() && !units.contains(&0),
        };
        program_valid && self.program_folder.validate_shape() && self.config_folder.validate_shape()
    }
}

/// The operator's declaration of a Claude Code connection, before checks.
#[derive(Clone, Debug, Default)]
pub struct ClaudeCodeDeclaration {
    /// The profile name, with the same rules as other provider profiles.
    pub name: String,
    /// The operator's `claude` program.
    pub program: PathBuf,
    /// The dedicated folder the operator signed in to with
    /// `CLAUDE_CONFIG_DIR=<folder> claude auth login`.
    pub config_folder: PathBuf,
}

/// Why a Claude Code container holds a grant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeCodeGrantRole {
    /// The folder that holds the `claude` program: read-execute.
    Program,
    /// The sign-in folder: read-write, because the CLI keeps its own state
    /// and rotates its tokens there.
    Configuration,
    /// OMGCode's runtime folder for the connection, which holds each turn's
    /// prompt, settings, and empty working folder: read-execute.
    Runtime,
    /// The folder that holds the OMGCode relay program: read-execute.
    Relay,
}

impl ClaudeCodeGrantRole {
    /// Every role, in the order grants are made.
    pub const ALL: [Self; 4] = [
        Self::Program,
        Self::Configuration,
        Self::Runtime,
        Self::Relay,
    ];

    /// The access this role needs.
    pub const fn access(self) -> ContainerGrantAccess {
        match self {
            Self::Configuration => ContainerGrantAccess::ReadWrite,
            Self::Program | Self::Runtime | Self::Relay => ContainerGrantAccess::ReadExecute,
        }
    }

    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Program => "program",
            Self::Configuration => "configuration",
            Self::Runtime => "runtime",
            Self::Relay => "relay",
        }
    }
}

/// One recorded grant to a Claude Code connection's container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeCodeGrant {
    id: Uuid,
    profile_id: Uuid,
    role: ClaudeCodeGrantRole,
    container: String,
    sid: String,
    directory: DirectoryRecord,
}

impl ClaudeCodeGrant {
    /// Stable grant identifier.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// The profile whose container holds the grant.
    pub const fn profile_id(&self) -> Uuid {
        self.profile_id
    }

    /// Why the grant exists.
    pub const fn role(&self) -> ClaudeCodeGrantRole {
        self.role
    }

    /// The granted folder as display text, for people only.
    pub fn directory_display(&self) -> String {
        self.directory.display()
    }
}

/// A Claude Code grant and its lifecycle state.
#[derive(Clone, Debug, Serialize)]
pub struct ClaudeCodeGrantStatus {
    #[serde(flatten)]
    grant: ClaudeCodeGrant,
    state: ContainerGrantState,
}

impl ClaudeCodeGrantStatus {
    /// The recorded grant.
    pub const fn grant(&self) -> &ClaudeCodeGrant {
        &self.grant
    }

    /// Its lifecycle state.
    pub const fn state(&self) -> ContainerGrantState {
        self.state
    }
}

/// The result of revoking a connection's grants.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ClaudeCodeRevocation {
    /// How many grants were revoked.
    pub revoked: usize,
    /// Whether the container profile was deleted after the last grant.
    pub profile_deleted: bool,
}

/// The AppContainer name for one Claude Code connection:
/// `OMGCode.ClaudeCode.` and the profile ID as 32 hex digits.
///
/// # Errors
///
/// When the platform refuses the name.
pub fn claude_code_container(profile_id: Uuid) -> Result<ContainerName, ControlError> {
    ContainerName::new(&format!("{CONTAINER_PREFIX}{}", profile_id.simple()))
        .map_err(|_| ControlError::Invalid("Claude Code container name is invalid"))
}

/// The connection's runtime folder under the application data folder
/// `data_root`: `claude-code/<profile ID>`.
pub fn claude_code_runtime_folder(data_root: &Path, profile_id: Uuid) -> PathBuf {
    data_root
        .join(RUNTIME_PARENT)
        .join(profile_id.simple().to_string())
}

impl ControlStore {
    /// Run every check [`Self::add_claude_code_profile`] applies, except the
    /// revision, without recording anything.
    ///
    /// # Errors
    ///
    /// As [`Self::add_claude_code_profile`].
    pub fn check_claude_code_profile(
        &self,
        declaration: &ClaudeCodeDeclaration,
    ) -> Result<(), ControlError> {
        self.new_claude_code_profile(declaration).map(|_| ())
    }

    /// Record a Claude Code connection: the operator's program and sign-in
    /// folder (ADR-0109). This records no credential and grants nothing;
    /// [`Self::grant_claude_code`] grants the container on Windows.
    ///
    /// # Errors
    ///
    /// A revision conflict; [`ControlError::ProviderRefused`] for a duplicate
    /// or invalid name, a program that is not a regular file (or, on
    /// Windows, not an `.exe`), or a folder that is too broad or overlaps
    /// application storage, the development root, or a workspace.
    pub fn add_claude_code_profile(
        &mut self,
        declaration: &ClaudeCodeDeclaration,
        expected_revision: u64,
    ) -> Result<ProviderProfile, ControlError> {
        self.check_revision(expected_revision)?;
        let profile = self.new_claude_code_profile(declaration)?;
        self.append(
            Action::ProviderProfileAdded {
                profile: profile.clone(),
            },
            self.next_revision()?,
        )?;
        Ok(profile)
    }

    fn new_claude_code_profile(
        &self,
        declaration: &ClaudeCodeDeclaration,
    ) -> Result<ProviderProfile, ControlError> {
        self.ensure_healthy()?;
        if self.provider_profiles().len() >= MAX_PROVIDER_PROFILES {
            return Err(ControlError::ProviderRefused(
                "at most 32 provider profiles may be recorded",
            ));
        }
        if !valid_profile_name(&declaration.name) {
            return Err(ControlError::ProviderRefused(
                "provider profile names are 1 to 64 ASCII letters, digits, '.', '-', or '_', starting with a letter or digit",
            ));
        }
        if self.provider_profile(&declaration.name).is_some() {
            return Err(ControlError::ProviderRefused(
                "a provider profile with this name already exists; remove it first",
            ));
        }
        let program = std::fs::canonicalize(&declaration.program).map_err(|_| {
            ControlError::ProviderRefused("the Claude Code program cannot be found")
        })?;
        if !std::fs::metadata(&program).is_ok_and(|metadata| metadata.is_file()) {
            return Err(ControlError::ProviderRefused(
                "the Claude Code program must be a regular file",
            ));
        }
        if cfg!(windows)
            && !program
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Err(ControlError::ProviderRefused(
                "the Claude Code program must be an .exe file",
            ));
        }
        let program_folder_path = program.parent().ok_or(ControlError::ProviderRefused(
            "the Claude Code program has no folder",
        ))?;
        let program_folder = DirectoryRecord::capture(program_folder_path)?;
        self.check_readable_folder(&program_folder)?;
        let config_folder = DirectoryRecord::capture(&declaration.config_folder).map_err(|_| {
            ControlError::ProviderRefused(
                "the Claude sign-in folder must be an existing folder; create it and sign in with `claude auth login` first",
            )
        })?;
        self.check_config_folder(&config_folder)?;
        if config_folder.same_directory(&program_folder) {
            return Err(ControlError::ProviderRefused(
                "the Claude sign-in folder must not be the program's folder",
            ));
        }
        let program = NativePath::from_path(&program).map_err(|_| {
            ControlError::ProviderRefused("the Claude Code program path cannot be recorded")
        })?;
        let profile = ProviderProfile::claude_code(
            Uuid::new_v4(),
            declaration.name.clone(),
            ClaudeCodeSetup {
                program,
                program_folder,
                config_folder,
            },
        );
        super::provider::validate_profile(&profile)?;
        Ok(profile)
    }

    /// Every recorded Claude Code grant that is not confirmed revoked.
    pub fn claude_code_grants(&self) -> &[ClaudeCodeGrantStatus] {
        &self.snapshot.claude_code_grants
    }

    /// Grant the connection's container each folder its child needs that it
    /// has no active grant for: the program folder, the sign-in folder, the
    /// runtime folder (created here if missing), and `relay_folder`, which
    /// holds the OMGCode relay program. Windows only.
    ///
    /// Each intent is synced before its ACL change. A failed change leaves
    /// that grant pending, and [`Self::revoke_claude_code_grants`] must run
    /// before another grant for the profile.
    ///
    /// # Errors
    ///
    /// A revision conflict; [`ControlError::UnknownProvider`] or
    /// [`ControlError::ProviderRefused`] for a profile that is not a Claude
    /// Code connection; [`ControlError::DirectoryChanged`] when a recorded
    /// folder changed; [`ControlError::GrantRefused`] for a folder that is too
    /// broad or overlaps storage, the development root, or a workspace, or
    /// while a grant is pending or revoking; the platform error otherwise.
    pub fn grant_claude_code(
        &mut self,
        name: &str,
        relay_folder: &Path,
        expected_revision: u64,
    ) -> Result<Vec<ClaudeCodeGrant>, ControlError> {
        self.check_revision(expected_revision)?;
        if !omgcode_platform::container::CONTAINER_GRANTS_SUPPORTED {
            return Err(ControlError::Container(io::Error::new(
                io::ErrorKind::Unsupported,
                "container grants exist only on Windows",
            )));
        }
        let (profile_id, setup) = self.claude_code_setup(name)?;
        if self.snapshot.claude_code_grants.iter().any(|status| {
            status.grant.profile_id == profile_id && status.state != ContainerGrantState::Active
        }) {
            return Err(ControlError::GrantRefused(
                "a grant for this connection is pending or revoking; revoke the connection's grants first",
            ));
        }
        let runtime_path =
            claude_code_runtime_folder(&self.data_directory.live_path()?, profile_id);
        std::fs::create_dir_all(&runtime_path)?;
        let runtime = DirectoryRecord::capture(&runtime_path)?;
        if !self.data_directory.contains(&runtime)? {
            return Err(ControlError::GrantRefused(
                "the runtime folder must be inside application storage",
            ));
        }
        let relay = DirectoryRecord::capture(relay_folder)?;
        self.check_readable_folder(&relay)?;
        if !setup.program_folder.is_current()? || !setup.config_folder.is_current()? {
            return Err(ControlError::DirectoryChanged);
        }
        self.check_readable_folder(&setup.program_folder)?;
        self.check_config_folder(&setup.config_folder)?;
        let planned = [
            (ClaudeCodeGrantRole::Program, setup.program_folder.clone()),
            (
                ClaudeCodeGrantRole::Configuration,
                setup.config_folder.clone(),
            ),
            (ClaudeCodeGrantRole::Runtime, runtime),
            (ClaudeCodeGrantRole::Relay, relay),
        ];
        let name = claude_code_container(profile_id)?;
        let sid = platform::container_sid(&name).map_err(ControlError::Container)?;
        let mut granted = Vec::new();
        for (role, directory) in planned {
            let existing =
                self.snapshot.claude_code_grants.iter().find(|status| {
                    status.grant.profile_id == profile_id && status.grant.role == role
                });
            match existing {
                Some(status) if status.grant.directory.same_directory(&directory) => continue,
                Some(_) => {
                    return Err(ControlError::GrantRefused(
                        "a folder this connection uses changed since it was granted; revoke the connection's grants first",
                    ));
                }
                None => {}
            }
            // Read-execute folders are shared by several roles only in odd
            // setups; one ACE per folder keeps revocation exact.
            if granted
                .iter()
                .any(|earlier: &ClaudeCodeGrant| earlier.directory.same_directory(&directory))
                || self.snapshot.claude_code_grants.iter().any(|status| {
                    status.grant.profile_id == profile_id
                        && status.grant.directory.same_directory(&directory)
                })
            {
                return Err(ControlError::GrantRefused(
                    "two roles of this connection name the same folder",
                ));
            }
            let grant = ClaudeCodeGrant {
                id: Uuid::new_v4(),
                profile_id,
                role,
                container: name.as_str().to_owned(),
                sid: sid.clone(),
                directory,
            };
            self.append(
                Action::ClaudeCodeGrantRequested {
                    grant: grant.clone(),
                },
                self.next_revision()?,
            )?;
            let live = grant.directory.live_path()?;
            platform::grant(
                &live,
                grant.directory.native_identity(),
                &name,
                role.access().into(),
            )
            .map_err(ControlError::Container)?;
            self.append(
                Action::ClaudeCodeGrantApplied { grant_id: grant.id },
                self.revision(),
            )?;
            granted.push(grant);
        }
        Ok(granted)
    }

    /// Revoke every grant of one Claude Code connection, then delete its
    /// container profile. A pending grant is revoked too: its ACL change may
    /// or may not have happened, and revoking an absent entry succeeds.
    ///
    /// # Errors
    ///
    /// [`ControlError::UnknownProvider`] for an unknown profile, the platform
    /// error that stopped a revocation (later grants stay recorded), or
    /// [`ControlError::ProfileRemoval`] when every grant was revoked but the
    /// profile could not be deleted.
    pub fn revoke_claude_code_grants(
        &mut self,
        name: &str,
    ) -> Result<ClaudeCodeRevocation, ControlError> {
        self.ensure_healthy()?;
        let profile_id = self
            .provider_profile(name)
            .map(|status| status.profile().id())
            .ok_or(ControlError::UnknownProvider)?;
        let grants: Vec<ClaudeCodeGrantStatus> = self
            .snapshot
            .claude_code_grants
            .iter()
            .filter(|status| status.grant.profile_id == profile_id)
            .cloned()
            .collect();
        let mut revoked = 0;
        for status in &grants {
            let grant_id = status.grant.id;
            let container = ContainerName::new(&status.grant.container)
                .map_err(|_| ControlError::Invalid("recorded container name is invalid"))?;
            if status.state != ContainerGrantState::Revoking {
                self.append(
                    Action::ClaudeCodeRevokeRequested { grant_id },
                    self.revision(),
                )?;
            }
            let live = status.grant.directory.live_path()?;
            platform::revoke(&live, status.grant.directory.native_identity(), &container)
                .map_err(ControlError::Container)?;
            self.append(Action::ClaudeCodeGrantRevoked { grant_id }, self.revision())?;
            revoked += 1;
        }
        let profile_deleted = if grants.is_empty() {
            false
        } else {
            let container = claude_code_container(profile_id)?;
            platform::delete_profile(&container).map_err(ControlError::ProfileRemoval)?;
            true
        };
        Ok(ClaudeCodeRevocation {
            revoked,
            profile_deleted,
        })
    }

    /// Resolve the paths one Claude Code run uses, after checking that the
    /// recorded folders are still the same folders and, on Windows, that
    /// the connection's container holds an active grant for each role,
    /// including the folder that holds `relay_program`.
    ///
    /// On Linux the runtime folder is created, private to the user, when
    /// missing; no grant exists there.
    ///
    /// # Errors
    ///
    /// [`ControlError::UnknownProvider`] or [`ControlError::ProviderRefused`]
    /// for an unknown or other profile, [`ControlError::DirectoryChanged`]
    /// when a recorded folder changed, [`ControlError::GrantRefused`] when a
    /// grant is missing, pending, or for another folder, and the I/O error
    /// that stopped a check.
    pub fn resolve_claude_code(
        &self,
        name: &str,
        relay_program: &Path,
    ) -> Result<ClaudeCodePaths, ControlError> {
        self.ensure_healthy()?;
        let (profile_id, setup) = self.claude_code_setup(name)?;
        let program = setup.live_program()?;
        let config_folder = setup.live_config_folder()?;
        let relay_program = std::fs::canonicalize(relay_program)?;
        let relay_folder = relay_program.parent().ok_or(ControlError::GrantRefused(
            "the relay program has no folder",
        ))?;
        let runtime_folder =
            claude_code_runtime_folder(&self.data_directory.live_path()?, profile_id);
        let container = if platform_grants_supported() {
            const MISSING: ControlError = ControlError::GrantRefused(
                "the Claude Code connection's container lacks an active grant for one of its folders; run `workspace claude-code-grant` first",
            );
            let active = |role: ClaudeCodeGrantRole| {
                self.snapshot
                    .claude_code_grants
                    .iter()
                    .filter(move |status| {
                        status.grant.profile_id == profile_id
                            && status.grant.role == role
                            && status.state == ContainerGrantState::Active
                    })
            };
            // Every role is checked before any folder is opened: the runtime
            // folder exists only once it has been granted.
            if ClaudeCodeGrantRole::ALL
                .iter()
                .any(|role| active(*role).next().is_none())
            {
                return Err(MISSING);
            }
            let runtime = DirectoryRecord::capture(&runtime_folder)?;
            let relay = DirectoryRecord::capture(relay_folder)?;
            for (role, expected) in [
                (ClaudeCodeGrantRole::Program, &setup.program_folder),
                (ClaudeCodeGrantRole::Configuration, &setup.config_folder),
                (ClaudeCodeGrantRole::Runtime, &runtime),
                (ClaudeCodeGrantRole::Relay, &relay),
            ] {
                if !active(role).any(|status| status.grant.directory.same_directory(expected)) {
                    return Err(MISSING);
                }
            }
            Some(claude_code_container(profile_id)?)
        } else {
            create_private_folder(&runtime_folder)?;
            None
        };
        Ok(ClaudeCodePaths {
            profile_id,
            program,
            config_folder,
            runtime_folder,
            relay_program,
            container,
        })
    }

    /// The profile ID and setup of a recorded Claude Code connection.
    ///
    /// # Errors
    ///
    /// [`ControlError::UnknownProvider`] or [`ControlError::ProviderRefused`].
    pub fn claude_code_setup(&self, name: &str) -> Result<(Uuid, ClaudeCodeSetup), ControlError> {
        let status = self
            .provider_profile(name)
            .ok_or(ControlError::UnknownProvider)?;
        let setup = status
            .profile()
            .claude_code_setup()
            .ok_or(ControlError::ProviderRefused(
                "this provider profile is not a claude_code connection",
            ))?;
        Ok((status.profile().id(), setup.clone()))
    }

    /// Refuse a folder whose read-execute grant would reach too much: a
    /// filesystem or drive root, the home folder or one of its ancestors, or
    /// a folder that holds application storage, the development root, or a
    /// registered workspace. A folder inside a workspace, such as a build
    /// output folder that holds the relay, is allowed.
    fn check_readable_folder(&self, directory: &DirectoryRecord) -> Result<(), ControlError> {
        check_not_broad(directory)?;
        if directory.contains(&self.data_directory)? {
            return Err(ControlError::GrantRefused(
                "a Claude Code folder must not contain application storage",
            ));
        }
        if let Some(root) = &self.snapshot.development_root
            && directory.contains(root)?
        {
            return Err(ControlError::GrantRefused(
                "a Claude Code folder must not contain the development root",
            ));
        }
        for workspace in &self.snapshot.workspaces {
            if directory.contains(&workspace.directory)? {
                return Err(ControlError::GrantRefused(
                    "a Claude Code folder must not contain a workspace",
                ));
            }
        }
        Ok(())
    }

    /// Refuse a sign-in folder that is too broad or that any other part of
    /// OMGCode can reach: it must not overlap application storage, the
    /// development root, or any workspace, so no model file tool can read it.
    fn check_config_folder(&self, directory: &DirectoryRecord) -> Result<(), ControlError> {
        check_not_broad(directory)?;
        if directory.contains(&self.data_directory)? || self.data_directory.contains(directory)? {
            return Err(ControlError::ProviderRefused(
                "the Claude sign-in folder must not overlap application storage",
            ));
        }
        if let Some(root) = &self.snapshot.development_root
            && (directory.contains(root)? || root.contains(directory)?)
        {
            return Err(ControlError::ProviderRefused(
                "the Claude sign-in folder must be outside the development root",
            ));
        }
        for workspace in &self.snapshot.workspaces {
            if directory.contains(&workspace.directory)?
                || workspace.directory.contains(directory)?
            {
                return Err(ControlError::ProviderRefused(
                    "the Claude sign-in folder must not overlap a workspace",
                ));
            }
        }
        Ok(())
    }
}

/// The paths one Claude Code run uses, resolved and checked by
/// [`ControlStore::resolve_claude_code`]. They are live paths for one launch,
/// not authority.
#[derive(Clone, Debug)]
pub struct ClaudeCodePaths {
    /// The connection's profile identifier.
    pub profile_id: Uuid,
    /// The canonical `claude` program.
    pub program: PathBuf,
    /// The sign-in folder.
    pub config_folder: PathBuf,
    /// The connection's runtime folder in application storage.
    pub runtime_folder: PathBuf,
    /// The canonical relay program.
    pub relay_program: PathBuf,
    /// The connection's AppContainer, on Windows.
    pub container: Option<ContainerName>,
}

const fn platform_grants_supported() -> bool {
    omgcode_platform::container::CONTAINER_GRANTS_SUPPORTED
}

/// Create `path` and its parents with mode `0700` on Unix, or plainly
/// elsewhere, when missing.
fn create_private_folder(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Refuse a filesystem or drive root and the home folder or its ancestors.
fn check_not_broad(directory: &DirectoryRecord) -> Result<(), ControlError> {
    let path = directory.live_path()?;
    if path.parent().is_none() {
        return Err(ControlError::GrantRefused(
            "a Claude Code folder must not be a filesystem or drive root",
        ));
    }
    let home_variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Some(home) = std::env::var_os(home_variable)
        && let Ok(home) = std::fs::canonicalize(home)
        && home.starts_with(&path)
    {
        return Err(ControlError::GrantRefused(
            "a Claude Code folder must not be the home folder or one of its ancestors",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Whether `profile` is a well-formed recorded Claude Code connection: the
/// kind, `subscription`, no endpoint, and a setup whose fields are valid.
/// Every other kind must not carry a setup.
pub(super) fn valid_profile_shape(profile: &ProviderProfile) -> bool {
    match (profile.kind(), profile.claude_code_setup()) {
        (ProviderKind::ClaudeCode, Some(setup)) => {
            profile.auth() == ProviderAuth::Subscription
                && profile.endpoint().is_none()
                && setup.validate_shape()
        }
        (ProviderKind::ClaudeCode, None) | (_, Some(_)) => false,
        (_, None) => true,
    }
}

/// Events and bytes that must stay free so every outstanding Claude Code
/// grant can still reach a confirmed revocation.
pub(super) fn grant_reserve_events(snapshot: &ControlSnapshot) -> usize {
    snapshot
        .claude_code_grants
        .iter()
        .map(|status| match status.state {
            ContainerGrantState::Pending => 3,
            ContainerGrantState::Active => 2,
            ContainerGrantState::Revoking => 1,
        })
        .sum()
}

/// Whether an action moves a Claude Code grant toward revocation.
pub(super) const fn uses_grant_reserve(action: &Action) -> bool {
    matches!(
        action,
        Action::ClaudeCodeGrantApplied { .. }
            | Action::ClaudeCodeRevokeRequested { .. }
            | Action::ClaudeCodeGrantRevoked { .. }
    )
}

/// Validate and apply one Claude Code grant action during replay or append
/// preflight.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    match action {
        Action::ClaudeCodeGrantRequested { grant } => {
            let profile = state
                .provider_profiles
                .iter()
                .find(|status| status.profile().id() == grant.profile_id)
                .ok_or(ControlError::Invalid(
                    "grant for an unknown provider profile",
                ))?;
            let per_profile = state
                .claude_code_grants
                .iter()
                .filter(|status| status.grant.profile_id == grant.profile_id)
                .count();
            if profile.profile().kind() != ProviderKind::ClaudeCode
                || !valid_uuid(grant.id)
                || grant.container != claude_code_container(grant.profile_id)?.as_str()
                || !valid_sid(&grant.sid)
                || !grant.directory.validate_shape()
                || per_profile >= MAX_GRANTS_PER_PROFILE
                || state.claude_code_grants.len() >= MAX_GRANTS
                || state.claude_code_grants.iter().any(|status| {
                    status.grant.id == grant.id
                        || (status.grant.profile_id == grant.profile_id
                            && (status.grant.role == grant.role
                                || status.grant.directory.same_directory(&grant.directory)))
                })
            {
                return Err(ControlError::Invalid(
                    "invalid or duplicate Claude Code grant",
                ));
            }
            state.claude_code_grants.push(ClaudeCodeGrantStatus {
                grant: grant.clone(),
                state: ContainerGrantState::Pending,
            });
        }
        Action::ClaudeCodeGrantApplied { grant_id } => {
            transition(state, *grant_id, |current| {
                (current == ContainerGrantState::Pending).then_some(ContainerGrantState::Active)
            })?;
        }
        Action::ClaudeCodeRevokeRequested { grant_id } => {
            transition(state, *grant_id, |current| {
                (current != ContainerGrantState::Revoking).then_some(ContainerGrantState::Revoking)
            })?;
        }
        Action::ClaudeCodeGrantRevoked { grant_id } => {
            let index = state
                .claude_code_grants
                .iter()
                .position(|status| {
                    status.grant.id == *grant_id && status.state == ContainerGrantState::Revoking
                })
                .ok_or(ControlError::Invalid(
                    "Claude Code revocation without a request",
                ))?;
            state.claude_code_grants.remove(index);
        }
        _ => {}
    }
    Ok(())
}

/// Whether a profile still has a Claude Code grant, which blocks its removal.
pub(super) fn has_grants(state: &ControlSnapshot, profile_id: Uuid) -> bool {
    state
        .claude_code_grants
        .iter()
        .any(|status| status.grant.profile_id == profile_id)
}

fn transition(
    state: &mut ControlSnapshot,
    grant_id: Uuid,
    next: impl FnOnce(ContainerGrantState) -> Option<ContainerGrantState>,
) -> Result<(), ControlError> {
    let status = state
        .claude_code_grants
        .iter_mut()
        .find(|status| status.grant.id == grant_id)
        .ok_or(ControlError::Invalid("unknown Claude Code grant"))?;
    status.state = next(status.state).ok_or(ControlError::Invalid("invalid grant transition"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
