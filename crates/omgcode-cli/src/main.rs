//! Developer CLI for the first OMGCode core prototype.
#![forbid(unsafe_code)]
#![recursion_limit = "256"]

mod actions_cli;
mod approvals_cli;
mod control_cli;
mod credential_cli;
mod file_cli;
mod packages_cli;
mod run_cli;
mod skills_cli;
mod stream_cli;

use clap::{Args, Parser, Subcommand};
use omgcode_app::{
    AppError, ChatClients, ChatHooks, ChatSettings, ChatStreamEvent, ChatTurnInput,
    ConnectionSource, LmToken, LocalConnection, MODEL_TOOLS_SUPPORTED, compact_journal,
    create_session, data_root, read_lm_token, redact_known_secret, resolve_local_connection,
    run_chat_turn_with_clients,
};
use omgcode_core::context::{ContextBudget, DEFAULT_COMPACT_THRESHOLD};
use omgcode_core::prompt::{self, BASE_PROMPT, BASE_PROMPT_VERSION};
use omgcode_core::provider::{ListedModel, LocalModelClient, LocalModelConfig, list_local_models};
use omgcode_core::run::{InterruptWatch, watch_interrupt};
use omgcode_core::session::{NativePath, SessionJournal};
use omgcode_core::skills::SkillName;
use omgcode_core::turn::CancelToken;
use serde::Serialize;
use serde_json::json;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Whether trusted runs and toolchain profiles exist on this platform.
const RUN_SUPPORTED: bool = cfg!(any(target_os = "linux", windows));

#[derive(Parser)]
#[command(name = "omgcode", version, about = "OMGCode local core prototype")]
struct Cli {
    /// Trusted application storage override. Keep this outside project folders.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect the compiled, read-only base prompt and its digest.
    Prompt,
    /// Report the capabilities actually available in this prototype.
    Capabilities,
    /// Create, inspect, recover, archive, or restore a JSONL session.
    Session {
        #[command(subcommand)]
        action: SessionCommand,
    },
    /// Configure workspaces and inspect configuration-only permission policy.
    Workspace {
        #[command(subcommand)]
        action: control_cli::WorkspaceCommand,
    },
    /// Perform bounded, native file operations through trusted operator input.
    File {
        #[command(subcommand)]
        action: file_cli::FileCommand,
    },
    /// Send one turn to an explicitly selected local LM Studio or Ollama model.
    Chat(ChatArgs),
    /// List the models the local LM Studio or Ollama server reports, so one
    /// can be passed to `chat --model` (ADR-0066). Reads no session.
    Models(ModelsArgs),
    /// Run one program in a workspace under native confinement, using a
    /// recorded toolchain profile. Needs the workspace `execute` preference.
    Run(run_cli::RunArgs),
    /// Inspect, acknowledge, or archive run records.
    Runs {
        #[command(subcommand)]
        action: run_cli::RunsCommand,
    },
    /// Fetch a package from a declared registry into a private cache.
    Packages {
        #[command(subcommand)]
        action: packages_cli::PackagesCommand,
    },
    /// Import, register, enable, inspect, and remove operator-selected skills.
    Skills {
        #[command(subcommand)]
        action: skills_cli::SkillsCommand,
    },
    /// Inspect, decide, or consume trusted per-call approvals.
    Approvals {
        #[command(subcommand)]
        action: approvals_cli::ApprovalsCommand,
    },
    /// Inspect the record of every model tool call (ADR-0063).
    Actions {
        #[command(subcommand)]
        action: actions_cli::ActionsCommand,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    New {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long, default_value = "New session")]
        title: String,
    },
    /// Read a selected journal without executing its recorded content.
    Inspect {
        #[command(flatten)]
        locator: JournalArgs,
    },
    Archive {
        #[command(flatten)]
        locator: JournalArgs,
    },
    Restore {
        #[command(flatten)]
        locator: JournalArgs,
    },
    /// Mark an abandoned turn failed without retrying its model request.
    Interrupt {
        #[command(flatten)]
        locator: JournalArgs,
    },
    /// Summarize older messages with the selected local model.
    Compact {
        #[command(flatten)]
        locator: JournalArgs,
        /// The loopback endpoint. Not allowed with --provider-profile, whose
        /// recorded endpoint is the only one a profile uses.
        #[arg(
            long,
            default_value = "http://127.0.0.1:1234/v1",
            conflicts_with = "provider_profile"
        )]
        endpoint: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        context_tokens: u64,
        #[arg(long, default_value_t = 2048)]
        summary_tokens: u32,
        /// Use this recorded `lm_studio`, `ollama_local`, or `ollama_cloud` provider profile's
        /// endpoint, and its stored credential, if any, instead of
        /// OMGCODE_LM_TOKEN.
        #[arg(long, value_parser = non_empty_name)]
        provider_profile: Option<String>,
    },
}

#[derive(Args)]
struct JournalArgs {
    #[arg(required_unless_present = "journal_native")]
    journal: Option<PathBuf>,
    /// Lossless journal_native JSON returned by an earlier command.
    #[arg(long, conflicts_with = "journal")]
    journal_native: Option<String>,
}

#[derive(Args)]
struct ChatArgs {
    #[arg(long, required_unless_present_any = ["journal", "journal_native"], conflicts_with_all = ["journal", "journal_native"])]
    workspace: Option<PathBuf>,
    /// Resume this explicit journal. Historical grants never authorize tools.
    #[arg(long)]
    journal: Option<PathBuf>,
    /// Resume using the lossless journal_native JSON from an earlier result.
    #[arg(long, conflicts_with = "journal")]
    journal_native: Option<String>,
    #[arg(long, default_value = "Local model chat")]
    title: String,
    /// The loopback endpoint. Not allowed with --provider-profile, whose
    /// recorded endpoint is the only one a profile uses.
    #[arg(
        long,
        default_value = "http://127.0.0.1:1234/v1",
        conflicts_with = "provider_profile"
    )]
    endpoint: String,
    #[arg(long)]
    model: String,
    /// Validated context capacity for the selected loaded model.
    #[arg(long)]
    context_tokens: u64,
    #[arg(long, default_value_t = 1024)]
    max_output_tokens: u32,
    /// User-level preferences. This cannot replace the compiled base prompt.
    #[arg(long, default_value = "")]
    instructions: String,
    /// Compact before a turn when the conservative estimate reaches this value.
    #[arg(long, default_value_t = DEFAULT_COMPACT_THRESHOLD)]
    compact_threshold: u64,
    /// Enable compaction at the effective trigger before a turn.
    #[arg(long)]
    auto_compact: bool,
    /// Use this recorded `lm_studio`, `ollama_local`, or `ollama_cloud` provider profile's
    /// endpoint, and its stored credential, if any, instead of
    /// OMGCODE_LM_TOKEN. The variable is then not read.
    #[arg(long, value_parser = non_empty_name)]
    provider_profile: Option<String>,
    /// Stream the model's text to standard error as it arrives. The text is
    /// provisional and escaped for display; standard output still carries the
    /// one JSON result, and the journal records only the validated reply.
    #[arg(long)]
    stream: bool,
    /// Load this registered, enabled skill for this turn only (repeatable, at
    /// most four, none repeated). Skill text is untrusted, grants no
    /// permission, and is not recorded in the session history.
    #[arg(long = "skill", value_name = "NAME", value_parser = parse_skill_name)]
    skills: Vec<SkillName>,
    /// Current user request. Quote it as one shell argument.
    message: String,
}

#[derive(Args)]
struct ModelsArgs {
    /// The loopback endpoint. Not allowed with --provider-profile, whose
    /// recorded endpoint is the only one a profile uses.
    #[arg(
        long,
        default_value = "http://127.0.0.1:1234/v1",
        conflicts_with = "provider_profile"
    )]
    endpoint: String,
    /// Use this recorded `lm_studio`, `ollama_local`, or `ollama_cloud` provider profile's
    /// endpoint, and its stored credential, if any, instead of
    /// OMGCODE_LM_TOKEN, exactly as `chat` does.
    #[arg(long, value_parser = non_empty_name)]
    provider_profile: Option<String>,
}

/// The `models` result. Ids are untrusted provider text; JSON escapes them.
#[derive(Serialize)]
struct ModelsOutput {
    endpoint: String,
    models: Vec<ListedModel>,
}

/// List the models of the connection `chat` would use with the same options.
fn list_models(data_dir: Option<&Path>, args: ModelsArgs) -> Result<(), CliError> {
    let LocalConnection { endpoint, token } =
        local_connection(data_dir, args.endpoint, args.provider_profile.as_deref())?;
    let models = list_local_models(&endpoint, token.as_ref().map(LmToken::expose))
        .map_err(AppError::provider)?;
    print_json(&ModelsOutput { endpoint, models })
}

/// Parse a skill name, so an invalid one is a usage error before anything runs.
fn parse_skill_name(value: &str) -> Result<SkillName, String> {
    SkillName::new(value).map_err(|error| format!("invalid skill name: {}", error.code()))
}

/// Reject an empty profile name. Core validates it against recorded profiles.
fn non_empty_name(value: &str) -> Result<String, String> {
    if value.is_empty() {
        Err("the provider profile name must not be empty".to_owned())
    } else {
        Ok(value.to_owned())
    }
}

#[derive(Serialize)]
struct CliError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<Box<JournalLocator>>,
}

#[derive(Clone, Serialize)]
struct JournalLocator {
    session_id: uuid::Uuid,
    journal: Option<String>,
    journal_display: String,
    journal_native: NativePath,
}

#[derive(Serialize)]
struct JournalOutput<T> {
    #[serde(flatten)]
    locator: JournalLocator,
    #[serde(flatten)]
    detail: T,
}

impl From<AppError> for CliError {
    fn from(error: AppError) -> Self {
        Self::new(error.code(), error.into_message())
    }
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            session: None,
        }
    }

    /// Preserve the existing session locator on all post-open chat failures.
    fn with_session(mut self, session: JournalLocator) -> Self {
        self.session = Some(Box::new(session));
        self
    }
}

/// Emit controlled JSON errors; never render untrusted text as terminal escapes.
fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // A closed error stream cannot be repaired by recursively printing.
            if serde_json::to_writer(io::stderr().lock(), &json!({"error": error})).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

/// Dispatch trusted operator commands without providing any model execution route.
/// `run` is an operator command; no model tool reaches it.
fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Prompt => print_json(&json!({
            "version": BASE_PROMPT_VERSION,
            "sha256": prompt::base_prompt_sha256(),
            "text": BASE_PROMPT,
        })),
        Command::Capabilities => print_json(&json!({
            "version": env!("CARGO_PKG_VERSION"),
            "prototype": true,
            "sessions_jsonl": true,
            "archive_restore": true,
            "compiled_prompt": true,
            "local_model_chat": true,
            "local_model_listing": true,
            "workspace_registry": true,
            "permission_policy": true,
            "permission_audit": true,
            "policy_decision": "configuration_only",
            "workspace_file_broker_implemented": cfg!(any(target_os = "linux", target_os = "windows")),
            "workspace_file_broker": if cfg!(any(target_os = "linux", target_os = "windows")) {
                serde_json::Value::Null
            } else {
                serde_json::Value::Bool(false)
            },
            "exclusive_file_creation": if cfg!(any(target_os = "linux", target_os = "windows")) {
                serde_json::Value::Null
            } else {
                serde_json::Value::Bool(false)
            },
            "existing_file_replacement": cfg!(any(target_os = "linux", target_os = "windows")),
            "checkpoint_restore": cfg!(any(target_os = "linux", target_os = "windows")),
            "checkpoint_deletion": cfg!(any(target_os = "linux", target_os = "windows")),
            "edit_previews": cfg!(any(target_os = "linux", target_os = "windows")),
            "model_read_list_tools": MODEL_TOOLS_SUPPORTED,
            "model_command_tool": "approval_required",
            "model_write_tools": "approval_required",
            "workspace_container_grants": omgcode_core::control::CONTAINER_GRANTS_SUPPORTED,
            "workspace_file_tools": false,
            "toolchain_profiles": RUN_SUPPORTED,
            "package_network_profiles": true,
            "package_network_enforcement": false,
            "package_fetch": true,
            "provider_profiles": true,
            "provider_profile_endpoints": true,
            "provider_adapters": false,
            "stored_credential_adapters": ["lm_studio", "ollama_cloud"],
            "streaming_adapters": ["lm_studio", "ollama_local", "ollama_cloud"],
            "cancellable_adapters": ["lm_studio", "ollama_local", "ollama_cloud"],
            "provider_credentials": omgcode_core::control::CredentialBackend::native().is_some(),
            "package_registry_credentials": omgcode_core::control::CredentialBackend::native().is_some(),
            "credential_store": credential_cli::capability(),
            "approvals": true,
            "process_execution": if RUN_SUPPORTED {
                serde_json::Value::from("trusted_cli_only")
            } else {
                serde_json::Value::Bool(false)
            },
            "skills": skills_cli::capability(),
            "mcp": false,
            "memories": false,
            "sandbox_backend": if cfg!(target_os = "linux") {
                "landlock"
            } else if cfg!(windows) {
                "appcontainer"
            } else {
                "unavailable"
            },
            "tool_policy": if cfg!(any(target_os = "linux", target_os = "windows")) {
                "read_list_when_workspace_model_tools_enabled_edit_multi_edit_create_rename_delete_and_command_require_approval"
            } else {
                "reject all model-requested tools"
            },
        })),
        Command::Session { action } => match action {
            SessionCommand::New { workspace, title } => {
                let token = read_lm_token()?;
                let title = redact_known_secret(&title, token.as_deref());
                let data_root = data_root(cli.data_dir)?;
                let journal = create_session(&data_root, &workspace, &title)?;
                print_session(&journal)
            }
            SessionCommand::Inspect { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                print_inspection(&SessionJournal::open(&path).map_err(session_error)?)
            }
            SessionCommand::Archive { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                let mut journal = SessionJournal::open(&path).map_err(session_error)?;
                journal.archive().map_err(session_error)?;
                print_session(&journal)
            }
            SessionCommand::Restore { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                let mut journal = SessionJournal::open(&path).map_err(session_error)?;
                journal.restore().map_err(session_error)?;
                print_session(&journal)
            }
            SessionCommand::Interrupt { locator } => {
                let path = resolve_journal(locator.journal, locator.journal_native)?;
                let mut journal = SessionJournal::open(&path).map_err(session_error)?;
                if let Some(request_id) = journal.snapshot().active_turn {
                    journal
                        .fail_turn(request_id, "operator_interrupted")
                        .map_err(session_error)?;
                }
                print_session(&journal)
            }
            SessionCommand::Compact {
                locator,
                endpoint,
                model,
                context_tokens,
                summary_tokens,
                provider_profile,
            } => compact_session(
                cli.data_dir,
                locator,
                endpoint,
                model,
                context_tokens,
                summary_tokens,
                provider_profile,
            ),
        },
        Command::Workspace { action } => {
            let data_root = data_root(cli.data_dir)?;
            control_cli::run(action, &data_root)
        }
        Command::File { action } => file_cli::run(action, &data_root(cli.data_dir)?),
        Command::Chat(args) => chat(cli.data_dir, args),
        Command::Models(args) => list_models(cli.data_dir.as_deref(), args),
        Command::Run(args) => run_cli::run(args, &data_root(cli.data_dir)?),
        Command::Runs { action } => run_cli::runs(action, &data_root(cli.data_dir)?),
        Command::Packages { action } => packages_cli::run(action, &data_root(cli.data_dir)?),
        Command::Skills { action } => skills_cli::run(action, &data_root(cli.data_dir)?),
        Command::Approvals { action } => approvals_cli::run(action, &data_root(cli.data_dir)?),
        Command::Actions { action } => actions_cli::run(action, &data_root(cli.data_dir)?),
    }
}

/// Validate the provider before opening storage, then attach recovery information.
fn chat(data_dir: Option<PathBuf>, mut args: ChatArgs) -> Result<(), CliError> {
    // A malformed skill selection stops the command before any storage use.
    omgcode_app::validate_skill_selection(&args.skills)?;
    // The connection comes first, so an unreadable selected credential or a
    // profile with no endpoint stops the command before the endpoint is
    // checked and before any journal exists. With a profile, its recorded
    // endpoint is the only endpoint used.
    let LocalConnection { endpoint, token } = local_connection(
        data_dir.as_deref(),
        args.endpoint,
        args.provider_profile.as_deref(),
    )?;
    let token_text = token.as_ref().map(LmToken::expose);
    let settings = ChatSettings {
        endpoint,
        model: args.model,
        context_tokens: args.context_tokens,
        max_output_tokens: args.max_output_tokens,
        instructions: args.instructions,
        compact_threshold: args.compact_threshold,
        auto_compact: args.auto_compact,
    };
    let clients = ChatClients::new(&settings, token_text)?;
    let title = redact_known_secret(&args.title, token_text);
    let path = if args.journal.is_some() || args.journal_native.is_some() {
        Some(resolve_journal(
            args.journal.take(),
            args.journal_native.take(),
        )?)
    } else {
        None
    };
    let data_root = data_root(data_dir)?;
    let mut journal = match path {
        Some(path) => SessionJournal::open(&path).map_err(session_error)?,
        None => {
            let workspace = args.workspace.as_deref().ok_or_else(|| {
                CliError::new("configuration", "A workspace or journal is required")
            })?;
            create_session(&data_root, workspace, &title)?
        }
    };
    let locator = journal_locator(&journal)?;
    let input = ChatTurnInput {
        message: &args.message,
        settings: &settings,
        token: token_text,
        skills: &args.skills,
    };
    // From here an interrupt cancels the turn instead of ending the process,
    // so the journal records the cancellation and keeps no open turn.
    let cancel = CancelToken::new();
    let interrupts =
        cancel_on_interrupt(&cancel).map_err(|error| error.with_session(locator.clone()))?;
    // Provisional text goes to standard error through one printer, which locks
    // standard error only for each write, never across a model request.
    let mut printer = stream_cli::StreamPrinter::new(std::io::stderr());
    let turn = {
        let mut on_stream = |event: ChatStreamEvent<'_>| printer.handle(event);
        let mut hooks = ChatHooks {
            approval_wait: None,
            cancel: Some(cancel),
            ..ChatHooks::default()
        };
        if args.stream {
            hooks.on_stream = Some(&mut on_stream);
        }
        run_chat_turn_with_clients(&mut journal, &data_root, &input, &clients, &mut hooks)
    };
    stop_interrupts(interrupts);
    // End the last streamed line, also after a failed turn.
    printer.finish();
    // Every post-open failure, including output failures, carries the locator.
    let result = turn.map_err(CliError::from).and_then(|output| {
        print_json(&JournalOutput {
            locator: journal_locator(&journal)?,
            detail: output,
        })
    });
    result.map_err(|error| error.with_session(locator))
}

fn compact_session(
    data_dir: Option<PathBuf>,
    locator: JournalArgs,
    endpoint: String,
    model: String,
    context_tokens: u64,
    summary_tokens: u32,
    provider_profile: Option<String>,
) -> Result<(), CliError> {
    if summary_tokens == 0 || u64::from(summary_tokens) >= context_tokens {
        return Err(CliError::new(
            "configuration",
            "summary-tokens must be positive and below context-tokens",
        ));
    }
    let LocalConnection { endpoint, token } =
        local_connection(data_dir.as_deref(), endpoint, provider_profile.as_deref())?;
    let token_text = token.as_ref().map(LmToken::expose);
    let client = LocalModelClient::new(
        LocalModelConfig {
            base_url: endpoint,
            model,
            context_tokens,
            max_output_tokens: summary_tokens,
        },
        token_text.map(str::to_owned),
    )
    .map_err(provider_error)?;
    let data_root = data_root(data_dir)?;
    let path = resolve_journal(locator.journal, locator.journal_native)?;
    let mut journal = SessionJournal::open(&path).map_err(session_error)?;
    // An interrupt stops the summarizer request in flight; a cancelled
    // compaction records nothing.
    let cancel = CancelToken::new();
    let interrupts = cancel_on_interrupt(&cancel)?;
    // Live settings decide whether saved tool results may reach the summarizer.
    let outcome = compact_journal(
        &mut journal,
        &data_root,
        &client,
        token_text,
        ContextBudget {
            threshold: u64::MAX,
            context_tokens,
            max_output_tokens: summary_tokens,
        },
        &cancel,
    );
    stop_interrupts(interrupts);
    let outcome = outcome?;
    let locator = journal_locator(&journal)?;
    print_json(&JournalOutput {
        locator,
        detail: json!({
            "compaction": outcome.record,
            "retained_messages": outcome.retained,
            "summarizer_calls": outcome.calls,
        }),
    })
}

/// Cancel `cancel` when the operator interrupts the command (ADR-0051).
///
/// Ctrl-C and Ctrl-Break on Windows, and `SIGINT` and `SIGTERM` on Linux, are
/// absorbed from here to the end of the process and set the token instead, so
/// a model or compaction request stops within about 25 ms and the outcome is
/// recorded. A command that cannot install the handler stops before any model
/// request with the code `interrupt`.
fn cancel_on_interrupt(cancel: &CancelToken) -> Result<InterruptWatch, CliError> {
    let token = cancel.clone();
    watch_interrupt(move || token.cancel()).map_err(|error| {
        CliError::new(
            "interrupt",
            format!("Interrupt handling could not be installed: {error}"),
        )
    })
}

/// Stop the interrupt watcher once the turn or compaction has returned.
fn stop_interrupts(interrupts: InterruptWatch) {
    // The watcher is joined on every path. Its only failure is a panic in
    // the watcher thread, which cannot change the outcome that was already
    // recorded, so it is not reported over the command's result.
    let _ = interrupts.stop();
}

/// Obtain the endpoint and server token for `chat` or `session compact`.
///
/// Without a profile this is exactly what it always was: the command-line
/// endpoint and the environment token, with no data root computed. With one,
/// the endpoint and the token come only from that profile (the clap
/// definitions refuse `--endpoint` beside it) and `OMGCODE_LM_TOKEN` is not
/// read.
fn local_connection(
    data_dir: Option<&Path>,
    endpoint: String,
    provider_profile: Option<&str>,
) -> Result<LocalConnection, CliError> {
    match provider_profile {
        None => Ok(resolve_local_connection(
            Path::new(""),
            &ConnectionSource::Environment { endpoint },
        )?),
        Some(name) => {
            let root = data_root(data_dir.map(Path::to_path_buf))?;
            Ok(resolve_local_connection(
                &root,
                &ConnectionSource::Profile(name.to_owned()),
            )?)
        }
    }
}

fn print_session(journal: &SessionJournal) -> Result<(), CliError> {
    print_json(&JournalOutput {
        locator: journal_locator(journal)?,
        detail: json!({"session":journal.snapshot()}),
    })
}

/// Print a session with its recorded tool requests, so the operator can review
/// a call's exact arguments, such as an edit's `old_text` and `new_text`,
/// before deciding its approval. The arguments remain untrusted model data.
fn print_inspection(journal: &SessionJournal) -> Result<(), CliError> {
    print_json(&JournalOutput {
        locator: journal_locator(journal)?,
        detail: json!({
            "session": journal.snapshot(),
            "tool_requests": journal.tool_requests(),
        }),
    })
}

/// Separate human display from a locator that preserves every native path unit.
fn journal_locator(journal: &SessionJournal) -> Result<JournalLocator, CliError> {
    Ok(JournalLocator {
        session_id: journal.snapshot().session_id,
        journal: journal.path().to_str().map(str::to_owned),
        journal_display: journal.path().to_string_lossy().into_owned(),
        journal_native: NativePath::from_path(journal.path()).map_err(session_error)?,
    })
}

/// Decode only the current OS's bounded native locator; never authorize its path.
fn resolve_journal(path: Option<PathBuf>, native: Option<String>) -> Result<PathBuf, CliError> {
    match (path, native) {
        (Some(path), None) => Ok(path),
        (None, Some(encoded)) if encoded.len() <= 256 * 1024 => {
            let native: NativePath = serde_json::from_str(&encoded)
                .map_err(|_| CliError::new("configuration", "Invalid native journal locator"))?;
            native.to_path_buf().map_err(session_error)
        }
        _ => Err(CliError::new(
            "configuration",
            "Supply one journal path or native locator",
        )),
    }
}

/// Serialize untrusted strings as escaped JSON and propagate broken output errors.
fn print_json(value: &impl Serialize) -> Result<(), CliError> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, value)
        .map_err(|_| CliError::new("output", "Could not write JSON output"))?;
    output
        .write_all(b"\n")
        .map_err(|_| CliError::new("output", "Could not finish JSON output"))
}

fn session_error(error: omgcode_core::session::SessionError) -> CliError {
    AppError::session(error).into()
}

fn provider_error(error: omgcode_core::provider::ProviderError) -> CliError {
    AppError::provider(error).into()
}
