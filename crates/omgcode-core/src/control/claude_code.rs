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
    /// The connection whose grants were revoked.
    pub profile_id: Uuid,
    /// How many grants were revoked.
    pub revoked: usize,
    /// Whether the container profile was deleted after the last grant.
    pub profile_deleted: bool,
    /// Whether the revocation withdrew the connection's running turns, so the
    /// caller waits for them with [`wait_for_claude_code_turns`] once the
    /// settings are released. Grants, and so revocations, exist only on
    /// Windows.
    pub turns_withdrawn: bool,
}

/// A revocation of a connection's grants that stopped with an error.
#[derive(Debug)]
pub struct ClaudeCodeRevokeFailure {
    /// What stopped the revocation.
    pub error: ControlError,
    /// The connection, when a withdrawal of its grants was recorded before
    /// the error, by this call or an interrupted earlier one. Its running
    /// turns then end, and the caller waits for them with
    /// [`wait_for_claude_code_turns`] once the settings are released, as
    /// after a success. `None` for a refusal before any withdrawal.
    pub withdrawn: Option<Uuid>,
}

impl From<ControlError> for ClaudeCodeRevokeFailure {
    fn from(error: ControlError) -> Self {
        Self {
            error,
            withdrawn: None,
        }
    }
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

/// The file name of the `claude` program on this platform.
pub const CLAUDE_CODE_PROGRAM_NAME: &str = if cfg!(windows) {
    "claude.exe"
} else {
    "claude"
};

/// The most candidates [`claude_code_program_candidates`] returns, so a very
/// long `PATH` cannot make the search unbounded.
const MAX_PROGRAM_CANDIDATES: usize = 128;

/// Where the `claude` program may be installed, most likely first
/// (ADR-0111): [`CLAUDE_CODE_PROGRAM_NAME`] in each absolute folder of
/// `search_path`, the operator's `PATH`, in order, then in the native
/// installer's folders under `home`. Relative and empty `PATH` entries are
/// skipped, and each candidate appears once.
///
/// Nothing is opened or run here. A caller keeps the first candidate that
/// [`ControlStore::check_claude_code_profile`] accepts, so a program that is
/// not a regular file, or whose folder is too broad or overlaps storage, the
/// development root, or a workspace, is never chosen.
pub fn claude_code_program_candidates(
    search_path: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let listed = search_path
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|folder| folder.is_absolute());
    let installed = home
        .filter(|home| home.is_absolute())
        .into_iter()
        .flat_map(|home| {
            let mut folders = vec![home.join(".local").join("bin")];
            if !cfg!(windows) {
                folders.push(home.join(".claude").join("local"));
            }
            folders
        });
    let mut candidates: Vec<PathBuf> = Vec::new();
    for folder in listed.chain(installed) {
        let candidate = folder.join(CLAUDE_CODE_PROGRAM_NAME);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
        if candidates.len() == MAX_PROGRAM_CANDIDATES {
            break;
        }
    }
    candidates
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
        self.check_readable_folder(&program_folder, None)?;
        let config_folder = DirectoryRecord::capture(&declaration.config_folder).map_err(|_| {
            ControlError::ProviderRefused(
                "the Claude sign-in folder must be an existing folder; create it and sign in with `claude auth login` first",
            )
        })?;
        self.check_config_folder(&config_folder, None)?;
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
        self.check_readable_folder(&relay, Some(profile_id))?;
        if !setup.program_folder.is_current()? || !setup.config_folder.is_current()? {
            return Err(ControlError::DirectoryChanged);
        }
        self.check_readable_folder(&setup.program_folder, Some(profile_id))?;
        self.check_config_folder(&setup.config_folder, Some(profile_id))?;
        let planned = [
            (ClaudeCodeGrantRole::Program, setup.program_folder.clone()),
            (
                ClaudeCodeGrantRole::Configuration,
                setup.config_folder.clone(),
            ),
            (ClaudeCodeGrantRole::Runtime, runtime),
            (ClaudeCodeGrantRole::Relay, relay),
        ];
        // The whole plan is checked before the first intent is recorded, so
        // a refused plan changes no record and no access-control list.
        let mut todo: Vec<(ClaudeCodeGrantRole, DirectoryRecord)> = Vec::new();
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
            if todo
                .iter()
                .any(|(_, earlier)| earlier.same_directory(&directory))
                || self.snapshot.claude_code_grants.iter().any(|status| {
                    status.grant.profile_id == profile_id
                        && status.grant.directory.same_directory(&directory)
                })
            {
                return Err(ControlError::GrantRefused(
                    "two roles of this connection name the same folder",
                ));
            }
            todo.push((role, directory));
        }
        let name = claude_code_container(profile_id)?;
        let sid = platform::container_sid(&name).map_err(ControlError::Container)?;
        let mut granted = Vec::new();
        for (role, directory) in todo {
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
    /// container profile, also when no grant remains, so an interrupted
    /// deletion can be retried. A pending grant is revoked too: its ACL change may
    /// or may not have happened, and revoking an absent entry succeeds.
    ///
    /// # Errors
    ///
    /// A [`ClaudeCodeRevokeFailure`] whose error is
    /// [`ControlError::UnknownProvider`] for an unknown profile,
    /// [`ControlError::ProviderRefused`] for a profile that is not a Claude
    /// Code connection, the platform error that stopped a revocation (later
    /// grants stay recorded), or [`ControlError::ProfileRemoval`] when every
    /// grant was revoked but the profile could not be deleted. It names the
    /// connection when a withdrawal was already recorded.
    pub fn revoke_claude_code_grants(
        &mut self,
        name: &str,
    ) -> Result<ClaudeCodeRevocation, ClaudeCodeRevokeFailure> {
        self.ensure_healthy()?;
        let (profile_id, _) = self.claude_code_setup(name)?;
        let grants: Vec<ClaudeCodeGrantStatus> = self
            .snapshot
            .claude_code_grants
            .iter()
            .filter(|status| status.grant.profile_id == profile_id)
            .cloned()
            .collect();
        let mut recorded = self.claude_code_withdrawal_recorded(profile_id, &grants);
        self.revoke_each_claude_code_grant(&grants, &mut recorded)
            .and_then(|revoked| {
                // Deleting an absent container profile succeeds, so a retry
                // after an interrupted deletion finishes it even when no
                // grant remains.
                let profile_deleted = if platform_grants_supported() {
                    let container = claude_code_container(profile_id)?;
                    platform::delete_profile(&container).map_err(ControlError::ProfileRemoval)?;
                    true
                } else {
                    false
                };
                Ok((revoked, profile_deleted))
            })
            .map(|(revoked, profile_deleted)| ClaudeCodeRevocation {
                profile_id,
                revoked,
                profile_deleted,
                turns_withdrawn: platform_grants_supported(),
            })
            .map_err(|error| ClaudeCodeRevokeFailure {
                error,
                withdrawn: recorded.then_some(profile_id),
            })
    }

    /// Whether an earlier revocation already withdrew the connection
    /// `profile_id`, whose grants are `grants`: one of them is being revoked,
    /// or none is active, as after a revocation that revoked the last grant
    /// and then failed to delete the container profile.
    fn claude_code_withdrawal_recorded(
        &self,
        profile_id: Uuid,
        grants: &[ClaudeCodeGrantStatus],
    ) -> bool {
        grants
            .iter()
            .any(|status| status.state == ContainerGrantState::Revoking)
            || self.claude_code_withdrawn(profile_id).is_some()
    }

    /// Revoke each of `grants` in turn and count them. `recorded` turns
    /// `true` once a withdrawal may be recorded, so a later error still
    /// reports it.
    fn revoke_each_claude_code_grant(
        &mut self,
        grants: &[ClaudeCodeGrantStatus],
        recorded: &mut bool,
    ) -> Result<usize, ControlError> {
        let mut revoked = 0;
        for status in grants {
            let grant_id = status.grant.id;
            let container = ContainerName::new(&status.grant.container)
                .map_err(|_| ControlError::Invalid("recorded container name is invalid"))?;
            if status.state != ContainerGrantState::Revoking {
                let appended = self.append(
                    Action::ClaudeCodeRevokeRequested { grant_id },
                    self.revision(),
                );
                // Every append error but a failed write or sync comes before
                // any byte is written. A failed write or sync can still have
                // left the event in the journal, which a running turn's
                // watcher reads, so it counts as recorded.
                *recorded |= matches!(appended, Ok(()) | Err(ControlError::Io(_)));
                appended?;
            }
            let live = status.grant.directory.live_path()?;
            platform::revoke(&live, status.grant.directory.native_identity(), &container)
                .map_err(ControlError::Container)?;
            self.append(Action::ClaudeCodeGrantRevoked { grant_id }, self.revision())?;
            revoked += 1;
        }
        Ok(revoked)
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
    /// when a recorded folder changed, [`ControlError::ProviderRefused`] when
    /// the sign-in folder now overlaps application storage, the development
    /// root, a workspace, a workspace container grant, or another connection's
    /// sign-in or program folder, [`ControlError::GrantRefused`] when a grant
    /// is missing, pending, or for another folder, or when the program or
    /// relay folder is too broad, holds application storage, the development
    /// root, or a workspace, or overlaps another connection's sign-in folder,
    /// and the I/O error that stopped a check.
    pub fn resolve_claude_code(
        &self,
        name: &str,
        relay_program: &Path,
    ) -> Result<ClaudeCodePaths, ControlError> {
        self.ensure_healthy()?;
        let (profile_id, setup) = self.claude_code_setup(name)?;
        let program = setup.live_program()?;
        let config_folder = setup.live_config_folder()?;
        // The sign-in folder must still be outside everything a model-run
        // program can reach, which can have changed since it was recorded.
        self.check_config_folder(&setup.config_folder, Some(profile_id))?;
        let relay_program = std::fs::canonicalize(relay_program)?;
        let relay_folder = relay_program.parent().ok_or(ControlError::GrantRefused(
            "the relay program has no folder",
        ))?;
        // The run can read its whole program and relay folders, so neither
        // may hold another connection's sign-in folder, also one recorded
        // after this connection was or before this check existed. On Linux
        // no grant records them, so only this check does.
        let relay = DirectoryRecord::capture(relay_folder)?;
        self.check_readable_folder(&setup.program_folder, Some(profile_id))?;
        self.check_readable_folder(&relay, Some(profile_id))?;
        let data_root = self.data_directory.live_path()?;
        let runtime_folder = claude_code_runtime_folder(&data_root, profile_id);
        // Taken under the settings lock, so the watcher sees every later change.
        let watch = ClaudeCodeWatch::new(
            data_root,
            profile_id,
            turns::TurnFolders {
                config_folder: setup.config_folder.clone(),
                program_folder: setup.program_folder.clone(),
                relay_folder: relay.clone(),
            },
            self.event_count(),
        );
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
            watch,
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
    /// filesystem or drive root, the home folder or one of its ancestors, a
    /// folder that holds application storage, the development root, or a
    /// registered workspace, or a folder that overlaps the sign-in folder of
    /// a Claude Code connection other than `profile`. A folder inside a
    /// workspace, such as a build output folder that holds the relay, is
    /// allowed.
    fn check_readable_folder(
        &self,
        directory: &DirectoryRecord,
        profile: Option<Uuid>,
    ) -> Result<(), ControlError> {
        check_not_broad(directory)?;
        if self.overlaps_claude_code_sign_in(directory, profile)? {
            return Err(ControlError::GrantRefused(
                "a Claude Code folder must not overlap another Claude Code connection's sign-in folder",
            ));
        }
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
    /// development root, any workspace, any workspace container grant, or the
    /// sign-in folder, program folder, or a program or relay grant of a
    /// Claude Code connection other than `profile`, or a program or relay
    /// folder that a turn record of such a connection names, so no model file
    /// tool, model-run program, or other connection's process can read it. A grant
    /// record whose folder changed proves nothing about where its
    /// access-control entry now is, so it is refused until revoked.
    fn check_config_folder(
        &self,
        directory: &DirectoryRecord,
        profile: Option<Uuid>,
    ) -> Result<(), ControlError> {
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
        // On every platform, also on Linux where no grant is recorded.
        if self.overlaps_claude_code_program(directory, profile)? {
            return Err(ControlError::ProviderRefused(
                "the Claude sign-in folder must not overlap another Claude Code connection's program folder",
            ));
        }
        // Also the relay folder, which no setting records on Linux, and the
        // program folder of a removed connection, while a turn reads them.
        self.check_turn_scopes(directory, profile)?;
        for status in &self.snapshot.container_grants {
            let granted = status.grant().directory();
            if !granted.is_current()? {
                return Err(ControlError::ProviderRefused(
                    "a workspace container grant's folder changed since it was granted; revoke that grant first",
                ));
            }
            if directory.contains(granted)? || granted.contains(directory)? {
                return Err(ControlError::ProviderRefused(
                    "the Claude sign-in folder must not overlap a workspace container grant; revoke that grant first",
                ));
            }
        }
        for status in &self.snapshot.claude_code_grants {
            // The sign-in folder's own grant is checked against the other
            // connections' sign-in folders below; the runtime folder lies in
            // application storage.
            if Some(status.grant.profile_id) == profile
                || !matches!(
                    status.grant.role,
                    ClaudeCodeGrantRole::Program | ClaudeCodeGrantRole::Relay
                )
            {
                continue;
            }
            let granted = &status.grant.directory;
            if !granted.is_current()? {
                return Err(ControlError::ProviderRefused(
                    "another Claude Code connection's granted folder changed since it was granted; revoke that connection's grants first",
                ));
            }
            if directory.contains(granted)? || granted.contains(directory)? {
                return Err(ControlError::ProviderRefused(
                    "the Claude sign-in folder must not overlap another Claude Code connection's program or relay grant",
                ));
            }
        }
        if self.overlaps_claude_code_sign_in(directory, profile)? {
            return Err(ControlError::ProviderRefused(
                "the Claude sign-in folder must not overlap another Claude Code connection's sign-in folder",
            ));
        }
        Ok(())
    }

    /// Whether `directory` equals, contains, or lies inside the program
    /// folder of a recorded Claude Code connection other than `except`. That
    /// connection's run can read its whole program folder (through Landlock
    /// on Linux and its program grant on Windows), so no sign-in folder may
    /// lie there. A program folder that changed since it was recorded counts
    /// as apart: its connection cannot launch until the folder is restored,
    /// and resolving that connection checks its program folder against every
    /// other sign-in folder.
    ///
    /// # Errors
    ///
    /// The I/O error that stopped a comparison.
    fn overlaps_claude_code_program(
        &self,
        directory: &DirectoryRecord,
        except: Option<Uuid>,
    ) -> Result<bool, ControlError> {
        for status in self.provider_profiles() {
            if Some(status.profile().id()) == except {
                continue;
            }
            let Some(setup) = status.profile().claude_code_setup() else {
                continue;
            };
            if directory.contains(&setup.program_folder)?
                || setup.program_folder.contains(directory)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `directory` equals, contains, or lies inside the sign-in
    /// folder of a recorded Claude Code connection, leaving out the one with
    /// the identifier `except`.
    ///
    /// A removed connection's turn whose tree was not observed ended keeps
    /// its sign-in folder reserved until the operator acknowledges it, so its
    /// processes cannot share the folder with a new connection or reach a
    /// new scope through it.
    ///
    /// # Errors
    ///
    /// [`ControlError::GrantRefused`] when a sign-in folder changed since it
    /// was recorded: a moved folder could now lie anywhere, so no scope is
    /// proven separate from it until that connection is removed.
    /// [`ControlError::ClaudeCodeSignInReserved`] when a removed connection's
    /// turn record reserves the folder, also when that folder changed, and
    /// [`ControlError::ClaudeCodeTurnRecordUnreadable`] for a record that
    /// cannot be read.
    pub(super) fn overlaps_claude_code_sign_in(
        &self,
        directory: &DirectoryRecord,
        except: Option<Uuid>,
    ) -> Result<bool, ControlError> {
        let recorded = |profile_id: Uuid| {
            self.provider_profiles().iter().any(|status| {
                status.profile().id() == profile_id
                    && status.profile().claude_code_setup().is_some()
            })
        };
        for status in self.provider_profiles() {
            if Some(status.profile().id()) == except {
                continue;
            }
            let Some(setup) = status.profile().claude_code_setup() else {
                continue;
            };
            if !setup.config_folder.is_current()? {
                return Err(ControlError::GrantRefused(
                    "a Claude Code connection's sign-in folder changed since it was recorded; remove that connection with `workspace provider-profile-remove` first, and where it has grants, restore the folder to its recorded path and run `workspace claude-code-revoke` before that",
                ));
            }
            if directory.contains(&setup.config_folder)?
                || setup.config_folder.contains(directory)?
            {
                return Ok(true);
            }
        }
        // A recorded connection's folder was checked above.
        let data_root = self.data_directory.live_path()?;
        let reserved = turns::reserved_folders(&data_root, |profile_id| {
            Some(profile_id) == except || recorded(profile_id)
        })?;
        for (profile_id, folders) in reserved {
            if overlaps_or_moved(directory, &folders.config_folder)? {
                return Err(ControlError::ClaudeCodeSignInReserved(profile_id));
            }
        }
        Ok(false)
    }

    /// Refuse a sign-in folder that overlaps a program or relay folder that
    /// a turn record of a connection other than `except` names: that turn
    /// can read the folder whole until its tree was observed ended, also once
    /// its connection is removed, and on Linux no grant records either
    /// folder. A disjoint folder is accepted.
    ///
    /// # Errors
    ///
    /// [`ControlError::ClaudeCodeScopeReserved`] for an overlapping folder or
    /// one that moved since the turn started, and the errors of
    /// [`turns::reserved_folders`].
    fn check_turn_scopes(
        &self,
        directory: &DirectoryRecord,
        except: Option<Uuid>,
    ) -> Result<(), ControlError> {
        let data_root = self.data_directory.live_path()?;
        for (profile_id, folders) in
            turns::reserved_folders(&data_root, |profile_id| Some(profile_id) == except)?
        {
            for folder in folders.readable() {
                if overlaps_or_moved(directory, folder)? {
                    return Err(ControlError::ClaudeCodeScopeReserved(profile_id));
                }
            }
        }
        Ok(())
    }
}

/// Whether `directory` equals, contains, or lies inside a folder a turn
/// record reserves, or that folder moved since: a moved folder could now
/// lie anywhere.
///
/// # Errors
///
/// The I/O error that stopped a comparison.
fn overlaps_or_moved(
    directory: &DirectoryRecord,
    reserved: &DirectoryRecord,
) -> Result<bool, ControlError> {
    Ok(!reserved.is_current().unwrap_or(false)
        || directory.contains(reserved)?
        || reserved.contains(directory)?)
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
    /// The settings the turn was resolved from, which its process's watcher
    /// rechecks.
    pub watch: ClaudeCodeWatch,
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

mod turns;

pub use turns::{
    CLAUDE_CODE_TURN_STOP_WAIT, ClaudeCodeWatch, MAX_TURN_RECORDS, acknowledge_claude_code_cleanup,
    wait_for_claude_code_turns,
};
pub(crate) use turns::{Recheck, TurnRecord, WatchCursor};

#[cfg(test)]
mod tests;
