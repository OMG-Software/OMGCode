//! Toolchain profiles and container grants generated from the toolchains
//! installed on this computer, for the `settings` window (ADR-0103).
//!
//! Discovery is core's trusted host-side service
//! ([`omgcode_core::toolchain_discovery`]): it reads folders and a few small
//! installer-written files and runs no program. This module only shows its
//! proposals and applies the ones the operator selects:
//!
//! - The window receives display-safe text and a digest for each proposal,
//!   and sends back digests, never a path. Each change discovers again and
//!   applies only proposals whose digest still matches, so a selection made
//!   from an outdated list is refused as `discovery_changed`.
//! - Each proposal is checked with core's [`ControlStore::check_profile`],
//!   and its refusal reason is shown; nothing is recorded by a check.
//! - Adding profiles calls [`ControlStore::add_profile`], and granting
//!   folders calls [`ControlStore::grant_container_access`], with the same
//!   validation and events as the CLI and the hand-entered forms (ADR-0088).
//!   Each batch is applied after one native confirmation that lists every
//!   profile's roots, `PATH` entries, and variables, or every folder, and
//!   uses the revision the operator confirmed, advancing it once per record.
//! - Unlike the hand-entered form, the confirmation shows variable values:
//!   discovery derived them from installation paths, the operator typed none
//!   of them, and none is a secret. Each is cut to a bounded length.
//! - On Windows the proposals say, for one selected workspace, which roots
//!   its container can already read, which need a read-execute grant, and
//!   which need administrator rights to grant. Only folders whose
//!   access-control list this account can change are offered, so a folder
//!   only an administrator can change is never recorded as a grant. A
//!   recorded grant can still stay `pending` when its access list cannot be
//!   changed afterwards; the command then reports `container_access_failed`,
//!   and the operator revokes the grant. A folder whose access-control list
//!   cannot be read is named and skipped, and every folder to grant is
//!   checked with core's grant rules
//!   ([`ControlStore::check_container_grants`]) before the dialog.
//! - Access-control lists are read while the settings store is closed.
//!
//! As for every settings change, the store is released before a dialog.
//!
//! [`ControlStore::check_profile`]: omgcode_core::control::ControlStore::check_profile
//! [`ControlStore::add_profile`]: omgcode_core::control::ControlStore::add_profile
//! [`ControlStore::grant_container_access`]: omgcode_core::control::ControlStore::grant_container_access
//! [`ControlStore::check_container_grants`]: omgcode_core::control::ControlStore::check_container_grants

use std::path::PathBuf;

use omgcode_core::control::{
    CONTAINER_GRANTS_SUPPORTED, ContainerGrantAccess, ContainerGrantState, ControlError,
    ControlStore, RootGrantNeed, root_grant_need,
};
use omgcode_core::policy::WorkspaceId;
use omgcode_core::toolchain_discovery::{
    DiscoveryHost, ToolchainFamily, ToolchainProposal, discover, discover_bounded,
};
use omgcode_core::turn::display_safe_line;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::DesktopService;
use super::error::DesktopError;
use super::settings::overview_of;
use super::settings_changes::{
    ChangeGuard, Confirmation, SettingsPrompt, check_revision, confirm, shown,
};
use super::state::folder_of_directory;
use super::wire::SettingsOverview;

/// Most proposals one change applies, so one confirmation stays readable.
pub const MAX_SELECTED_PROPOSALS: usize = 8;
/// Longest variable value a confirmation shows, in characters.
const SHOWN_VALUE_CHARS: usize = 160;
/// Longest version a view or confirmation shows, in characters.
const SHOWN_VERSION_CHARS: usize = 80;

/// Arguments of `settings_discover_toolchains`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolchainDiscoveryRequest {
    /// On Windows, a registered workspace whose container access the roots
    /// report. `None` reports none.
    pub workspace_id: Option<Uuid>,
}

/// Arguments of `settings_add_discovered_profiles`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredProfilesRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// The digests of the selected proposals, 1 to 8.
    pub digests: Vec<String>,
    /// The workspace the refreshed list reports access for, as in
    /// [`ToolchainDiscoveryRequest`].
    pub workspace_id: Option<Uuid>,
}

/// Arguments of `settings_grant_discovered_roots`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredGrantsRequest {
    /// The settings revision the window last read.
    pub revision: u64,
    /// The registered workspace whose container gets the grants.
    pub workspace_id: Uuid,
    /// The digests of the selected proposals, 1 to 8.
    pub digests: Vec<String>,
}

/// One proposed root and what the selected workspace's container needs to
/// read it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProposedRootView {
    /// Display-safe folder text. Never reopen a path from it.
    pub folder: String,
    /// `not_applicable` off Windows or with no workspace selected.
    pub grant: RootGrantNeed,
}

/// One proposed variable. Values are derived setup values, not secrets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProposedVariableView {
    /// The variable name.
    pub name: String,
    /// The display-safe value.
    pub value: String,
}

/// One proposal as the window shows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolchainProposalView {
    /// Selects the proposal in a change; see the module documentation.
    pub digest: String,
    /// The family.
    pub family: ToolchainFamily,
    /// The family's fixed label.
    pub family_label: &'static str,
    /// The proposed profile name, which satisfies core's name rule.
    pub name: String,
    /// A display-safe version taken from folder or file names.
    pub version: Option<String>,
    /// The roots, in order.
    pub read_only_roots: Vec<ProposedRootView>,
    /// Display-safe `PATH` entries, in order.
    pub path_entries: Vec<String>,
    /// The variables, in order.
    pub environment: Vec<ProposedVariableView>,
    /// Fixed remarks.
    pub notes: Vec<&'static str>,
    /// Why core would refuse the proposal now, if it would.
    pub refusal: Option<String>,
    /// A profile with this name is already recorded.
    pub recorded: bool,
}

/// What `settings_discover_toolchains` and the two changes return.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolchainDiscovery {
    /// The settings as they are now.
    pub overview: SettingsOverview,
    /// The workspace the roots report access for, if any.
    pub workspace_id: Option<Uuid>,
    /// On Windows with a workspace selected: whether it has the active
    /// read-write grant on its own folder that every run needs. `None`
    /// otherwise.
    pub workspace_grant_active: Option<bool>,
    /// The proposals, at most 32.
    pub proposals: Vec<ToolchainProposalView>,
    /// Installations found and not proposed: more than eight under one name,
    /// or more than 32 in all.
    pub omitted: usize,
}

/// What a view does when its workspace is not registered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingWorkspace {
    /// Refuse with `unknown_workspace`, as a plain listing does.
    Refuse,
    /// Report no container access, as the list after a change does: the
    /// change is recorded, so a failure would misreport it.
    Ignore,
}

impl DesktopError {
    /// Discovery runs only where toolchain profiles exist.
    fn discovery_unsupported() -> Self {
        Self::execution(
            "unsupported_platform",
            "Toolchain profiles exist only on Linux and Windows.",
        )
    }

    /// A selected digest matched no proposal of a new discovery.
    fn discovery_changed() -> Self {
        Self::execution(
            "discovery_changed",
            "The installed toolchains or the proposals changed since the list was made. Generate the list again and select again.",
        )
    }

    /// No root of the selected proposals can be granted: each is readable,
    /// needs an administrator, or has an access-control list that could not
    /// be read.
    fn nothing_to_grant(administrator: usize, unknown: usize) -> Self {
        if administrator == 0 && unknown == 0 {
            return Self::execution(
                "nothing_to_grant",
                "The workspace's container can already read every folder these profiles need.",
            );
        }
        let mut reasons = Vec::new();
        let mut advice = Vec::new();
        if administrator > 0 {
            reasons.push(format!(
                "{administrator} folder(s) need an administrator to change their access-control lists"
            ));
            advice.push(
                "Ask an administrator, or install a copy of the toolchain under your own profile.",
            );
        }
        if unknown > 0 {
            reasons.push(format!(
                "the access-control lists of {unknown} folder(s) could not be read"
            ));
            advice.push(
                "Check that each folder still exists and is not a link, then generate the list again.",
            );
        }
        Self::execution(
            "nothing_to_grant",
            format!(
                "No folder can be granted from this account: {}. {}",
                reasons.join(", and "),
                advice.join(" ")
            ),
        )
    }

    /// A batch stopped after recording some of its records.
    fn partly_applied(what: &str, done: usize, total: usize, cause: &Self) -> Self {
        Self::execution(
            cause.code(),
            format!(
                "{done} of {total} {what} were recorded before this failure: {}",
                cause.message()
            ),
        )
    }
}

/// Check the selection's shape before any discovery or dialog.
fn check_digests(digests: &[String]) -> Result<(), DesktopError> {
    if digests.is_empty() || digests.len() > MAX_SELECTED_PROPOSALS {
        return Err(DesktopError::invalid_request(
            "Select 1 to 8 proposals at a time.",
        ));
    }
    for (index, digest) in digests.iter().enumerate() {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DesktopError::invalid_request(
                "A selection is not a proposal digest.",
            ));
        }
        if digests[..index].contains(digest) {
            return Err(DesktopError::invalid_request(
                "A proposal is selected twice.",
            ));
        }
    }
    Ok(())
}

/// The selected proposals of a fresh discovery, in the order selected.
fn selected(
    host: &DiscoveryHost,
    digests: &[String],
) -> Result<Vec<ToolchainProposal>, DesktopError> {
    let digested: Vec<(String, ToolchainProposal)> = discover(host)
        .into_iter()
        .map(|proposal| (proposal.digest(), proposal))
        .collect();
    digests
        .iter()
        .map(|digest| {
            digested
                .iter()
                .find(|(known, _)| known == digest)
                .map(|(_, proposal)| proposal.clone())
                .ok_or_else(DesktopError::discovery_changed)
        })
        .collect()
}

/// `text` as one bounded, display-safe line.
fn bounded(text: &str, limit: usize) -> String {
    let safe = display_safe_line(text);
    if safe.chars().count() <= limit {
        safe
    } else {
        let mut cut: String = safe.chars().take(limit).collect();
        cut.push('…');
        cut
    }
}

/// Why core refuses a proposal, as the window shows it.
fn refusal_text(error: &ControlError) -> String {
    match error {
        ControlError::Io(_) | ControlError::WorkspacePath(_) => {
            "A folder could not be read as it is now.".to_owned()
        }
        other => DesktopError::from_control(other).message().to_owned(),
    }
}

/// The view of one proposal against the store as it is now. `needs` gives
/// each root's container access, in root order, when a workspace is
/// selected.
fn proposal_view(
    store: &ControlStore,
    proposal: &ToolchainProposal,
    needs: Option<&[RootGrantNeed]>,
) -> ToolchainProposalView {
    ToolchainProposalView {
        digest: proposal.digest(),
        family: proposal.family(),
        family_label: proposal.family().label(),
        name: proposal.name().to_owned(),
        version: proposal
            .version()
            .map(|version| bounded(version, SHOWN_VERSION_CHARS)),
        read_only_roots: proposal
            .read_only_roots()
            .iter()
            .enumerate()
            .map(|(index, root)| ProposedRootView {
                folder: shown(root),
                grant: needs
                    .and_then(|needs| needs.get(index).copied())
                    .unwrap_or(RootGrantNeed::NotApplicable),
            })
            .collect(),
        path_entries: proposal
            .path_entries()
            .iter()
            .map(|entry| shown(entry))
            .collect(),
        environment: proposal
            .environment()
            .iter()
            .map(|(name, value)| ProposedVariableView {
                name: display_safe_line(name),
                value: display_safe_line(value),
            })
            .collect(),
        notes: proposal.notes().iter().map(|note| note.text()).collect(),
        refusal: store
            .check_profile(&proposal.declaration())
            .err()
            .map(|error| refusal_text(&error)),
        recorded: store.profile(proposal.name()).is_some(),
    }
}

/// One profile as a confirmation lists it.
fn confirmation_entry(proposal: &ToolchainProposal) -> String {
    let heading = match proposal.version() {
        Some(version) => format!(
            "{} ({}, {})",
            proposal.name(),
            proposal.family().label(),
            bounded(version, SHOWN_VERSION_CHARS)
        ),
        None => format!("{} ({})", proposal.name(), proposal.family().label()),
    };
    let list = |paths: &[PathBuf]| {
        if paths.is_empty() {
            "    (none)".to_owned()
        } else {
            paths
                .iter()
                .map(|path| format!("    {}", shown(path)))
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    let variables = if proposal.environment().is_empty() {
        "    (none)".to_owned()
    } else {
        proposal
            .environment()
            .iter()
            .map(|(name, value)| {
                format!(
                    "    {}={}",
                    display_safe_line(name),
                    bounded(value, SHOWN_VALUE_CHARS)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{heading}\n  Read-only roots:\n{}\n  PATH entries:\n{}\n  Environment variables:\n{variables}",
        list(proposal.read_only_roots()),
        list(proposal.path_entries()),
    )
}

/// The native confirmation for adding the selected proposals.
fn add_confirmation(chosen: &[ToolchainProposal]) -> Confirmation {
    let platform = if cfg!(windows) { "Windows" } else { "Linux" };
    Confirmation {
        title: "Add generated toolchain profiles",
        message: format!(
            "Add {} toolchain profile(s) for {platform}? A run that selects a profile can read and execute everything beneath its roots.\n\n{}\n\nOMGCode found these by reading folders on this computer; it ran no program. A profile grants nothing by itself: a run still needs the workspace's execute preference. Variable values are stored in plain text and shown by status.",
            chosen.len(),
            chosen
                .iter()
                .map(confirmation_entry)
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
    }
}

/// The native confirmation for granting `folders` to a workspace. It also
/// names the folders that are not granted: those that need an administrator
/// and those whose access-control list could not be read.
fn grants_confirmation(
    workspace: &str,
    folders: &[PathBuf],
    administrator: &[PathBuf],
    unknown: &[PathBuf],
) -> Confirmation {
    let list = |paths: &[PathBuf]| {
        paths
            .iter()
            .map(|path| format!("  {}", shown(path)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut skipped = String::new();
    if !administrator.is_empty() {
        skipped.push_str(&format!(
            "\n\nThese folders also need a grant, but this account cannot change their access-control lists, so they are not granted:\n{}",
            list(administrator)
        ));
    }
    if !unknown.is_empty() {
        skipped.push_str(&format!(
            "\n\nOMGCode could not read the access-control lists of these folders, so they are not granted. Check that each still exists and is not a link, then generate the list again:\n{}",
            list(unknown)
        ));
    }
    Confirmation {
        title: "Grant toolchain folders to a workspace",
        message: format!(
            "Give the workspace\n{workspace}\nread and execute access to these folders?\n{}\n\nThis changes each folder's access-control list on disk for that workspace's AppContainer, and each grant stays until you revoke it. Runs in the workspace can then read and execute everything beneath them.{skipped}",
            list(folders)
        ),
    }
}

impl DesktopService {
    /// `settings` only. Find the toolchains installed on `host` and return a
    /// proposal for each, checked against the settings as they are now.
    /// Nothing is recorded, granted, or run. On Windows, with a workspace,
    /// each root says whether that workspace's container can read it.
    ///
    /// # Errors
    ///
    /// `unsupported_platform` outside Linux and Windows; `unknown_workspace`;
    /// `busy`.
    pub fn settings_discover_toolchains(
        &self,
        request: ToolchainDiscoveryRequest,
        host: &DiscoveryHost,
    ) -> Result<ToolchainDiscovery, DesktopError> {
        self.discovery_view(host, request.workspace_id, MissingWorkspace::Refuse)
    }

    /// `settings` only. After one native confirmation that lists each
    /// selected proposal's roots, `PATH` entries, and variables, record the
    /// proposals as `omgcode workspace profile-add` does, in the order
    /// selected, starting at the confirmed revision.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a selection outside its bounds;
    /// `discovery_changed` when a selected proposal is no longer proposed;
    /// `unknown_workspace`, before any dialog; `control` with core's reason,
    /// before any dialog, for a proposal core would refuse; `cancelled`;
    /// `stale_revision`; `settings_change_running`; `busy`. A failure after
    /// some profiles were recorded says how many. Once the profiles are
    /// recorded, a workspace unregistered meanwhile only leaves the refreshed
    /// list without container access.
    pub fn settings_add_discovered_profiles(
        &self,
        request: DiscoveredProfilesRequest,
        host: &DiscoveryHost,
        prompt: &dyn SettingsPrompt,
    ) -> Result<ToolchainDiscovery, DesktopError> {
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_digests(&request.digests)?;
        if host.platform() != current_platform()? {
            return Err(DesktopError::discovery_unsupported());
        }
        let chosen = selected(host, &request.digests)?;
        {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            if let Some(workspace_id) = request.workspace_id {
                store
                    .registered_workspace(workspace_id)
                    .ok_or_else(DesktopError::unknown_workspace)?;
            }
            for proposal in &chosen {
                store
                    .check_profile(&proposal.declaration())
                    .map_err(|error| {
                        DesktopError::execution(
                            DesktopError::from_control(&error).code(),
                            format!("{}: {}", proposal.name(), refusal_text(&error)),
                        )
                    })?;
            }
        }
        confirm(prompt, &add_confirmation(&chosen))?;
        {
            let mut store = self.inner.open_or_create_control()?;
            let mut expected = request.revision;
            for (done, proposal) in chosen.iter().enumerate() {
                if let Err(error) = store.add_profile(&proposal.declaration(), expected) {
                    let cause = DesktopError::from_control(&error);
                    return Err(if done == 0 {
                        cause
                    } else {
                        DesktopError::partly_applied("profiles", done, chosen.len(), &cause)
                    });
                }
                expected = store.revision();
            }
        }
        self.discovery_view(host, request.workspace_id, MissingWorkspace::Ignore)
    }

    /// `settings` only. Windows only. For the selected proposals, find the
    /// roots the workspace's container cannot read and this account can
    /// grant, and after one native confirmation that names the workspace and
    /// every folder, grant each read-execute as `omgcode workspace
    /// container-grant` does, starting at the confirmed revision. Folders
    /// that need administrator rights, and folders whose access-control list
    /// could not be read, are named and skipped. Every folder to grant is
    /// checked with core's grant rules before the dialog.
    ///
    /// # Errors
    ///
    /// `unsupported_platform` outside Windows; `invalid_request`;
    /// `discovery_changed`; `unknown_workspace`; `nothing_to_grant`;
    /// `control` with core's reason, before any dialog, for a folder core
    /// would refuse; `cancelled`; `stale_revision`;
    /// `settings_change_running`; `busy`; `container_access_failed` when a
    /// grant was recorded but its access list could not be changed (it stays
    /// `pending`; revoke it). A failure after some grants were applied says
    /// how many.
    pub fn settings_grant_discovered_roots(
        &self,
        request: DiscoveredGrantsRequest,
        host: &DiscoveryHost,
        prompt: &dyn SettingsPrompt,
    ) -> Result<ToolchainDiscovery, DesktopError> {
        if !CONTAINER_GRANTS_SUPPORTED {
            return Err(DesktopError::unsupported_platform());
        }
        let _guard = ChangeGuard::take(&self.inner.settings_change)?;
        check_digests(&request.digests)?;
        if host.platform() != current_platform()? {
            return Err(DesktopError::discovery_unsupported());
        }
        let chosen = selected(host, &request.digests)?;
        let workspace_id = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?
                .id()
        };
        // The access-control lists are read while the store is closed.
        let mut folders: Vec<PathBuf> = Vec::new();
        let mut administrator: Vec<PathBuf> = Vec::new();
        let mut unknown: Vec<PathBuf> = Vec::new();
        for root in chosen.iter().flat_map(ToolchainProposal::read_only_roots) {
            let list = match root_grant_need(workspace_id, root) {
                RootGrantNeed::NeedsGrant => &mut folders,
                RootGrantNeed::NeedsAdministrator => &mut administrator,
                RootGrantNeed::Unknown => &mut unknown,
                RootGrantNeed::NotApplicable | RootGrantNeed::Readable => continue,
            };
            if !list.contains(root) {
                list.push(root.clone());
            }
        }
        // A grant reaches everything beneath its folder.
        let needed = folders.clone();
        folders.retain(|folder| {
            !needed
                .iter()
                .any(|outer| outer != folder && folder.starts_with(outer))
        });
        if folders.is_empty() {
            return Err(DesktopError::nothing_to_grant(
                administrator.len(),
                unknown.len(),
            ));
        }
        let workspace_folder = {
            let store = self.inner.open_or_create_control()?;
            check_revision(&store, request.revision)?;
            let workspace = store
                .registered_workspace(request.workspace_id)
                .ok_or_else(DesktopError::unknown_workspace)?;
            store
                .check_container_grants(workspace_id, &folders, ContainerGrantAccess::ReadExecute)
                .map_err(|(index, error)| {
                    let folder = folders.get(index).map(|folder| shown(folder));
                    DesktopError::execution(
                        DesktopError::from_control(&error).code(),
                        format!("{}: {}", folder.unwrap_or_default(), refusal_text(&error)),
                    )
                })?;
            folder_of_directory(workspace.directory_path())
        };
        confirm(
            prompt,
            &grants_confirmation(&workspace_folder, &folders, &administrator, &unknown),
        )?;
        {
            let mut store = self.inner.open_or_create_control()?;
            let mut expected = request.revision;
            for (done, folder) in folders.iter().enumerate() {
                if let Err(error) = store.grant_container_access(
                    workspace_id,
                    folder,
                    ContainerGrantAccess::ReadExecute,
                    expected,
                ) {
                    let cause = DesktopError::from_grant(&error);
                    return Err(if done == 0 {
                        cause
                    } else {
                        DesktopError::partly_applied("grants", done, folders.len(), &cause)
                    });
                }
                expected = store.revision();
            }
        }
        self.discovery_view(host, Some(request.workspace_id), MissingWorkspace::Ignore)
    }

    /// Discover on `host` and build the view against the store as it is now.
    /// Discovery, and the access-control reads for a workspace, run while the
    /// store is closed: the settings lock is held only to resolve the
    /// workspace and to build the view.
    fn discovery_view(
        &self,
        host: &DiscoveryHost,
        workspace_id: Option<Uuid>,
        missing: MissingWorkspace,
    ) -> Result<ToolchainDiscovery, DesktopError> {
        if host.platform() != current_platform()? {
            return Err(DesktopError::discovery_unsupported());
        }
        let found = discover_bounded(host);
        let resolve = |store: &ControlStore| -> Result<Option<WorkspaceId>, DesktopError> {
            let Some(id) = workspace_id else {
                return Ok(None);
            };
            match (store.registered_workspace(id), missing) {
                (Some(workspace), _) => Ok(Some(workspace.id())),
                (None, MissingWorkspace::Refuse) => Err(DesktopError::unknown_workspace()),
                (None, MissingWorkspace::Ignore) => Ok(None),
            }
        };
        let workspace = match workspace_id {
            Some(_) => resolve(&self.inner.open_or_create_control()?)?,
            None => None,
        };
        let needs: Vec<Vec<RootGrantNeed>> = found
            .proposals
            .iter()
            .map(|proposal| {
                proposal
                    .read_only_roots()
                    .iter()
                    .map(|root| {
                        workspace.map_or(RootGrantNeed::NotApplicable, |workspace| {
                            root_grant_need(workspace, root)
                        })
                    })
                    .collect()
            })
            .collect();
        let store = self.inner.open_or_create_control()?;
        // The workspace can be unregistered while the store is closed.
        let workspace = match workspace {
            Some(_) => resolve(&store)?,
            None => None,
        };
        // Core records a read-write grant only on the workspace's own folder
        // (`ControlStore::check_grant_scope` when it is granted, and the
        // replay `apply` in `omgcode_core::control::container`), so an active
        // read-write grant of this workspace is the one on its folder.
        let workspace_grant_active =
            workspace
                .filter(|_| CONTAINER_GRANTS_SUPPORTED)
                .map(|workspace| {
                    store.container_grants().iter().any(|status| {
                        status.grant().workspace_id() == workspace
                            && status.grant().access() == ContainerGrantAccess::ReadWrite
                            && status.state() == ContainerGrantState::Active
                    })
                });
        let views = found
            .proposals
            .iter()
            .zip(&needs)
            .map(|(proposal, needs)| {
                proposal_view(&store, proposal, workspace.map(|_| needs.as_slice()))
            })
            .collect();
        Ok(ToolchainDiscovery {
            overview: overview_of(&self.inner, store),
            workspace_id: workspace.map(|workspace| *workspace.as_uuid()),
            workspace_grant_active,
            proposals: views,
            omitted: found.omitted,
        })
    }
}

/// The profile platform of this build.
fn current_platform() -> Result<omgcode_core::control::ProfilePlatform, DesktopError> {
    omgcode_core::control::ProfilePlatform::current()
        .ok_or_else(DesktopError::discovery_unsupported)
}

/// This computer as discovery sees it: its standard install locations and
/// the operator's `PATH`, `JAVA_HOME`, `GOROOT`, and `RUSTUP_HOME`, read from
/// this process's environment. Repository configuration such as
/// `rust-toolchain.toml` is never read.
///
/// # Errors
///
/// `unsupported_platform` outside Linux and Windows.
pub fn discovery_host() -> Result<DiscoveryHost, DesktopError> {
    DiscoveryHost::from_environment().ok_or_else(DesktopError::discovery_unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_selection_is_one_to_eight_distinct_lowercase_digests() {
        assert!(check_digests(&[DIGEST.to_owned()]).is_ok());
        for refused in [
            Vec::new(),
            vec![DIGEST.to_owned(); 2],
            vec![DIGEST.to_uppercase()],
            vec![DIGEST[..63].to_owned()],
            vec![format!("{}g", &DIGEST[..63])],
            (0..9).map(|n| format!("{:064x}", n)).collect(),
        ] {
            let error = check_digests(&refused).expect_err("refused");
            assert_eq!(error.code(), "invalid_request", "{refused:?}");
        }
    }

    #[test]
    fn requests_refuse_unknown_fields_and_paths() {
        for refused in [
            serde_json::json!({ "workspace_id": null, "roots": ["C:\\x"] }),
            serde_json::json!({ "path": "/usr" }),
        ] {
            assert!(serde_json::from_value::<ToolchainDiscoveryRequest>(refused).is_err());
        }
        assert!(
            serde_json::from_value::<DiscoveredProfilesRequest>(serde_json::json!({
                "revision": 1, "digests": [DIGEST], "workspace_id": null, "name": "rust"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DiscoveredGrantsRequest>(serde_json::json!({
                "revision": 1, "digests": [DIGEST],
                "workspace_id": "6f1c1f0e-8f42-4c5b-9a55-0d2a3c4b5d6e", "folder": "C:\\x"
            }))
            .is_err()
        );
    }

    #[test]
    fn long_values_and_versions_are_cut_and_made_safe() {
        let shown = bounded(&format!("{}\u{202e}x", "a".repeat(200)), SHOWN_VALUE_CHARS);
        assert_eq!(shown.chars().count(), SHOWN_VALUE_CHARS + 1);
        assert!(shown.ends_with('…'));
        assert!(!bounded("a\nb\u{202e}", 80).contains(['\n', '\u{202e}']));
    }

    #[test]
    fn the_grant_confirmation_names_the_workspace_every_folder_and_the_skipped_ones() {
        let confirmation = grants_confirmation(
            "C:\\dev\\app",
            &[PathBuf::from("C:\\Users\\a\\.rustup\\toolchains\\stable")],
            &[PathBuf::from("C:\\Strawberry")],
            &[PathBuf::from("C:\\Linked\\jdk")],
        );
        for expected in [
            "C:\\dev\\app",
            "stable",
            "read and execute",
            "access-control list",
            "AppContainer",
            "C:\\Strawberry",
            "cannot change",
            "C:\\Linked\\jdk",
            "could not read",
        ] {
            assert!(
                confirmation.message.contains(expected),
                "{expected}: {}",
                confirmation.message
            );
        }
        let without = grants_confirmation("w", &[PathBuf::from("C:\\x")], &[], &[]);
        assert!(!without.message.contains("cannot change"));
        assert!(!without.message.contains("could not read"));
    }

    #[test]
    fn nothing_to_grant_says_why_each_folder_was_skipped() {
        let readable = DesktopError::nothing_to_grant(0, 0);
        assert_eq!(readable.code(), "nothing_to_grant");
        assert!(readable.message().contains("can already read every folder"));
        let unread = DesktopError::nothing_to_grant(0, 2);
        assert!(
            !unread.message().contains("can already read"),
            "{}",
            unread.message()
        );
        assert!(unread.message().contains("2 folder(s) could not be read"));
        assert!(unread.message().contains("not a link"));
        assert!(!unread.message().contains("administrator"));
        let both = DesktopError::nothing_to_grant(1, 3);
        assert!(both.message().contains("1 folder(s) need an administrator"));
        assert!(both.message().contains("3 folder(s) could not be read"));
    }
}
