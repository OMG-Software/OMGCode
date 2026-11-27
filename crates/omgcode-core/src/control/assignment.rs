//! Toolchain profiles assigned to a workspace, and the container access the
//! operator's assignment grants (ADR-0107).
//!
//! A workspace runs only the profiles assigned to it. Assigning a profile is
//! the operator's consent to give that workspace's Windows container read and
//! execute access to each of the profile's roots it cannot read yet, and
//! registering a workspace is the consent to give it read and write access to
//! its own folder. Unassigning a profile, removing it, or unregistering the
//! workspace revokes the grants that no remaining assignment needs.
//!
//! [`ControlStore::assign_profile`] and [`ControlStore::unassign_profile`]
//! record the assignment only and change no access-control list. The
//! `*_with_access` methods are for the trusted operator surfaces, the CLI and
//! the `settings` window. They grant and revoke through
//! [`ControlStore::grant_container_access`] and
//! [`ControlStore::revoke_container_grant`], so every grant is checked,
//! recorded, and revocable like any other. Off Windows they record the same
//! events and change no access.

use std::path::{Path, PathBuf};

use omgcode_platform::container::{self as platform, CONTAINER_GRANTS_SUPPORTED};
use uuid::Uuid;

use super::{
    Action, ContainerGrant, ContainerGrantAccess, ContainerGrantState, ContainerGrantStatus,
    ContainerRevocation, ControlError, ControlSnapshot, ControlStore, ProfilePlatform,
    RootGrantNeed, WorkspaceRegistration, root_grant_need,
};
use crate::policy::WorkspaceId;
use crate::workspace::DirectoryRecord;

/// One folder an assignment would grant, or leave out. Two planned folders
/// are equal when they are the same directory, however their paths are
/// spelled, so a plan made from discovered paths matches the plan made again
/// from the recorded roots.
#[derive(Clone, Debug)]
pub struct PlannedFolder {
    path: PathBuf,
    display: String,
    /// The directory's identity, when it could be read.
    record: Option<DirectoryRecord>,
}

impl PartialEq for PlannedFolder {
    fn eq(&self, other: &Self) -> bool {
        match (&self.record, &other.record) {
            (Some(own), Some(theirs)) => own.same_directory(theirs),
            (None, None) => self.path == other.path,
            _ => false,
        }
    }
}

impl Eq for PlannedFolder {}

impl PlannedFolder {
    /// The folder. Show it only on trusted operator surfaces; never add it to
    /// text a model receives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder as display-safe text. Never reopen a path from it.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }
}

/// The folders assigning a profile to a workspace would grant that
/// workspace's container, planned before the operator confirms. Planning
/// changes and records nothing, and the answer can change before the
/// assignment, which plans again and refuses a different list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileAccessPlan {
    grant: Vec<PlannedFolder>,
    administrator: Vec<PlannedFolder>,
    unknown: Vec<PlannedFolder>,
}

impl ProfileAccessPlan {
    /// The folders to grant read and execute, in profile order: each root the
    /// container cannot read that this account can grant, leaving out a root
    /// inside another planned one and a root that already has a grant of the
    /// workspace. Empty off Windows.
    #[must_use]
    pub fn to_grant(&self) -> &[PlannedFolder] {
        &self.grant
    }

    /// Roots the container cannot read that only an administrator can grant.
    /// They are left out, and a run that needs one is refused (ADR-0106).
    #[must_use]
    pub fn needs_administrator(&self) -> &[PlannedFolder] {
        &self.administrator
    }

    /// Roots whose access could not be read, or that are no longer at their
    /// recorded location. They are left out.
    #[must_use]
    pub fn unknown(&self) -> &[PlannedFolder] {
        &self.unknown
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.grant
            .iter()
            .map(|folder| folder.path.clone())
            .collect()
    }
}

/// What assigning profiles with their access did.
#[derive(Debug)]
pub struct ProfileAssignment {
    /// The assigned profiles, in the order named.
    pub profile_ids: Vec<Uuid>,
    /// The grants applied before the assignments were recorded, in plan
    /// order.
    pub granted: Vec<ContainerGrant>,
    /// The plan the assignment applied, with the folders it left out.
    pub plan: ProfileAccessPlan,
}

/// A registration made by [`ControlStore::register_workspace_with_access`].
#[derive(Debug)]
pub struct RegisteredWorkspace {
    /// The registration, new or the existing one for the same folder.
    pub registration: WorkspaceRegistration,
    /// What became of the read-write grant on its folder. A failed grant
    /// leaves the workspace registered; a grant whose access-control change
    /// failed stays `pending` until it is revoked.
    pub grant: Result<FolderGrant, ControlError>,
}

/// The read-write grant on a workspace's folder that registering it applied,
/// or why it applied none.
#[derive(Debug)]
pub enum FolderGrant {
    /// Granted now.
    Granted(ContainerGrant),
    /// None was needed: container grants exist only on Windows, or an active
    /// read-write grant of the workspace is recorded on the folder already.
    NotNeeded,
    /// The folder needs a grant that was not attempted. A run in the
    /// workspace is refused until it has one.
    Skipped(SkippedGrant),
}

/// Why a folder that needs a grant was left without one, as
/// [`root_grant_need`](super::root_grant_need) classifies it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkippedGrant {
    /// This account cannot change the folder's access-control list; only an
    /// administrator can grant it.
    NeedsAdministrator,
    /// Whether this account can change the folder's access-control list
    /// could not be read.
    AccessUnknown,
}

impl SkippedGrant {
    /// The reason `skipped` from whether this account can change a folder's
    /// access-control list, or `None` when it can.
    #[must_use]
    pub fn from_check(can_change: &std::io::Result<bool>) -> Option<Self> {
        match can_change {
            Ok(true) => None,
            Ok(false) => Some(Self::NeedsAdministrator),
            Err(_) => Some(Self::AccessUnknown),
        }
    }

    /// The stable reason code, as `profile-assign` lists its folders.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsAdministrator => "needs_administrator",
            Self::AccessUnknown => "access_unknown",
        }
    }
}

/// The revocations an operator change made after it was recorded.
#[derive(Debug, Default)]
pub struct Revocations {
    /// The confirmed revocations, in order.
    pub revoked: Vec<ContainerRevocation>,
    /// Each revocation that failed, with its grant, in order. Every grant is
    /// attempted, so one failure never leaves a later grant unrevoked. A
    /// failed grant stays recorded, `revoking` when its access-control change
    /// failed, and `workspace container-revoke` retries it. After
    /// [`ControlError::ProfileRemoval`] the grant is revoked, and only the
    /// container's AppContainer profile, which then holds no grant, remains.
    pub failures: Vec<(Uuid, ControlError)>,
    /// The workspace of each grant whose revocation was attempted, confirmed
    /// or not, each once, in order. A revocation withdraws every run of its
    /// workspace, which the settings alone do not show, so a caller waits
    /// for the runs of these workspaces that it listed before the change.
    pub workspaces: Vec<WorkspaceId>,
}

/// Why [`ControlStore::assign_profiles_with_access`] failed, and what it
/// undid. The profiles it did not record stay unassigned.
#[derive(Debug)]
pub struct AssignmentFailure {
    /// The refusal or failure that stopped the assignment.
    pub error: ControlError,
    /// The revocations of the grants the call made that no assigned profile
    /// needs. A grant in [`Revocations::failures`] other than a
    /// [`ControlError::ProfileRemoval`] stays recorded, `pending` or
    /// `revoking`, with its access, until the operator revokes it. Each
    /// revocation withdrew the runs of its workspace.
    pub released: Revocations,
}

impl From<ControlError> for AssignmentFailure {
    /// A failure before any grant was made, so nothing was undone.
    fn from(error: ControlError) -> Self {
        Self {
            error,
            released: Revocations::default(),
        }
    }
}

/// What [`ControlStore::unregister_workspace_with_access`] did.
#[derive(Debug)]
pub struct Unregistration {
    /// The revocations made first. The workspace stays registered when any
    /// of them failed.
    pub revocations: Revocations,
    /// Why the workspace stays registered although every revocation was
    /// confirmed: its removal could not be recorded. `None` otherwise.
    pub removal: Option<ControlError>,
}

impl Unregistration {
    /// Whether the workspace was unregistered.
    #[must_use]
    pub fn unregistered(&self) -> bool {
        self.revocations.failures.is_empty() && self.removal.is_none()
    }
}

impl Revocations {
    /// The runs in `pending`, listed by `run::pending_runs` before the change,
    /// that belong to a workspace in [`Self::workspaces`].
    #[must_use]
    pub fn withdrawn_runs(&self, pending: &[(WorkspaceId, Uuid)]) -> Vec<Uuid> {
        pending
            .iter()
            .filter(|(workspace, _)| self.workspaces.contains(workspace))
            .map(|(_, run)| *run)
            .collect()
    }
}

impl ControlStore {
    /// The profiles assigned to `workspace_id`, by identifier, in the order
    /// they were assigned. Empty for an unknown workspace. Configuration only.
    pub fn assigned_profiles(&self, workspace_id: WorkspaceId) -> &[Uuid] {
        self.workspace(workspace_id)
            .map_or(&[], |workspace| workspace.profiles.as_slice())
    }

    /// Whether `profile_id` is assigned to `workspace_id`.
    pub fn profile_assigned(&self, workspace_id: WorkspaceId, profile_id: Uuid) -> bool {
        self.assigned_profiles(workspace_id).contains(&profile_id)
    }

    /// Record that `workspace_id` may run the profile `name`, without
    /// changing any access. This advances the settings revision. The operator
    /// surfaces use [`Self::assign_profile_with_access`].
    ///
    /// # Errors
    ///
    /// Refuses a stale revision, an unknown workspace or profile, a profile
    /// declared on another platform, and a profile already assigned.
    pub fn assign_profile(
        &mut self,
        workspace_id: WorkspaceId,
        name: &str,
        expected_revision: u64,
    ) -> Result<Uuid, ControlError> {
        self.check_revision(expected_revision)?;
        let profile_id = self.assignable(workspace_id, name)?;
        self.append(
            Action::ProfileAssigned {
                workspace_id,
                profile_id,
            },
            self.next_revision()?,
        )?;
        Ok(profile_id)
    }

    /// Record that `workspace_id` no longer runs the profile `name`, without
    /// changing any access. This advances the settings revision and withdraws
    /// the workspace's runs of that profile. The operator surfaces use
    /// [`Self::unassign_profile_with_access`].
    ///
    /// # Errors
    ///
    /// Refuses a stale revision, an unknown workspace or profile, and a
    /// profile that is not assigned to the workspace.
    pub fn unassign_profile(
        &mut self,
        workspace_id: WorkspaceId,
        name: &str,
        expected_revision: u64,
    ) -> Result<Uuid, ControlError> {
        self.check_revision(expected_revision)?;
        let workspace = self
            .workspace(workspace_id)
            .ok_or(ControlError::UnknownWorkspace)?;
        let profile_id = self
            .profile(name)
            .ok_or(ControlError::UnknownProfile)?
            .profile()
            .id();
        if !workspace.profiles.contains(&profile_id) {
            return Err(ControlError::ProfileRefused(
                "the profile is not assigned to this workspace",
            ));
        }
        self.append(
            Action::ProfileUnassigned {
                workspace_id,
                profile_id,
            },
            self.next_revision()?,
        )?;
        Ok(profile_id)
    }

    /// Plan the grants assigning the profile `name` to `workspace_id` would
    /// make. This reads access-control lists and changes nothing.
    ///
    /// # Errors
    ///
    /// What [`Self::assign_profile`] refuses, apart from the revision.
    pub fn plan_profile_access(
        &self,
        workspace_id: WorkspaceId,
        name: &str,
    ) -> Result<ProfileAccessPlan, ControlError> {
        self.plan_profiles_access(workspace_id, &[name])
    }

    /// Plan the grants assigning every profile in `names` to `workspace_id`
    /// would make, as one list over all their roots in order.
    ///
    /// # Errors
    ///
    /// What [`Self::assign_profile`] refuses for any of them, apart from the
    /// revision; a profile named twice; and a container grant of the
    /// workspace that is pending or being revoked.
    pub fn plan_profiles_access(
        &self,
        workspace_id: WorkspaceId,
        names: &[&str],
    ) -> Result<ProfileAccessPlan, ControlError> {
        let mut ids = Vec::with_capacity(names.len());
        for name in names {
            let id = self.assignable(workspace_id, name)?;
            if ids.contains(&id) {
                return Err(ControlError::ProfileRefused("a profile is named twice"));
            }
            ids.push(id);
        }
        self.check_grants_settled(workspace_id)?;
        let mut plan = ProfileAccessPlan::default();
        if !CONTAINER_GRANTS_SUPPORTED {
            return Ok(plan);
        }
        let mut live = Vec::new();
        for name in names {
            let status = self.profile(name).ok_or(ControlError::UnknownProfile)?;
            for root in status.profile().read_only_roots() {
                match root.is_current() {
                    Ok(true) => live.push(root.live_path()?),
                    Ok(false) | Err(_) => {
                        let folder = PlannedFolder {
                            path: root.live_path().unwrap_or_default(),
                            display: root.display(),
                            record: None,
                        };
                        if !plan.unknown.contains(&folder) {
                            plan.unknown.push(folder);
                        }
                    }
                }
            }
        }
        self.plan_into(workspace_id, &live, &mut plan);
        Ok(plan)
    }

    /// Plan read-execute grants of `paths` for `workspace_id` as
    /// [`Self::plan_profile_access`] does for a profile's roots, for folders
    /// that are not recorded yet, such as discovered toolchains (ADR-0103).
    ///
    /// # Errors
    ///
    /// An unknown workspace, and a container grant of it that is pending or
    /// being revoked.
    pub fn plan_folder_access(
        &self,
        workspace_id: WorkspaceId,
        paths: &[PathBuf],
    ) -> Result<ProfileAccessPlan, ControlError> {
        if self.workspace(workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        self.check_grants_settled(workspace_id)?;
        let mut plan = ProfileAccessPlan::default();
        if CONTAINER_GRANTS_SUPPORTED {
            self.plan_into(workspace_id, paths, &mut plan);
        }
        Ok(plan)
    }

    /// Grant the folders the operator confirmed in `confirmed`, then record
    /// the assignment, under this store's lock. The plan is made again first:
    /// a different list of folders to grant refuses the assignment, so the
    /// operator never grants a folder they did not see. Each grant goes
    /// through [`Self::grant_container_access`], is refused when its folder
    /// is no longer the confirmed directory, and advances the revision.
    ///
    /// # Errors
    ///
    /// What [`Self::assign_profile`] refuses; a changed plan; a folder core's
    /// grant rules refuse, before anything is recorded; the first grant that
    /// fails; and an assignment that cannot be recorded, each in
    /// [`AssignmentFailure::error`]. After a failure the profile is not
    /// assigned, and the grants this call made that no assigned profile needs
    /// are revoked, which withdraws the workspace's runs; those revocations
    /// are returned in [`AssignmentFailure::released`]. A grant whose
    /// revocation fails then stays recorded, `pending` or `revoking`, until
    /// it is revoked.
    pub fn assign_profile_with_access(
        &mut self,
        workspace_id: WorkspaceId,
        name: &str,
        confirmed: &ProfileAccessPlan,
        expected_revision: u64,
    ) -> Result<ProfileAssignment, AssignmentFailure> {
        self.assign_profiles_with_access(workspace_id, &[name], confirmed, expected_revision)
    }

    /// As [`Self::assign_profile_with_access`] for every profile in `names`
    /// under one confirmed plan, made by [`Self::plan_profiles_access`] or,
    /// for folders not yet recorded as profile roots, by
    /// [`Self::plan_folder_access`]. The grants are applied first, then each
    /// assignment is recorded in order.
    ///
    /// # Errors
    ///
    /// As [`Self::assign_profile_with_access`]. An assignment that fails
    /// after others were recorded leaves those recorded, with the grants
    /// they need.
    pub fn assign_profiles_with_access(
        &mut self,
        workspace_id: WorkspaceId,
        names: &[&str],
        confirmed: &ProfileAccessPlan,
        expected_revision: u64,
    ) -> Result<ProfileAssignment, AssignmentFailure> {
        self.check_revision(expected_revision)?;
        let plan = self.plan_profiles_access(workspace_id, names)?;
        if plan.grant != confirmed.grant {
            return Err(ControlError::ProfileRefused(
                "the folders this assignment grants changed; review it again",
            )
            .into());
        }
        self.check_container_grants(
            workspace_id,
            &plan.paths(),
            ContainerGrantAccess::ReadExecute,
        )
        .map_err(|(_, error)| error)?;
        let mut granted = Vec::with_capacity(plan.grant.len());
        for folder in &plan.grant {
            match self.grant_confirmed_container_access(
                workspace_id,
                &folder.path,
                folder.record.as_ref(),
                ContainerGrantAccess::ReadExecute,
                self.revision(),
            ) {
                Ok(grant) => granted.push(grant),
                Err(error) => {
                    // A grant whose access-control change failed is recorded
                    // `pending`, and it is this call's: the plan leaves out a
                    // folder that already has a grant of the workspace.
                    let mut made: Vec<Uuid> = granted.iter().map(ContainerGrant::id).collect();
                    made.extend(
                        folder
                            .record
                            .as_ref()
                            .and_then(|record| self.workspace_grant_on(workspace_id, record)),
                    );
                    let released = self.release_unneeded(workspace_id, &made);
                    return Err(AssignmentFailure { error, released });
                }
            }
        }
        let mut profile_ids = Vec::with_capacity(names.len());
        for name in names {
            match self.assign_profile(workspace_id, name, self.revision()) {
                Ok(id) => profile_ids.push(id),
                Err(error) => {
                    // The grants were the operator's consent to this
                    // assignment, so a grant no recorded assignment needs
                    // does not outlive it.
                    let made: Vec<Uuid> = granted.iter().map(ContainerGrant::id).collect();
                    let released = self.release_unneeded(workspace_id, &made);
                    return Err(AssignmentFailure { error, released });
                }
            }
        }
        Ok(ProfileAssignment {
            profile_ids,
            granted,
            plan,
        })
    }

    /// Record the unassignment, then revoke the workspace's read-execute
    /// grants on the profile's roots, or on folders that hold them, that
    /// neither a profile still assigned to it nor its package cache root
    /// needs. The workspace folder's read-write grant is never revoked here.
    /// Revocations need no revision; each withdraws every run of the
    /// workspace.
    ///
    /// # Errors
    ///
    /// What [`Self::unassign_profile`] refuses. A failed revocation does not
    /// fail the call; it is reported in [`Revocations::failure`].
    pub fn unassign_profile_with_access(
        &mut self,
        workspace_id: WorkspaceId,
        name: &str,
        expected_revision: u64,
    ) -> Result<(Uuid, Revocations), ControlError> {
        self.check_revision(expected_revision)?;
        let grants = self.grants_released_by_unassignment(workspace_id, name)?;
        let profile_id = self.unassign_profile(workspace_id, name, expected_revision)?;
        Ok((profile_id, self.revoke_each(&grants)))
    }

    /// The grants of `workspace_id` that
    /// [`Self::unassign_profile_with_access`] would revoke now, for a
    /// confirmation shown before the change. Reads settings only.
    ///
    /// # Errors
    ///
    /// An unknown workspace or profile, and a profile not assigned to the
    /// workspace.
    pub fn grants_released_by_unassignment(
        &self,
        workspace_id: WorkspaceId,
        name: &str,
    ) -> Result<Vec<Uuid>, ControlError> {
        if self.workspace(workspace_id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        let profile = self
            .profile(name)
            .ok_or(ControlError::UnknownProfile)?
            .profile();
        if !self.profile_assigned(workspace_id, profile.id()) {
            return Err(ControlError::ProfileRefused(
                "the profile is not assigned to this workspace",
            ));
        }
        Ok(self.unneeded_root_grants(workspace_id, profile.id(), profile.read_only_roots()))
    }

    /// For each workspace the profile `name` is assigned to, in registration
    /// order, the grants [`Self::remove_profile_with_access`] would revoke
    /// now. Reads settings only.
    ///
    /// # Errors
    ///
    /// An unknown profile.
    pub fn grants_released_by_removal(
        &self,
        name: &str,
    ) -> Result<Vec<(WorkspaceId, Vec<Uuid>)>, ControlError> {
        let profile = self
            .profile(name)
            .ok_or(ControlError::UnknownProfile)?
            .profile();
        Ok(self
            .snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.profiles.contains(&profile.id()))
            .map(|workspace| {
                (
                    workspace.id,
                    self.unneeded_root_grants(
                        workspace.id,
                        profile.id(),
                        profile.read_only_roots(),
                    ),
                )
            })
            .collect())
    }

    /// Remove the profile `name`, which ends every assignment of it, then
    /// revoke, in each workspace it was assigned to, the grants
    /// [`Self::unassign_profile_with_access`] would.
    ///
    /// # Errors
    ///
    /// What [`Self::remove_profile`] refuses. A failed revocation is reported
    /// in [`Revocations::failure`].
    pub fn remove_profile_with_access(
        &mut self,
        name: &str,
        expected_revision: u64,
    ) -> Result<(Uuid, Revocations), ControlError> {
        self.check_revision(expected_revision)?;
        let grants: Vec<Uuid> = self
            .grants_released_by_removal(name)?
            .into_iter()
            .flat_map(|(_, grants)| grants)
            .collect();
        let profile_id = self.remove_profile(name, expected_revision)?;
        Ok((profile_id, self.revoke_each(&grants)))
    }

    /// Register `path` as [`Self::register_workspace`] does, then, on Windows,
    /// grant the workspace's container read-write on its folder when no grant
    /// of the workspace is recorded on that folder and this account can change
    /// its access-control list. Registering a folder again that has no such
    /// grant grants it. A grant this account cannot apply, or whose need
    /// could not be read, is not attempted and is reported as
    /// [`FolderGrant::Skipped`]. Another grant of the workspace on the folder,
    /// such as a read-execute grant or one left `pending` or `revoking`,
    /// refuses the grant, because core records one grant per folder; see
    /// [`Self::unusable_folder_grant`].
    ///
    /// # Errors
    ///
    /// What [`Self::register_workspace`] refuses. A failed or refused grant
    /// does not fail the call; it is reported in
    /// [`RegisteredWorkspace::grant`].
    pub fn register_workspace_with_access(
        &mut self,
        path: &Path,
        expected_revision: u64,
    ) -> Result<RegisteredWorkspace, ControlError> {
        let registration = self.register_workspace(path, expected_revision)?;
        let grant = self.grant_own_folder(&registration);
        Ok(RegisteredWorkspace {
            registration,
            grant,
        })
    }

    /// The grant of workspace `id` recorded on its own folder that is not an
    /// active read-write grant: a read-execute grant, or one left `pending`
    /// or `revoking`. While it is recorded the folder cannot get the
    /// read-write grant the workspace's runs need, so it must be revoked
    /// first. `None` for an unknown workspace or when there is no such grant.
    #[must_use]
    pub fn unusable_folder_grant(&self, id: WorkspaceId) -> Option<&ContainerGrantStatus> {
        let registration = self.workspace(id)?;
        self.folder_grant(registration)
            .filter(|status| !Self::usable_folder_grant(status))
    }

    /// The grant of a workspace recorded on its own folder, in any state.
    fn folder_grant(&self, registration: &WorkspaceRegistration) -> Option<&ContainerGrantStatus> {
        self.container_grants().iter().find(|status| {
            status.grant().workspace_id() == registration.id
                && status
                    .grant()
                    .directory()
                    .same_directory(&registration.directory)
        })
    }

    /// Whether a grant on a workspace's folder gives its runs what they need.
    fn usable_folder_grant(status: &ContainerGrantStatus) -> bool {
        status.state() == ContainerGrantState::Active
            && status.grant().access() == ContainerGrantAccess::ReadWrite
    }

    /// The read-write grant [`Self::register_workspace_with_access`] applies.
    fn grant_own_folder(
        &mut self,
        registration: &WorkspaceRegistration,
    ) -> Result<FolderGrant, ControlError> {
        if !CONTAINER_GRANTS_SUPPORTED {
            return Ok(FolderGrant::NotNeeded);
        }
        if let Some(status) = self.folder_grant(registration) {
            return if Self::usable_folder_grant(status) {
                Ok(FolderGrant::NotNeeded)
            } else {
                Err(ControlError::GrantRefused(
                    "the workspace folder has a grant of the workspace that is not an active read-write grant; revoke it, then register the folder again",
                ))
            };
        }
        let live = registration.directory.live_path()?;
        if let Some(skipped) = SkippedGrant::from_check(&platform::can_change_access(&live)) {
            return Ok(FolderGrant::Skipped(skipped));
        }
        self.grant_container_access(
            registration.id,
            &live,
            ContainerGrantAccess::ReadWrite,
            self.revision(),
        )
        .map(FolderGrant::Granted)
    }

    /// Revoke every grant of `id`, in any state, then, when every revocation
    /// was confirmed, unregister it as [`Self::unregister_workspace`] does.
    /// Each revocation withdraws the workspace's runs.
    ///
    /// # Errors
    ///
    /// A stale revision or an unknown workspace, before anything changes.
    /// What fails after that does not fail the call, because the revocations
    /// before it stand and their runs must be waited for: a failed revocation
    /// is reported in [`Revocations::failures`], and a removal that cannot be
    /// recorded after every grant was revoked in
    /// [`Unregistration::removal`]. The workspace then stays registered, with
    /// the grants not yet revoked.
    pub fn unregister_workspace_with_access(
        &mut self,
        id: WorkspaceId,
        expected_revision: u64,
    ) -> Result<Unregistration, ControlError> {
        self.check_revision(expected_revision)?;
        if self.workspace(id).is_none() {
            return Err(ControlError::UnknownWorkspace);
        }
        let grants: Vec<Uuid> = self
            .container_grants()
            .iter()
            .filter(|status| status.grant().workspace_id() == id)
            .map(|status| status.grant().id())
            .collect();
        let revocations = self.revoke_each(&grants);
        let removal = if revocations.failures.is_empty() {
            self.unregister_workspace(id, self.revision()).err()
        } else {
            None
        };
        Ok(Unregistration {
            revocations,
            removal,
        })
    }

    /// The profile `name`'s identifier when it can be assigned to
    /// `workspace_id` now.
    fn assignable(&self, workspace_id: WorkspaceId, name: &str) -> Result<Uuid, ControlError> {
        let workspace = self
            .workspace(workspace_id)
            .ok_or(ControlError::UnknownWorkspace)?;
        let profile = self
            .profile(name)
            .ok_or(ControlError::UnknownProfile)?
            .profile();
        if Some(profile.platform()) != ProfilePlatform::current() {
            return Err(ControlError::ProfileRefused(
                "the profile was declared on another platform",
            ));
        }
        if workspace.profiles.contains(&profile.id()) {
            return Err(ControlError::ProfileRefused(
                "the profile is already assigned to this workspace",
            ));
        }
        Ok(profile.id())
    }

    /// Sort `paths` into `plan`, in order. A folder with a grant of the
    /// workspace recorded, in any state, is left out, as is one inside or
    /// equal to another planned folder, which inherits that grant.
    fn plan_into(
        &self,
        workspace_id: WorkspaceId,
        paths: &[PathBuf],
        plan: &mut ProfileAccessPlan,
    ) {
        let recorded: Vec<&DirectoryRecord> = self
            .container_grants()
            .iter()
            .filter(|status| status.grant().workspace_id() == workspace_id)
            .map(|status| status.grant().directory())
            .collect();
        let mut planned: Vec<DirectoryRecord> = Vec::new();
        for path in paths {
            let Ok(record) = DirectoryRecord::capture(path) else {
                let folder = PlannedFolder {
                    path: path.clone(),
                    display: crate::turn::display_safe_line(&path.to_string_lossy()),
                    record: None,
                };
                if !plan.unknown.contains(&folder) {
                    plan.unknown.push(folder);
                }
                continue;
            };
            let folder = PlannedFolder {
                path: path.clone(),
                display: record.display(),
                record: Some(record.clone()),
            };
            if recorded.iter().any(|grant| grant.same_directory(&record))
                || planned
                    .iter()
                    .any(|earlier| earlier.same_directory(&record))
            {
                continue;
            }
            let list = match root_grant_need(workspace_id, path) {
                RootGrantNeed::NeedsGrant => {
                    planned.push(record);
                    &mut plan.grant
                }
                RootGrantNeed::NeedsAdministrator => &mut plan.administrator,
                RootGrantNeed::Unknown => &mut plan.unknown,
                RootGrantNeed::Readable | RootGrantNeed::NotApplicable => continue,
            };
            if !list.contains(&folder) {
                list.push(folder);
            }
        }
        // A grant reaches everything beneath its folder, so a folder inside
        // another planned one, in either order, needs none of its own.
        let inside: Vec<bool> = planned
            .iter()
            .map(|folder| {
                planned.iter().any(|outer| {
                    !outer.same_directory(folder) && outer.contains(folder).unwrap_or(false)
                })
            })
            .collect();
        let mut kept = inside.iter().map(|inside| !inside);
        plan.grant.retain(|_| kept.next().unwrap_or(true));
    }

    /// Refuse while a container grant of `workspace_id` is pending or being
    /// revoked. A run of the workspace is refused while one is, and a plan
    /// leaves out a folder whose grant is recorded in any state, so an
    /// assignment would look complete while its runs stay refused.
    fn check_grants_settled(&self, workspace_id: WorkspaceId) -> Result<(), ControlError> {
        if self.container_grants().iter().any(|status| {
            status.grant().workspace_id() == workspace_id
                && status.state() != ContainerGrantState::Active
        }) {
            return Err(ControlError::GrantRefused(
                "a container grant of this workspace is pending or being revoked; revoke it under Settings, Advanced, Container grants, or with `omgcode workspace container-revoke`, then assign the profile again",
            ));
        }
        Ok(())
    }

    /// The recorded grant of `workspace_id` on `directory`, in any state.
    fn workspace_grant_on(
        &self,
        workspace_id: WorkspaceId,
        directory: &DirectoryRecord,
    ) -> Option<Uuid> {
        self.container_grants()
            .iter()
            .map(ContainerGrantStatus::grant)
            .find(|grant| {
                grant.workspace_id() == workspace_id && grant.directory().same_directory(directory)
            })
            .map(ContainerGrant::id)
    }

    /// The workspace's read-execute grants on `roots`, the roots of the
    /// profile `leaving`, or on a folder that holds one of them, that no other
    /// profile assigned to it and not its package cache root needs. A grant on
    /// an outer folder kept for a nested root while its own profile was
    /// unassigned is released with the last profile inside it, even when that
    /// root was moved or removed since: [`holds`] also reads the recorded
    /// paths.
    fn unneeded_root_grants(
        &self,
        workspace_id: WorkspaceId,
        leaving: Uuid,
        roots: &[DirectoryRecord],
    ) -> Vec<Uuid> {
        let candidates: Vec<&ContainerGrant> = self
            .container_grants()
            .iter()
            .map(ContainerGrantStatus::grant)
            .filter(|grant| {
                let directory = grant.directory();
                grant.workspace_id() == workspace_id
                    && grant.access() == ContainerGrantAccess::ReadExecute
                    && roots
                        .iter()
                        .any(|root| root.same_directory(directory) || holds(directory, root))
            })
            .collect();
        let releasing: Vec<Uuid> = candidates.iter().map(|grant| grant.id()).collect();
        let cache = self.package_cache_root(workspace_id);
        candidates
            .into_iter()
            .filter(|grant| {
                !self.grant_needed(workspace_id, grant, Some(leaving), &releasing, cache)
            })
            .map(ContainerGrant::id)
            .collect()
    }

    /// Whether a root of a profile assigned to `workspace_id`, other than
    /// `leaving`, or the folder `cache`, relies on `grant`: the root is the
    /// grant's folder, or lies beneath it, as [`holds`] reads it, and no
    /// other active grant of the workspace outside `releasing` is on the root
    /// or, by a live check, above it. A root inside another profile's root was
    /// readable when it was planned, so it was given no grant of its own. A
    /// pending or revoking grant may hold no access, so it never stands in
    /// for this one, and neither does a grant whose containment of the root
    /// cannot be read: in both cases the grant is kept.
    fn grant_needed(
        &self,
        workspace_id: WorkspaceId,
        grant: &ContainerGrant,
        leaving: Option<Uuid>,
        releasing: &[Uuid],
        cache: Option<&DirectoryRecord>,
    ) -> bool {
        let directory = grant.directory();
        let kept: Vec<&DirectoryRecord> = self
            .container_grants()
            .iter()
            .filter(|other| {
                let other = other.grant();
                other.workspace_id() == workspace_id
                    && other.id() != grant.id()
                    && !releasing.contains(&other.id())
            })
            .filter(|other| other.state() == ContainerGrantState::Active)
            .map(|other| other.grant().directory())
            .collect();
        self.assigned_profiles(workspace_id)
            .iter()
            .filter(|id| Some(**id) != leaving)
            .filter_map(|id| {
                self.profiles()
                    .iter()
                    .find(|status| status.profile().id() == *id)
            })
            .flat_map(|status| status.profile().read_only_roots())
            .chain(cache)
            .any(|root| {
                root.same_directory(directory)
                    || (holds(directory, root)
                        && !kept.iter().any(|other| {
                            other.same_directory(root) || other.contains(root).unwrap_or(false)
                        }))
            })
    }

    /// Revoke the grants in `made`, made by an assignment that then failed,
    /// that no profile assigned to the workspace needs, and return those
    /// revocations. The package cache root does not keep one: it did not rely
    /// on a grant that did not exist before. A revocation that fails leaves
    /// its grant recorded, as any failed revocation does, and is returned for
    /// the caller to report.
    fn release_unneeded(&mut self, workspace_id: WorkspaceId, made: &[Uuid]) -> Revocations {
        let unneeded: Vec<Uuid> = made
            .iter()
            .filter(|id| {
                self.container_grants()
                    .iter()
                    .map(ContainerGrantStatus::grant)
                    .find(|grant| grant.id() == **id)
                    .is_some_and(|grant| !self.grant_needed(workspace_id, grant, None, made, None))
            })
            .copied()
            .collect();
        self.revoke_each(&unneeded)
    }

    /// Revoke each grant in order, attempting every one.
    fn revoke_each(&mut self, grants: &[Uuid]) -> Revocations {
        let mut revocations = Revocations::default();
        for grant in grants {
            if let Some(workspace) = self.container_grant_workspace(*grant)
                && !revocations.workspaces.contains(&workspace)
            {
                revocations.workspaces.push(workspace);
            }
            match self.revoke_container_grant(*grant) {
                Ok(revocation) => revocations.revoked.push(revocation),
                Err(error) => revocations.failures.push((*grant, error)),
            }
        }
        revocations
    }
}

/// Whether `inner` lies beneath `outer` or is `outer`, by a live walk of
/// their directory identities or by their recorded canonical paths. Both
/// paths were canonical when recorded and never change, so the answer does
/// not depend on whether either folder still exists or can be read now. It
/// decides only which grants to keep or release; it never decides what a
/// run may reach, and a revocation still checks its folder's live identity.
fn holds(outer: &DirectoryRecord, inner: &DirectoryRecord) -> bool {
    if outer.contains(inner).unwrap_or(false) {
        return true;
    }
    match (outer.live_path(), inner.live_path()) {
        (Ok(outer), Ok(inner)) => inner.starts_with(outer),
        _ => false,
    }
}

/// Validate and apply one assignment action during replay or append preflight.
pub(super) fn apply(state: &mut ControlSnapshot, action: &Action) -> Result<(), ControlError> {
    let (workspace_id, profile_id, assign) = match action {
        Action::ProfileAssigned {
            workspace_id,
            profile_id,
        } => (*workspace_id, *profile_id, true),
        Action::ProfileUnassigned {
            workspace_id,
            profile_id,
        } => (*workspace_id, *profile_id, false),
        _ => return Err(ControlError::Invalid("not a profile assignment action")),
    };
    if !state
        .profiles
        .iter()
        .any(|status| status.profile().id() == profile_id)
    {
        return Err(ControlError::Invalid("assignment of an unknown profile"));
    }
    let workspace = state
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or(ControlError::Invalid("assignment for an unknown workspace"))?;
    let index = workspace.profiles.iter().position(|id| *id == profile_id);
    match (assign, index) {
        (true, None) => workspace.profiles.push(profile_id),
        (false, Some(index)) => {
            workspace.profiles.remove(index);
        }
        (true, Some(_)) => return Err(ControlError::Invalid("duplicate profile assignment")),
        (false, None) => {
            return Err(ControlError::Invalid(
                "removal of an absent profile assignment",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
