//! Operator commands for workspace registration, configuration-only policy
//! checks, recorded AppContainer grants, and toolchain profiles.
#![forbid(unsafe_code)]

use clap::{Args, Subcommand, ValueEnum};
use omgcode_core::control::{
    ContainerGrantAccess, ControlError, ControlStore, PackageProfileDeclaration,
    ProfileDeclaration, ProviderProfileDeclaration,
};
use omgcode_core::policy::{
    Access, Operation, PermissionSet, PolicyRequest, WorkspaceId, WriteTool,
};
use omgcode_core::run;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::{CliError, credential_cli, print_json};

#[derive(Subcommand)]
pub(crate) enum WorkspaceCommand {
    /// Display and initialize the current trusted workspace configuration.
    Status,
    /// Set the maximum development-data root using an expected revision.
    SetRoot {
        path: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Select one workspace's fetched-package cache root, outside application
    /// storage and the development root.
    SetPackageCache {
        workspace: String,
        path: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Clear one workspace's package cache root. Cached files are left in place.
    ClearPackageCache {
        workspace: String,
        #[arg(long)]
        revision: u64,
    },
    /// Register a workspace below the configured development root.
    Register {
        path: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Remove one workspace's registration and the settings that name it: its
    /// preferences, package cache selection, skill overrides, checkpoint
    /// record, and saved desktop instructions. The folder and its files are
    /// not touched. Refused while a container grant names the workspace; revoke
    /// it with `container-revoke` first. Registering the folder again assigns a
    /// new identifier.
    Unregister {
        workspace: String,
        #[arg(long)]
        revision: u64,
    },
    /// Change one or more policy flags using an expected revision.
    Permissions(PermissionsArgs),
    /// Evaluate one diagnostic policy request and record it in the audit log.
    Check(CheckArgs),
    /// Show recent policy audit entries.
    Audit {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Grant the workspace's AppContainer access to a directory (Windows).
    ///
    /// Read-write grants are limited to the workspace directory. The grant
    /// persists until it is revoked. `omgcode run` needs a read-write grant
    /// on the workspace.
    ContainerGrant(ContainerGrantArgs),
    /// Revoke one recorded container grant. Needs no revision.
    ContainerRevoke {
        /// The grant identifier shown by `container-grant` and `status`.
        grant: Uuid,
    },
    /// Record a toolchain profile for `omgcode run`.
    ProfileAdd(ProfileAddArgs),
    /// Show one recorded toolchain profile.
    ProfileShow { name: String },
    /// Remove a toolchain profile.
    ProfileRemove {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Record a package-network profile. Configuration only: no run accepts it.
    PackageProfileAdd(PackageProfileAddArgs),
    /// Show one recorded package-network profile.
    PackageProfileShow { name: String },
    /// Remove a package-network profile. Credentials bound to its origins are
    /// unbound by the same recorded event and then deleted from the native store.
    PackageProfileRemove {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Store a registry credential and bind it to one declared origin of a
    /// package-network profile. Only `packages fetch` reads it, and it sends it
    /// to that origin alone.
    ///
    /// The secret is read from standard input only, up to 16 KiB; one trailing
    /// newline is removed. It must be visible ASCII without control characters
    /// or surrounding whitespace. `basic` needs `user:password`. A previously
    /// bound credential is deleted after the new binding is recorded.
    PackageCredentialSet {
        /// The package-network profile name.
        name: String,
        /// One of the profile's declared origins, such as
        /// https://registry.example.com/api/.
        #[arg(long)]
        origin: String,
        /// How the secret is sent: `bearer` or `basic`.
        #[arg(long)]
        auth: String,
        #[arg(long)]
        revision: u64,
    },
    /// Unbind a registry credential from one origin, then delete it natively.
    PackageCredentialRemove {
        /// The package-network profile name.
        name: String,
        /// The origin the credential is bound to.
        #[arg(long)]
        origin: String,
        #[arg(long)]
        revision: u64,
    },
    /// Record a provider connection shape, optionally with its endpoint. No
    /// credential is stored; the local adapter reads a selected lm_studio or
    /// ollama_local profile.
    ProviderProfileAdd(ProviderProfileAddArgs),
    /// Choose the connection a desktop shell started without `--endpoint` or
    /// `--provider-profile` sends new turns to: a recorded lm_studio or
    /// ollama_local profile with an endpoint, or `--default` for the default
    /// endpoint and OMGCODE_LM_TOKEN. `chat` is not affected.
    ProviderProfileSelect {
        #[arg(required_unless_present = "default")]
        name: Option<String>,
        /// Clear the selection instead of naming a profile.
        #[arg(long, conflicts_with = "name")]
        default: bool,
        #[arg(long)]
        revision: u64,
    },
    /// Show one recorded provider profile.
    ProviderProfileShow { name: String },
    /// Remove a provider profile. A bound credential is unbound by the same
    /// recorded event and then deleted from the native store.
    ProviderProfileRemove {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Store a new credential for an `api_key` provider profile and bind it.
    ///
    /// The secret is read from standard input only, up to 16 KiB; one
    /// trailing newline is removed. A previously bound credential is deleted
    /// after the new binding is recorded. The local adapter reads it for a
    /// selected lm_studio profile.
    ProviderCredentialSet {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Unbind a provider profile's credential, then delete it natively.
    ProviderCredentialRemove {
        name: String,
        #[arg(long)]
        revision: u64,
    },
}

#[derive(Args)]
pub(crate) struct ProviderProfileAddArgs {
    /// The profile name: ASCII letters, digits, '.', '-', or '_'.
    name: String,
    /// One of: openai, anthropic, ollama_local, ollama_cloud, open_router,
    /// lm_studio.
    #[arg(long)]
    kind: String,
    /// One of: subscription, api_key, none.
    #[arg(long)]
    auth: String,
    /// The endpoint the profile's adapter uses. lm_studio and ollama_local
    /// accept http://<loopback IP>:<port>/v1; ollama_cloud accepts exactly
    /// https://ollama.com/v1. No other kind accepts one. It cannot be
    /// changed later; remove the profile and add it again.
    #[arg(long)]
    endpoint: Option<String>,
    #[arg(long)]
    revision: u64,
}

#[derive(Args)]
pub(crate) struct PackageProfileAddArgs {
    /// The profile name: ASCII letters, digits, '.', '-', or '_'.
    name: String,
    /// A registry origin, such as https://registry.example.com/api/.
    /// Repeat for each origin. http is accepted only for a loopback fixture.
    #[arg(long = "registry", required = true)]
    registries: Vec<String>,
    #[arg(long)]
    revision: u64,
}

#[derive(Args)]
pub(crate) struct ProfileAddArgs {
    /// The profile name: ASCII letters, digits, '.', '-', or '_'.
    name: String,
    /// A directory the run may read and execute. Repeat for each root.
    #[arg(long = "root", required = true)]
    roots: Vec<PathBuf>,
    /// A directory inside a root, searched for bare program names and joined
    /// in order as PATH. Repeat for each entry.
    #[arg(long = "path")]
    path_entries: Vec<PathBuf>,
    /// An extra environment value, NAME=VALUE. Stored in the private
    /// settings journal and shown by `status`; never pass secrets.
    #[arg(long = "env")]
    environment: Vec<String>,
    #[arg(long)]
    revision: u64,
}

#[derive(Args)]
pub(crate) struct ContainerGrantArgs {
    /// The workspace identifier.
    id: String,
    /// The directory to grant.
    path: PathBuf,
    #[arg(long, value_enum)]
    access: AccessArg,
    #[arg(long)]
    revision: u64,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum AccessArg {
    ReadExecute,
    ReadWrite,
}

impl From<AccessArg> for ContainerGrantAccess {
    fn from(value: AccessArg) -> Self {
        match value {
            AccessArg::ReadExecute => Self::ReadExecute,
            AccessArg::ReadWrite => Self::ReadWrite,
        }
    }
}

#[derive(Args)]
pub(crate) struct PermissionsArgs {
    id: String,
    #[arg(long)]
    revision: u64,
    /// `deny`, `prompt`, or `allow`. The legacy `true` means `allow`.
    #[arg(long, value_parser = parse_read_access)]
    read: Option<Access>,
    /// `deny`, `prompt`, or `allow`. The legacy `true` means `prompt`.
    #[arg(long, value_parser = parse_access)]
    write: Option<Access>,
    /// `deny`, `prompt`, or `allow`. The legacy `true` means `prompt`.
    #[arg(long, value_parser = parse_access)]
    execute: Option<Access>,
    /// `deny`, `prompt`, or `allow`. No model tool uses it yet.
    #[arg(long = "task-network", value_parser = parse_access)]
    task_network: Option<Access>,
    /// Let chat offer the local model workspace tools at all.
    #[arg(long = "model-tools")]
    model_tools: Option<bool>,
    /// Let a trusted `run` select host-service Unix-socket access when it
    /// also passes `--host-services`. Refused on Windows at run time.
    /// `deny`, `prompt`, or `allow`; never offered to a model.
    #[arg(long = "host-services", value_parser = parse_access)]
    host_services: Option<Access>,
    /// Limit on the model edit tool; it can only narrow `--write`.
    #[arg(long = "edit-tool", value_parser = parse_tool_limit)]
    edit_tool: Option<Access>,
    /// Limit on the model multi-edit tool; it can only narrow `--write`.
    #[arg(long = "multi-edit-tool", value_parser = parse_tool_limit)]
    multi_edit_tool: Option<Access>,
    /// Limit on the model create tool; it can only narrow `--write`.
    #[arg(long = "create-tool", value_parser = parse_tool_limit)]
    create_tool: Option<Access>,
    /// Limit on the model rename tool; it can only narrow `--write`.
    #[arg(long = "rename-tool", value_parser = parse_tool_limit)]
    rename_tool: Option<Access>,
    /// Limit on the model delete tool; it can only narrow `--write`.
    #[arg(long = "delete-tool", value_parser = parse_tool_limit)]
    delete_tool: Option<Access>,
    /// Limit on the model create-directory tool; it can only narrow `--write`.
    #[arg(long = "create-directory-tool", value_parser = parse_tool_limit)]
    create_directory_tool: Option<Access>,
}

/// Parse a preference whose legacy `true` meant `prompt`.
fn parse_access(value: &str) -> Result<Access, String> {
    parse_with_legacy(value, Access::Prompt)
}

/// Parse `read`, whose legacy `true` meant `allow`.
fn parse_read_access(value: &str) -> Result<Access, String> {
    parse_with_legacy(value, Access::Allow)
}

/// Parse a per-tool limit, which has no legacy form.
fn parse_tool_limit(value: &str) -> Result<Access, String> {
    value.parse::<Access>().map_err(|error| error.to_string())
}

/// Accept `deny`, `prompt`, `allow`, and the earlier `true`/`false` flags.
fn parse_with_legacy(value: &str, legacy: Access) -> Result<Access, String> {
    match value {
        "true" => Ok(legacy),
        "false" => Ok(Access::Deny),
        other => other.parse::<Access>().map_err(|_| {
            "expected deny, prompt, or allow (or the legacy true or false)".to_owned()
        }),
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum OperationArg {
    Read,
    Write,
    Execute,
    TaskNetwork,
}

impl From<OperationArg> for Operation {
    fn from(value: OperationArg) -> Self {
        match value {
            OperationArg::Read => Self::Read,
            OperationArg::Write => Self::Write,
            OperationArg::Execute => Self::Execute,
            OperationArg::TaskNetwork => Self::TaskNetwork,
        }
    }
}

#[derive(Args)]
pub(crate) struct CheckArgs {
    id: String,
    #[arg(long)]
    session: Uuid,
    #[arg(long, value_enum)]
    operation: OperationArg,
    #[arg(long)]
    target_workspace: Option<String>,
}

/// Run one control command against trusted application storage.
pub(crate) fn run(action: WorkspaceCommand, data_root: &Path) -> Result<(), CliError> {
    match action {
        WorkspaceCommand::Status => {
            let store = open(data_root)?;
            print_json(&store.snapshot())
        }
        WorkspaceCommand::SetRoot { path, revision } => {
            let mut store = open(data_root)?;
            store
                .set_development_root(&path, revision)
                .map_err(control_error)?;
            let snapshot = store.snapshot();
            let listed = withdrawn_runs(&store);
            drop(store);
            stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
            listed?;
            print_json(&snapshot)
        }
        WorkspaceCommand::SetPackageCache {
            workspace,
            path,
            revision,
        } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            store
                .set_package_cache_root(workspace_id, &path, revision)
                .map_err(control_error)?;
            let snapshot = store.snapshot();
            let listed = withdrawn_runs(&store);
            drop(store);
            stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
            listed?;
            print_json(&snapshot)
        }
        WorkspaceCommand::ClearPackageCache {
            workspace,
            revision,
        } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            store
                .clear_package_cache_root(workspace_id, revision)
                .map_err(control_error)?;
            let snapshot = store.snapshot();
            let listed = withdrawn_runs(&store);
            drop(store);
            stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
            listed?;
            print_json(&snapshot)
        }
        WorkspaceCommand::Register { path, revision } => {
            let mut store = open(data_root)?;
            let registration = store
                .register_workspace(&path, revision)
                .map_err(control_error)?;
            print_json(&json!({
                "registration_id": registration.id(),
                "snapshot": store.snapshot(),
            }))
        }
        WorkspaceCommand::Unregister {
            workspace,
            revision,
        } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            store
                .unregister_workspace(workspace_id, revision)
                .map_err(control_error)?;
            let snapshot = store.snapshot();
            let listed = withdrawn_runs(&store);
            drop(store);
            stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
            listed?;
            // The change is recorded. A saved desktop instructions file is
            // removed next, without the desktop's in-process lock; a file that
            // remains is never sent, because a turn reads it only for a
            // registered workspace.
            omgcode_app::desktop::remove_workspace_instructions(
                data_root,
                *workspace_id.as_uuid(),
            )
            .map_err(|_| {
                CliError::new(
                    "preferences",
                    "The workspace was unregistered, but its saved instructions file could not be deleted",
                )
            })?;
            print_json(&json!({
                "unregistered_workspace_id": workspace_id,
                "snapshot": snapshot,
            }))
        }
        WorkspaceCommand::Permissions(args) => set_permissions(args, data_root),
        WorkspaceCommand::Check(args) => check(args, data_root),
        WorkspaceCommand::Audit { limit } => audit(limit, data_root),
        WorkspaceCommand::ContainerGrant(args) => container_grant(args, data_root),
        WorkspaceCommand::ProfileAdd(args) => profile_add(args, data_root),
        WorkspaceCommand::ProfileShow { name } => {
            let store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let profile = store
                .profile(&name)
                .ok_or_else(|| control_error(ControlError::UnknownProfile))?;
            print_json(&json!({ "profile": profile }))
        }
        WorkspaceCommand::ProfileRemove { name, revision } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let profile_id = store
                .remove_profile(&name, revision)
                .map_err(control_error)?;
            let output = json!({
                "removed_profile_id": profile_id,
                "snapshot": store.snapshot(),
            });
            let listed = withdrawn_runs(&store);
            drop(store);
            stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
            listed?;
            print_json(&output)
        }
        WorkspaceCommand::PackageProfileAdd(args) => package_profile_add(args, data_root),
        WorkspaceCommand::PackageProfileShow { name } => {
            let store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let profile = store
                .package_profile(&name)
                .ok_or_else(|| control_error(ControlError::UnknownPackageProfile))?;
            print_json(&json!({ "package_profile": profile }))
        }
        WorkspaceCommand::PackageProfileRemove { name, revision } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let output =
                credential_cli::remove_package_profile(&mut store, data_root, &name, revision)?;
            let listed = withdrawn_runs(&store);
            drop(store);
            stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
            listed?;
            print_json(&output)
        }
        WorkspaceCommand::PackageCredentialSet {
            name,
            origin,
            auth,
            revision,
        } => {
            // As for provider credentials: read the secret before the control
            // store is opened, so its lock is never held while input is awaited.
            let secret = credential_cli::read_secret()?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            credential_cli::set_package(
                &mut store, data_root, &name, &origin, &auth, secret, revision,
            )
        }
        WorkspaceCommand::PackageCredentialRemove {
            name,
            origin,
            revision,
        } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            credential_cli::remove_package(&mut store, data_root, &name, &origin, revision)
        }
        WorkspaceCommand::ProviderProfileAdd(args) => provider_profile_add(args, data_root),
        WorkspaceCommand::ProviderProfileShow { name } => {
            let store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let profile = store
                .provider_profile(&name)
                .ok_or_else(|| control_error(ControlError::UnknownProvider))?;
            print_json(&json!({ "provider_profile": profile }))
        }
        WorkspaceCommand::ProviderProfileSelect {
            name,
            default: _,
            revision,
        } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            store
                .select_active_provider(name.as_deref(), revision)
                .map_err(control_error)?;
            print_json(&json!({ "snapshot": store.snapshot() }))
        }
        WorkspaceCommand::ProviderProfileRemove { name, revision } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let output = credential_cli::remove_profile(&mut store, data_root, &name, revision)?;
            let listed = withdrawn_runs(&store);
            drop(store);
            stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
            listed?;
            print_json(&output)
        }
        WorkspaceCommand::ProviderCredentialSet { name, revision } => {
            // Read the secret before the control store is opened, so its
            // lock is never held while standard input is awaited. Input
            // refusals still happen before any write.
            let secret = credential_cli::read_secret()?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            credential_cli::set(&mut store, data_root, &name, secret, revision)
        }
        WorkspaceCommand::ProviderCredentialRemove { name, revision } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            credential_cli::remove(&mut store, data_root, &name, revision)
        }
        WorkspaceCommand::ContainerRevoke { grant } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            // Every run of the workspace that started before this revocation
            // is withdrawn. Runs start only under the lock held here. An
            // unreadable run journal must not keep the grant in place, so the
            // lookup's failure is reported only after the revocation.
            let affected = match store.container_grant_workspace(grant) {
                Some(workspace_id) => run::workspace_runs(&store, workspace_id),
                None => Ok(Vec::new()),
            };
            let revocation = store.revoke_container_grant(grant).map_err(control_error)?;
            let output = json!({
                "revocation": revocation,
                "snapshot": store.snapshot(),
            });
            drop(store);
            match affected {
                Ok(affected) => stop_withdrawn_runs(data_root, &affected)?,
                Err(error) => {
                    // Still stop what the settings now withdraw, then report
                    // that the affected runs could not be confirmed.
                    let stopped = match stop_withdrawn_runs(data_root, &[]) {
                        Ok(()) => String::new(),
                        Err(stop) => format!(" {}", stop.message),
                    };
                    return Err(CliError::new(
                        "run",
                        format!(
                            "The grant was revoked, but the runs it affects could not be found: {error}.{stopped}"
                        ),
                    ));
                }
            }
            print_json(&output)
        }
    }
}

fn container_grant(args: ContainerGrantArgs, data_root: &Path) -> Result<(), CliError> {
    let id = parse_workspace_id(&args.id)?;
    let mut store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let grant = store
        .grant_container_access(id, &args.path, args.access.into(), args.revision)
        .map_err(control_error)?;
    print_json(&json!({
        "grant_id": grant.id(),
        "snapshot": store.snapshot(),
    }))
}

fn profile_add(args: ProfileAddArgs, data_root: &Path) -> Result<(), CliError> {
    let environment = args
        .environment
        .iter()
        .map(|pair| {
            pair.split_once('=')
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .ok_or_else(|| CliError::new("configuration", "Use --env NAME=VALUE"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let profile = store
        .add_profile(
            &ProfileDeclaration {
                name: args.name,
                read_only_roots: args.roots,
                path_entries: args.path_entries,
                environment,
            },
            args.revision,
        )
        .map_err(control_error)?;
    print_json(&json!({
        "profile_id": profile.id(),
        "snapshot": store.snapshot(),
    }))
}

fn package_profile_add(args: PackageProfileAddArgs, data_root: &Path) -> Result<(), CliError> {
    let mut store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let profile = store
        .add_package_profile(
            &PackageProfileDeclaration {
                name: args.name,
                origins: args.registries,
            },
            args.revision,
        )
        .map_err(control_error)?;
    print_json(&json!({
        "package_profile_id": profile.id(),
        "snapshot": store.snapshot(),
    }))
}

fn provider_profile_add(args: ProviderProfileAddArgs, data_root: &Path) -> Result<(), CliError> {
    let mut store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let profile = store
        .add_provider_profile(
            &ProviderProfileDeclaration {
                name: args.name,
                kind: args.kind,
                auth: args.auth,
                endpoint: args.endpoint,
            },
            args.revision,
        )
        .map_err(control_error)?;
    print_json(&json!({
        "provider_profile_id": profile.id(),
        "snapshot": store.snapshot(),
    }))
}

fn set_permissions(args: PermissionsArgs, data_root: &Path) -> Result<(), CliError> {
    if args.read.is_none()
        && args.write.is_none()
        && args.execute.is_none()
        && args.task_network.is_none()
        && args.model_tools.is_none()
        && args.host_services.is_none()
        && args.edit_tool.is_none()
        && args.multi_edit_tool.is_none()
        && args.create_tool.is_none()
        && args.rename_tool.is_none()
        && args.delete_tool.is_none()
        && args.create_directory_tool.is_none()
    {
        return Err(CliError::new(
            "configuration",
            "Set at least one permission flag",
        ));
    }
    let id = parse_workspace_id(&args.id)?;
    let mut store = open(data_root)?;
    let current = store
        .workspace(id)
        .ok_or_else(|| CliError::new("control", "Workspace is not registered"))?
        .permissions();
    let mut write_tools = current.write_tools;
    for (tool, limit) in [
        (WriteTool::Edit, args.edit_tool),
        (WriteTool::MultiEdit, args.multi_edit_tool),
        (WriteTool::Create, args.create_tool),
        (WriteTool::Rename, args.rename_tool),
        (WriteTool::Delete, args.delete_tool),
        (WriteTool::CreateDirectory, args.create_directory_tool),
    ] {
        if let Some(limit) = limit {
            write_tools.set(tool, limit);
        }
    }
    let permissions = PermissionSet {
        read: args.read.unwrap_or(current.read),
        write: args.write.unwrap_or(current.write),
        execute: args.execute.unwrap_or(current.execute),
        task_network: args.task_network.unwrap_or(current.task_network),
        model_tools: args.model_tools.unwrap_or(current.model_tools),
        host_services: args.host_services.unwrap_or(current.host_services),
        write_tools,
    };
    store
        .set_permissions(id, permissions, args.revision)
        .map_err(control_error)?;
    let snapshot = store.snapshot();
    let listed = withdrawn_runs(&store);
    drop(store);
    stop_withdrawn_runs(data_root, listed.as_deref().unwrap_or(&[]))?;
    listed?;
    print_json(&snapshot)
}

/// List the runs a change withdrew while its control lock is still held, so
/// the wait that follows includes them even if a later change restores their
/// access (ADR-0061). Their watchers stop them in any case. The caller
/// reports a failure only after it has waited for what the settings
/// withdraw; a failure never undoes the recorded change.
fn withdrawn_runs(store: &ControlStore) -> Result<Vec<Uuid>, CliError> {
    run::withdrawn_runs(store).map_err(|error| {
        CliError::new(
            "run",
            format!(
                "The settings change was recorded, but the runs it withdrew could not be listed: {error}. Their watchers still stop them; inspect them with `runs audit`."
            ),
        )
    })
}

/// After a change that can withdraw access, wait for the affected runs to
/// stop. The change is already recorded; a run not observed to stop makes
/// the command fail so the revocation is not reported as complete.
fn stop_withdrawn_runs(data_root: &Path, also_withdrawn: &[Uuid]) -> Result<(), CliError> {
    let withdrawn = run::stop_withdrawn_runs(data_root, run::REVOCATION_WAIT, also_withdrawn)
        .map_err(|error| CliError::new("run", error.to_string()))?;
    if withdrawn.unconfirmed.is_empty() {
        return Ok(());
    }
    let runs: Vec<String> = withdrawn.unconfirmed.iter().map(Uuid::to_string).collect();
    Err(CliError::new(
        "run",
        format!(
            "The settings change was recorded, but these runs were not observed to stop: {}. Inspect them with `runs audit`.",
            runs.join(", ")
        ),
    ))
}

fn check(args: CheckArgs, data_root: &Path) -> Result<(), CliError> {
    let id = parse_workspace_id(&args.id)?;
    let target_id = args
        .target_workspace
        .as_deref()
        .map(parse_workspace_id)
        .transpose()?
        .unwrap_or(id);
    let mut store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let binding = store
        .bind_session(args.session, id)
        .map_err(control_error)?;
    let decision = store
        .evaluate(
            &binding,
            &PolicyRequest {
                session_id: args.session,
                workspace_id: target_id,
                operation: args.operation.into(),
            },
        )
        .map_err(control_error)?;
    print_json(&json!({"decision": decision, "snapshot": store.snapshot()}))
}

fn audit(limit: usize, data_root: &Path) -> Result<(), CliError> {
    if !(1..=100).contains(&limit) {
        return Err(CliError::new(
            "configuration",
            "Audit limit must be 1 to 100",
        ));
    }
    let store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let entries = store.audit(limit).map_err(control_error)?;
    print_json(&json!({"audit": entries}))
}

/// How long a settings command waits for another command's control lock.
/// Run watchers hold it briefly while they reread the settings.
const CONTROL_WAIT: Duration = Duration::from_secs(10);
const CONTROL_POLL: Duration = Duration::from_millis(20);

fn open(data_root: &Path) -> Result<ControlStore, CliError> {
    waiting(|| ControlStore::open(data_root))
}

pub(crate) fn open_existing(data_root: &Path) -> Result<Option<ControlStore>, CliError> {
    waiting(|| ControlStore::open_existing(data_root))
}

fn waiting<T>(mut open: impl FnMut() -> Result<T, ControlError>) -> Result<T, CliError> {
    let until = Instant::now() + CONTROL_WAIT;
    loop {
        match open() {
            Err(ControlError::Busy) if Instant::now() < until => thread::sleep(CONTROL_POLL),
            result => return result.map_err(control_error),
        }
    }
}

fn parse_workspace_id(value: &str) -> Result<WorkspaceId, CliError> {
    WorkspaceId::from_str(value)
        .map_err(|_| CliError::new("configuration", "Invalid workspace identifier"))
}

pub(crate) fn control_error(error: ControlError) -> CliError {
    CliError::new("control", error.to_string())
}
