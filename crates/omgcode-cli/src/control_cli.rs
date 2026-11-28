//! Operator commands for workspace registration, configuration-only policy
//! checks, recorded AppContainer grants, and toolchain profiles.
#![forbid(unsafe_code)]

use clap::{Args, Subcommand, ValueEnum};
use omgcode_core::control::{
    AssignmentFailure, ClaudeCodeDeclaration, ContainerGrantAccess, ControlError, ControlStore,
    CredentialId, FolderGrant, PackageProfileDeclaration, ProfileDeclaration,
    ProviderProfileDeclaration, Revocations,
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
    ///
    /// On Windows, registering is also the consent to grant the workspace's
    /// AppContainer read and write access to its folder (ADR-0107), when this
    /// account can change the folder's access. Registering a folder again
    /// keeps its identifier and grants that access if it is missing. A folder
    /// only an administrator can grant, or whose access could not be read, is
    /// listed under `not_granted`, and runs in the workspace are refused.
    Register {
        path: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Remove one workspace's registration and the settings that name it: its
    /// preferences, `web_fetch` host allowlist, assigned toolchain profiles,
    /// package cache selection, skill overrides, checkpoint record, and saved
    /// desktop instructions. The folder and its files are not touched. Every
    /// container grant of the workspace is revoked first (ADR-0107); if one
    /// cannot be revoked, the workspace stays registered. Registering the
    /// folder again assigns a new identifier.
    Unregister {
        workspace: String,
        #[arg(long)]
        revision: u64,
    },
    /// Change one or more policy flags using an expected revision.
    Permissions(PermissionsArgs),
    /// Add one host pattern to a workspace's `web_fetch` allowlist: an exact
    /// name such as docs.example.com, or `*.example.com` for its subdomains
    /// only. Under `--web-fetch allow`, the model tool fetches an allowlisted
    /// host without a per-call approval. Names only: no IP address, no local
    /// name, ASCII (use punycode). At most 32 per workspace.
    WebHostAdd {
        workspace: String,
        pattern: String,
        #[arg(long)]
        revision: u64,
    },
    /// Remove one host pattern from a workspace's `web_fetch` allowlist.
    WebHostRemove {
        workspace: String,
        pattern: String,
        #[arg(long)]
        revision: u64,
    },
    /// List a workspace's `web_fetch` preference and host allowlist.
    WebHostList { workspace: String },
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
    /// on the workspace, which `register` applies; `profile-assign` grants a
    /// profile's folders.
    ContainerGrant(ContainerGrantArgs),
    /// Revoke one recorded container grant. Needs no revision.
    ContainerRevoke {
        /// The grant identifier shown by `container-grant` and `status`.
        grant: Uuid,
    },
    /// Record a toolchain profile for `omgcode run`. A workspace runs it only
    /// after `profile-assign`.
    ProfileAdd(ProfileAddArgs),
    /// Show one recorded toolchain profile.
    ProfileShow { name: String },
    /// Remove a toolchain profile. Its assignments end, and in each workspace
    /// it was assigned to, the grants on its folders that no profile still
    /// assigned there needs are revoked.
    ProfileRemove {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Let a workspace run a recorded toolchain profile (ADR-0107).
    ///
    /// On Windows, running this command is the consent to grant the
    /// workspace's AppContainer read and execute access to each of the
    /// profile's folders it cannot read yet and this account can change. The
    /// grants are applied first, then the assignment is recorded. Folders only
    /// an administrator can grant, or whose access could not be read, are
    /// listed under `not_granted`; a run that needs one is refused.
    ProfileAssign {
        workspace: String,
        /// The toolchain profile name.
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Stop a workspace from running a toolchain profile. Its runs of that
    /// profile are withdrawn, and on Windows the grants on the profile's
    /// folders that no profile still assigned to the workspace needs are
    /// revoked, which withdraws every run of the workspace.
    ProfileUnassign {
        workspace: String,
        /// The toolchain profile name.
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
    /// after the new binding is recorded. The chat client reads it for a
    /// selected lm_studio or hosted `api_key` profile.
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
    /// Sign an `openai` subscription profile in with ChatGPT (ADR-0093).
    ///
    /// Opens OpenAI's sign-in page in the system browser and prints its
    /// address on standard error. It waits up to five minutes for the
    /// browser to return to 127.0.0.1; Ctrl-C cancels. Tokens are kept only
    /// in protected OS storage. A sign-in does not show that plan-backed
    /// requests will be accepted.
    ProviderSignin {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Revoke a ChatGPT sign-in once, record the sign-out, and delete its
    /// stored tokens. A revocation OpenAI does not confirm is reported, and
    /// the local sign-out still happens.
    ProviderSignout {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Record a Claude Code connection that drives the signed-in `claude`
    /// program for `chat --provider-profile NAME` (ADR-0109).
    ///
    /// Sign `claude` in first with `claude auth login`, with
    /// CLAUDE_CONFIG_DIR set to a folder kept for this connection alone,
    /// outside application storage, the development root, and every
    /// workspace. OMGCode records that folder's identity and never reads,
    /// copies, or stores the Claude credential in it. The connection is for
    /// the operator's own use; Anthropic has not approved OMGCode as a
    /// Claude Code client. On Windows grant its folders next with
    /// `claude-code-grant`.
    ClaudeCodeAdd {
        #[arg(long)]
        name: String,
        /// The `claude` program (`claude.exe` on Windows).
        #[arg(long)]
        program: PathBuf,
        /// The folder `claude auth login` signed in to.
        #[arg(long)]
        config_folder: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// Grant a Claude Code connection's own AppContainer the folders its
    /// process needs, Windows only: read and execute on the `claude`
    /// program's folder, on this executable's folder (the MCP relay), and on
    /// the connection's runtime folder in application storage, and read and
    /// write on the sign-in folder.
    ClaudeCodeGrant {
        name: String,
        #[arg(long)]
        revision: u64,
    },
    /// Revoke every grant of a Claude Code connection and delete its
    /// AppContainer profile. A connection is removed with
    /// `provider-profile-remove` only after this.
    ClaudeCodeRevoke { name: String },
}

/// Sign a profile in with ChatGPT. The settings store is not held while the
/// browser step runs.
fn provider_signin(data_root: &Path, name: &str, revision: u64) -> Result<(), CliError> {
    let cancel = omgcode_core::turn::CancelToken::new();
    let interrupts = crate::cancel_on_interrupt(&cancel)?;
    let mut open = |address: &str| {
        eprintln!("Sign in with ChatGPT in your browser. If it does not open, visit:\n{address}");
        if omgcode_app::open_system_browser(address, &cancel).is_err() {
            eprintln!("The system browser could not be opened; open the address yourself.");
        }
        Ok(())
    };
    let result = omgcode_app::sign_in_with_chatgpt(data_root, name, revision, &mut open, &cancel);
    crate::stop_interrupts(interrupts);
    let outcome = result?;
    print_json(&json!({
        "provider_signin": {
            "profile": outcome.profile,
            "account": outcome.account_label,
            "credential_id": outcome.credential_id,
            "revision": outcome.revision,
            "note": "Signed in. This does not show that plan-backed requests will be accepted.",
        },
    }))
}

/// Revoke and forget a ChatGPT sign-in.
fn provider_signout(data_root: &Path, name: &str, revision: u64) -> Result<(), CliError> {
    let cancel = omgcode_core::turn::CancelToken::new();
    let interrupts = crate::cancel_on_interrupt(&cancel)?;
    let result = omgcode_app::sign_out_of_chatgpt(data_root, name, revision, &cancel);
    crate::stop_interrupts(interrupts);
    let outcome = result?;
    let mut output = json!({
        "provider_signout": {
            "profile": name,
            "revoked": outcome.revoked,
            "revision": outcome.revision,
        },
    });
    if !outcome.revoked {
        output["warning"] = json!(
            "OpenAI did not confirm the revocation. The local sign-out is recorded; you can also disconnect OMGCode in your ChatGPT settings."
        );
    }
    if !outcome.orphans.is_empty() {
        output["orphaned_credential_ids"] = json!(
            outcome
                .orphans
                .iter()
                .map(CredentialId::as_str)
                .collect::<Vec<_>>()
        );
    }
    print_json(&output)
}

#[derive(Args)]
pub(crate) struct ProviderProfileAddArgs {
    /// The profile name: ASCII letters, digits, '.', '-', or '_'.
    name: String,
    /// One of: lm_studio, ollama_local, ollama_cloud, anthropic, openai,
    /// google, meta, moonshot, tencent, thinking_machines, minimax,
    /// open_router, z_ai, deepseek, xai, openai_compatible.
    #[arg(long)]
    kind: String,
    /// One of: subscription, api_key, none.
    #[arg(long)]
    auth: String,
    /// The endpoint the profile's adapter uses. lm_studio and ollama_local
    /// accept http://<loopback IP>:<port>/v1; each named hosted kind accepts
    /// exactly its documented endpoint (for example https://ollama.com/v1 or
    /// https://api.deepseek.com); openai_compatible accepts an https:// base
    /// URL. It cannot be changed later; remove the profile and add it again.
    /// A named hosted kind with --auth api_key may omit it, and its documented
    /// endpoint is then recorded, so check --kind; openai_compatible always
    /// needs one.
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
    /// `deny`, `prompt`, or `allow`. No model tool uses it.
    #[arg(long = "task-network", value_parser = parse_access)]
    task_network: Option<Access>,
    /// Let chat offer the model workspace tools at all.
    #[arg(long = "model-tools")]
    model_tools: Option<bool>,
    /// Let a trusted `run` select host-service Unix-socket access when it
    /// also passes `--host-services`. Refused on Windows at run time.
    /// `deny`, `prompt`, or `allow`; never offered to a model.
    #[arg(long = "host-services", value_parser = parse_access)]
    host_services: Option<Access>,
    /// `deny`, `prompt`, or `allow` for the model `web_fetch` tool, one HTTPS
    /// GET of a public page. `prompt` asks before every fetch; `allow` skips
    /// the ask only for hosts on the workspace allowlist (`web-host-add`). It
    /// has no effect unless `--model-tools true`.
    #[arg(long = "web-fetch", value_parser = parse_tool_limit)]
    web_fetch: Option<Access>,
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

/// Parse a state that has no legacy boolean form: `deny`, `prompt`, or
/// `allow` only. Used for per-tool limits and `--web-fetch`.
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
            let registered = store
                .register_workspace_with_access(&path, revision)
                .map_err(control_error)?;
            let grant = registered.grant.map_err(|error| {
                let id = registered.registration.id();
                // A grant whose access-control change failed is recorded
                // `pending`, and core records one grant per folder, so a
                // grant on the folder that is not an active read-write grant
                // refuses a second grant, and registering again, until it is
                // revoked.
                let unusable = store
                    .unusable_folder_grant(id)
                    .map(|status| status.grant().id());
                let recovery = unusable.map_or_else(
                    || "Grant it with `workspace container-grant`.".to_owned(),
                    |grant| {
                        format!(
                            "Grant {grant} on the folder is not an active read-write grant: revoke it with `workspace container-revoke {grant}`, then register the folder again with the current revision from `workspace status`, which grants it."
                        )
                    },
                );
                CliError::new(
                    "control",
                    format!(
                        "The workspace was registered as {id}, but its container could not be granted its folder: {error}. {recovery}"
                    ),
                )
            })?;
            let mut output = json!({
                "registration_id": registered.registration.id(),
                "snapshot": store.snapshot(),
            });
            match grant {
                FolderGrant::Granted(grant) => output["grant_id"] = json!(grant.id()),
                FolderGrant::Skipped(skipped) => {
                    output["not_granted"] = json!({
                        "path": registered.registration.directory_display(),
                        "reason": skipped.as_str(),
                    });
                }
                FolderGrant::NotNeeded => {}
            }
            print_json(&output)
        }
        WorkspaceCommand::Unregister {
            workspace,
            revision,
        } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            // A revocation withdraws every run of the workspace that started
            // before it, and runs start only under the lock held here.
            let before = run::pending_runs(&store);
            let unregistration = store
                .unregister_workspace_with_access(workspace_id, revision)
                .map_err(control_error)?;
            let snapshot = store.snapshot();
            let listed = withdrawn_runs(&store);
            drop(store);
            finish_revocations(
                data_root,
                listed,
                before,
                &unregistration.revocations,
                RevokingChange::Unregistration(unregistration.removal.as_ref()),
            )?;
            let revocations = unregistration.revocations;
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
                "revoked": revocations.revoked,
                "snapshot": snapshot,
            }))
        }
        WorkspaceCommand::Permissions(args) => set_permissions(args, data_root),
        WorkspaceCommand::WebHostAdd {
            workspace,
            pattern,
            revision,
        } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let pattern = store
                .add_web_fetch_host(workspace_id, &pattern, revision)
                .map_err(control_error)?;
            print_json(&json!({
                "pattern": pattern.as_str(),
                "snapshot": store.snapshot(),
            }))
        }
        WorkspaceCommand::WebHostRemove {
            workspace,
            pattern,
            revision,
        } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let pattern = store
                .remove_web_fetch_host(workspace_id, &pattern, revision)
                .map_err(control_error)?;
            print_json(&json!({
                "removed_pattern": pattern.as_str(),
                "snapshot": store.snapshot(),
            }))
        }
        WorkspaceCommand::WebHostList { workspace } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let registration = store
                .workspace(workspace_id)
                .ok_or_else(|| CliError::new("control", "Workspace is not registered"))?;
            print_json(&json!({
                "workspace_id": workspace_id,
                "web_fetch": registration.permissions().web_fetch,
                "web_fetch_hosts": store.web_fetch_hosts(workspace_id),
                "revision": store.revision(),
            }))
        }
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
            let before = run::pending_runs(&store);
            let (profile_id, revocations) = store
                .remove_profile_with_access(&name, revision)
                .map_err(control_error)?;
            let output = json!({
                "removed_profile_id": profile_id,
                "revoked": revocations.revoked,
                "snapshot": store.snapshot(),
            });
            let listed = withdrawn_runs(&store);
            drop(store);
            finish_revocations(
                data_root,
                listed,
                before,
                &revocations,
                RevokingChange::Recorded,
            )?;
            print_json(&output)
        }
        WorkspaceCommand::ProfileAssign {
            workspace,
            name,
            revision,
        } => profile_assign(&workspace, &name, revision, data_root),
        WorkspaceCommand::ProfileUnassign {
            workspace,
            name,
            revision,
        } => {
            let workspace_id = parse_workspace_id(&workspace)?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let before = run::pending_runs(&store);
            let (profile_id, revocations) = store
                .unassign_profile_with_access(workspace_id, &name, revision)
                .map_err(control_error)?;
            let output = json!({
                "unassigned_profile_id": profile_id,
                "revoked": revocations.revoked,
                "snapshot": store.snapshot(),
            });
            let listed = withdrawn_runs(&store);
            drop(store);
            finish_revocations(
                data_root,
                listed,
                before,
                &revocations,
                RevokingChange::Recorded,
            )?;
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
        WorkspaceCommand::ProviderSignin { name, revision } => {
            provider_signin(data_root, &name, revision)
        }
        WorkspaceCommand::ProviderSignout { name, revision } => {
            provider_signout(data_root, &name, revision)
        }
        WorkspaceCommand::ClaudeCodeAdd {
            name,
            program,
            config_folder,
            revision,
        } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let profile = store
                .add_claude_code_profile(
                    &ClaudeCodeDeclaration {
                        name,
                        program,
                        config_folder,
                    },
                    revision,
                )
                .map_err(control_error)?;
            print_json(&json!({
                "provider_profile_id": profile.id(),
                "snapshot": store.snapshot(),
            }))
        }
        WorkspaceCommand::ClaudeCodeGrant { name, revision } => {
            let relay = crate::relay_program()?;
            let relay_folder = relay.parent().ok_or_else(|| {
                CliError::new("configuration", "The OMGCode executable has no folder")
            })?;
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let grants = store
                .grant_claude_code(&name, relay_folder, revision)
                .map_err(control_error)?;
            print_json(&json!({
                "claude_code_grants": grants,
                "snapshot": store.snapshot(),
            }))
        }
        WorkspaceCommand::ClaudeCodeRevoke { name } => {
            let mut store = open_existing(data_root)?
                .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
            let revocation = store
                .revoke_claude_code_grants(&name)
                .map_err(control_error)?;
            print_json(&json!({
                "revocation": revocation,
                "snapshot": store.snapshot(),
            }))
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

/// Assign a profile to a workspace and grant what it needs. The command is
/// the operator's consent, so the plan made here under the control lock is
/// the one applied. A failed assignment whose rollback revoked a grant, or
/// tried to, waits for the workspace's runs that withdrew before it reports.
fn profile_assign(
    workspace: &str,
    name: &str,
    revision: u64,
    data_root: &Path,
) -> Result<(), CliError> {
    let workspace_id = parse_workspace_id(workspace)?;
    let mut store = open_existing(data_root)?
        .ok_or_else(|| CliError::new("control", "Workspace control is not initialized"))?;
    let plan = store
        .plan_profile_access(workspace_id, name)
        .map_err(control_error)?;
    let before = run::pending_runs(&store);
    let assignment = match store.assign_profile_with_access(workspace_id, name, &plan, revision) {
        Ok(assignment) => assignment,
        Err(failure) if failure.released.workspaces.is_empty() => {
            return Err(assignment_failed(failure));
        }
        Err(failure) => {
            // Revoking a grant withdraws every run of its workspace that
            // started before: wait for them with the store closed.
            let listed = withdrawn_runs(&store);
            drop(store);
            let problems = wait_for_revoked_runs(data_root, listed, before, &failure.released);
            let mut error = assignment_failed(failure);
            for problem in problems {
                if !error.message.ends_with('.') {
                    error.message.push('.');
                }
                error.message.push(' ');
                error.message.push_str(&problem);
            }
            return Err(error);
        }
    };
    let not_granted: Vec<_> = assignment
        .plan
        .needs_administrator()
        .iter()
        .map(|folder| (folder, "needs_administrator"))
        .chain(
            assignment
                .plan
                .unknown()
                .iter()
                .map(|folder| (folder, "access_unknown")),
        )
        .map(|(folder, reason)| json!({ "path": folder.display(), "reason": reason }))
        .collect();
    print_json(&json!({
        "assigned_profile_id": assignment.profile_ids.first(),
        "granted": assignment.granted.iter().map(|grant| json!({
            "grant_id": grant.id(),
            "path": grant.directory_display(),
        })).collect::<Vec<_>>(),
        "not_granted": not_granted,
        "snapshot": store.snapshot(),
    }))
}

/// The error of a failed assignment, naming each grant it made that could
/// not be revoked afterwards, with the command that retries it.
fn assignment_failed(failure: AssignmentFailure) -> CliError {
    let mut error = control_error(failure.error);
    let failures = &failure.released.failures;
    if failures.is_empty() && failure.kept.is_empty() {
        return error;
    }
    if !error.message.ends_with('.') {
        error.message.push('.');
    }
    error.message.push_str(if all_revoked(failures) && failure.kept.is_empty() {
        " The profile was not assigned, and every container grant the assignment made was revoked."
    } else {
        " The profile was not assigned, but not every container grant the assignment made was revoked."
    });
    error.message.push_str(&unrevoked_grants(failures));
    for grant in &failure.kept {
        error.message.push_str(&format!(
            " Grant {grant} is kept, because a profile already assigned to the workspace needs it."
        ));
    }
    error
}

/// Whether every failure is a grant revoked whose AppContainer profile alone
/// could not be deleted, so no grant keeps its access.
fn all_revoked(failures: &[(Uuid, ControlError)]) -> bool {
    failures
        .iter()
        .all(|(_, error)| matches!(error, ControlError::ProfileRemoval(_)))
}

/// One sentence per failed revocation: a grant not revoked, with the command
/// that retries it, or a grant revoked whose profile could not be deleted.
fn unrevoked_grants(failures: &[(Uuid, ControlError)]) -> String {
    let mut text = String::new();
    for (grant, error) in failures {
        if matches!(error, ControlError::ProfileRemoval(_)) {
            // The grant is revoked; only the container's profile, which
            // holds no grant now, could not be deleted.
            text.push_str(&format!(" Grant {grant}: {error}."));
        } else {
            text.push_str(&format!(
                " Grant {grant} could not be revoked: {error}; retry with `workspace container-revoke {grant}`."
            ));
        }
    }
    text
}

/// The change [`finish_revocations`] reports on.
#[derive(Clone, Copy)]
enum RevokingChange<'a> {
    /// A recorded change, which stands whatever its revocations did.
    Recorded,
    /// An unregistration, recorded only after every grant was revoked, with
    /// the error that kept it from being recorded after that, if any.
    Unregistration(Option<&'a ControlError>),
}

/// After a recorded change that may have revoked grants, wait for the runs it
/// withdrew, then report what did not complete. `listed` holds the runs the
/// settings withdrew; `before` every pending run, listed by
/// `run::pending_runs` before the change. Any revocation, even one that failed
/// after it was requested, withdraws every run of its workspace, so the runs
/// of those workspaces are waited for too. Every grant that could not be
/// revoked is named; it stays recorded, and `workspace container-revoke`
/// retries it.
fn finish_revocations(
    data_root: &Path,
    listed: Result<Vec<Uuid>, CliError>,
    before: Result<Vec<(WorkspaceId, Uuid)>, run::RunError>,
    revocations: &Revocations,
    change: RevokingChange<'_>,
) -> Result<(), CliError> {
    let mut problems = wait_for_revoked_runs(data_root, listed, before, revocations);
    let mut kind = "run";
    if !revocations.failures.is_empty() {
        kind = "control";
        let revoked = all_revoked(&revocations.failures);
        let mut message = match (change, revoked) {
            (RevokingChange::Recorded, false) => {
                "The change was recorded, but not every container grant it releases was revoked."
            }
            (RevokingChange::Recorded, true) => {
                "The change was recorded, and every container grant it releases was revoked."
            }
            (RevokingChange::Unregistration(_), false) => {
                "The workspace stays registered, because not every container grant of it was revoked."
            }
            (RevokingChange::Unregistration(_), true) => {
                "The workspace stays registered: every container grant of it was revoked, but its removal was not attempted."
            }
        }
        .to_owned();
        message.push_str(&unrevoked_grants(&revocations.failures));
        if matches!(change, RevokingChange::Unregistration(_)) {
            message.push_str(if revoked {
                " Unregister it again."
            } else {
                " Then unregister it again."
            });
        }
        problems.insert(0, message);
    } else if let RevokingChange::Unregistration(Some(error)) = change {
        kind = "control";
        problems.insert(
            0,
            format!(
                "Every container grant of the workspace was revoked, but its removal could not be recorded: {error}. The workspace stays registered; unregister it again."
            ),
        );
    } else if !problems.is_empty() {
        // The JSON naming what was revoked is not printed after a failure.
        problems.insert(0, completed_change(change, revocations));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(CliError::new(kind, problems.join(" ")))
    }
}

/// What a change whose every revocation succeeded did, for an error raised
/// after it: the change and each grant it revoked.
fn completed_change(change: RevokingChange<'_>, revocations: &Revocations) -> String {
    let (done, grants) = match change {
        RevokingChange::Recorded => (
            "The change was recorded",
            "every container grant it releases was revoked",
        ),
        RevokingChange::Unregistration(_) => (
            "The workspace was unregistered",
            "every container grant of it was revoked",
        ),
    };
    let revoked: Vec<String> = revocations
        .revoked
        .iter()
        .map(|revocation| revocation.grant_id.to_string())
        .collect();
    match revoked.as_slice() {
        [] => format!("{done}."),
        [grant] => format!("{done}, and {grants} (grant {grant})."),
        _ => format!("{done}, and {grants} (grants {}).", revoked.join(", ")),
    }
}

/// Wait for the runs the settings withdrew, `listed`, and the pending runs in
/// `before` of each workspace whose grant was revoked or was tried, then
/// return each problem met, in order. Any revocation, even one that failed
/// after it was requested, withdraws every run of its workspace. The store
/// must be closed.
fn wait_for_revoked_runs(
    data_root: &Path,
    listed: Result<Vec<Uuid>, CliError>,
    before: Result<Vec<(WorkspaceId, Uuid)>, run::RunError>,
    revocations: &Revocations,
) -> Vec<String> {
    let mut wait = listed.as_deref().unwrap_or(&[]).to_vec();
    let mut problems = Vec::new();
    if !revocations.workspaces.is_empty() {
        match before {
            Ok(runs) => wait.extend(revocations.withdrawn_runs(&runs)),
            Err(error) => problems.push(format!(
                "The runs the revocations affect could not be found: {error}."
            )),
        }
    }
    if let Err(error) = stop_withdrawn_runs(data_root, &wait) {
        problems.push(error.message);
    }
    if let Err(error) = listed {
        problems.push(error.message);
    }
    problems
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
        && args.web_fetch.is_none()
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
        web_fetch: args.web_fetch.unwrap_or(current.web_fetch),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn failure(released: Vec<(Uuid, ControlError)>) -> AssignmentFailure {
        AssignmentFailure {
            error: ControlError::LimitReached,
            released: Revocations {
                failures: released,
                ..Revocations::default()
            },
            kept: Vec::new(),
        }
    }

    /// A grant a failed assignment kept for a profile already assigned to
    /// the workspace is named, and the message does not say every grant was
    /// revoked.
    #[test]
    fn a_failed_assignment_names_each_grant_it_kept() {
        let kept = Uuid::new_v4();
        let error = assignment_failed(AssignmentFailure {
            kept: vec![kept],
            ..failure(Vec::new())
        });
        assert_eq!(
            error.message,
            format!(
                "control journal capacity reached. The profile was not assigned, but not every container grant the assignment made was revoked. Grant {kept} is kept, because a profile already assigned to the workspace needs it."
            )
        );
    }

    /// A failed assignment whose grants were all revoked reports only its
    /// cause.
    #[test]
    fn a_failed_assignment_with_nothing_left_reports_its_cause() {
        let error = assignment_failed(failure(Vec::new()));
        assert_eq!(error.code, "control");
        assert_eq!(error.message, "control journal capacity reached");
    }

    /// A grant a failed assignment could not revoke is named with the
    /// command that retries it; one whose profile alone could not be deleted
    /// is named as revoked.
    #[test]
    fn a_failed_assignment_names_each_grant_it_left() {
        let (left, removed) = (Uuid::new_v4(), Uuid::new_v4());
        let error = assignment_failed(failure(vec![
            (left, ControlError::Container(io::Error::other("denied"))),
            (
                removed,
                ControlError::ProfileRemoval(io::Error::other("busy")),
            ),
        ]));
        assert!(
            error.message.starts_with(
                "control journal capacity reached. The profile was not assigned, but not every container grant the assignment made was revoked."
            ),
            "{}",
            error.message
        );
        assert!(
            error.message.contains(&format!(
                "Grant {left} could not be revoked: container access change failed: denied; retry with `workspace container-revoke {left}`."
            )),
            "{}",
            error.message
        );
        assert!(
            error.message.contains(&format!(
                "Grant {removed}: the grant was revoked, but the container profile could not be deleted: busy."
            )),
            "{}",
            error.message
        );
    }

    /// When every failure is a profile that could not be deleted, the
    /// message says every grant was revoked.
    #[test]
    fn a_failed_profile_deletion_alone_says_every_grant_was_revoked() {
        let failures = vec![(
            Uuid::new_v4(),
            ControlError::ProfileRemoval(io::Error::other("busy")),
        )];
        assert!(all_revoked(&failures));
        let error = assignment_failed(failure(failures));
        assert!(
            error
                .message
                .contains("every container grant the assignment made was revoked."),
            "{}",
            error.message
        );
        assert!(
            !error.message.contains("container-revoke"),
            "{}",
            error.message
        );
        assert!(!all_revoked(&[(
            Uuid::new_v4(),
            ControlError::Container(io::Error::other("denied")),
        )]));
    }

    fn revoked(grants: &[Uuid]) -> Revocations {
        Revocations {
            revoked: grants
                .iter()
                .map(|&grant_id| omgcode_core::control::ContainerRevocation {
                    grant_id,
                    profile_deleted: false,
                })
                .collect(),
            ..Revocations::default()
        }
    }

    /// An error raised after every revocation succeeded, such as an
    /// unreadable run journal, names the change and each grant it revoked,
    /// because the JSON that lists them is not printed.
    #[test]
    fn a_completed_change_names_each_grant_it_revoked() {
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            completed_change(RevokingChange::Recorded, &revoked(&[first])),
            format!(
                "The change was recorded, and every container grant it releases was revoked (grant {first})."
            )
        );
        assert_eq!(
            completed_change(
                RevokingChange::Unregistration(None),
                &revoked(&[first, second])
            ),
            format!(
                "The workspace was unregistered, and every container grant of it was revoked (grants {first}, {second})."
            )
        );
        assert_eq!(
            completed_change(RevokingChange::Recorded, &revoked(&[])),
            "The change was recorded."
        );
    }
}
