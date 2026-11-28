//! One confined `claude` process per OMGCode turn (ADR-0109).
//!
//! [`ClaudeCodeClient`] holds a checked launch plan. Each run writes a fresh
//! per-turn folder under the connection's runtime folder (the system prompt,
//! the MCP configuration, the settings, and an empty working folder), binds
//! the local channel the relay connects to, and starts the program with an
//! environment built from nothing. Standard input carries the rendered
//! conversation and is then closed. Everything the process writes is
//! untrusted: [`Session`] parses it with the bounded readers of
//! [`super::stream`] and [`super::mcp`], checks the lockdown, and ends the
//! tree on any surprise.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use omgcode_platform::container::ContainerName;
use omgcode_platform::process::{
    self as platform, CancelSignal, Confinement, NetworkAccess, ProcessLimits, ProcessSpec,
    StreamingChild, TreeLimits, UnixSocketAccess,
};
use serde_json::json;

use super::ClaudeCodeFailure;
use super::history::{self, RenderError};
use super::mcp::{MAX_MCP_LINE_BYTES, McpServer, McpTool, PendingCall};
use super::stream::{
    AssistantBlock, BoundedLines, InitReport, StreamError, StreamEvent, check_lockdown,
    check_toolless_lockdown, parse_line,
};
use crate::prompt::ModelMessage;
use crate::provider::{ProviderError, ReasoningEffort};
use crate::turn::{CancelToken, ToolDefinition};

/// The MCP server name, so every tool is `mcp__omgcode__<name>`.
pub const SERVER_NAME: &str = "omgcode";
/// The prefix Claude Code gives the server's tools.
pub const TOOL_PREFIX: &str = "mcp__omgcode__";
/// The hidden subcommand of the `omgcode` executable that relays MCP bytes.
pub const RELAY_SUBCOMMAND: &str = "mcp-relay";
/// The relay's environment variable that carries the per-turn nonce.
pub const RELAY_NONCE_VARIABLE: &str = "OMGCODE_RELAY_NONCE";
/// The longest accepted model name.
const MAX_MODEL_BYTES: usize = 128;
/// How long the relay has to connect after the process starts.
const CONNECT_WINDOW: Duration = Duration::from_secs(60);
/// How often the event loop checks cancellation and deadlines.
const POLL: Duration = Duration::from_millis(25);
/// How long the process may take to exit after its final result.
const EXIT_GRACE: Duration = Duration::from_secs(10);
/// The settings that switch off the built-in plugins Claude Code 2.1.325
/// loads even with `--setting-sources ""`. A plugin this list misses is
/// still refused by the lockdown check.
const BUILTIN_PLUGINS: [&str; 4] = [
    "cc-plugin-agents-md@builtin",
    "cc-plugin-privacy@builtin",
    "cc-plugin-plugin-authoring@builtin",
    "cc-plugin-telemetry@builtin",
];

/// A checked launch plan for one recorded `claude_code` connection.
///
/// The caller resolves every path from the connection's recorded identities
/// and, on Windows, checks that the connection's container holds its grants.
#[derive(Clone, Debug)]
pub struct ClaudeCodeConfig {
    /// The live canonical `claude` program.
    pub program: PathBuf,
    /// The live sign-in folder, passed as `CLAUDE_CONFIG_DIR`. OMGCode never
    /// opens a file in it.
    pub config_folder: PathBuf,
    /// The connection's runtime folder, which the container can read.
    pub runtime_folder: PathBuf,
    /// The `omgcode` executable that serves as the MCP relay.
    pub relay_program: PathBuf,
    /// The connection's AppContainer. Windows requires it; Linux ignores it.
    pub container: Option<ContainerName>,
    /// The model name or alias passed with `--model`.
    pub model: String,
    /// The session's reasoning effort, passed with `--effort`.
    pub effort: Option<ReasoningEffort>,
    /// The reply budget, passed as `CLAUDE_CODE_MAX_OUTPUT_TOKENS`.
    pub max_output_tokens: u32,
    /// How long one model request may wait for the process.
    pub request_timeout: Duration,
}

/// A validated Claude Code connection for chat, tool turns, and summaries.
#[derive(Clone, Debug)]
pub struct ClaudeCodeClient {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeClient {
    /// Check `config` without starting anything.
    ///
    /// # Errors
    ///
    /// [`ProviderError::InvalidConfiguration`] for a relative path, a model
    /// name that is empty, too long, starts with anything but a letter or
    /// digit, or holds other than letters, digits, `.`, `-`, `_`, `:`, `[`,
    /// or `]`, a zero output budget, or a timeout outside one second to an
    /// hour; [`ProviderError::EffortRefused`] for an effort Claude Code does
    /// not take (`none` and `minimal`).
    pub fn new(config: ClaudeCodeConfig) -> Result<Self, ProviderError> {
        let paths_absolute = [
            &config.program,
            &config.config_folder,
            &config.runtime_folder,
            &config.relay_program,
        ]
        .iter()
        .all(|path| path.is_absolute());
        if !paths_absolute
            || !valid_model(&config.model)
            || config.max_output_tokens == 0
            || config.request_timeout < Duration::from_secs(1)
            || config.request_timeout > platform::MAX_DEADLINE
            || (cfg!(windows) && config.container.is_none())
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        if let Some(effort) = config.effort
            && effort_argument(effort).is_none()
        {
            return Err(ProviderError::EffortRefused(effort));
        }
        Ok(Self { config })
    }

    /// The model name or alias.
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// The session's reasoning effort.
    pub const fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.config.effort
    }

    /// The same connection with another reply budget and no effort, for
    /// compaction summaries.
    #[must_use]
    pub fn for_summaries(&self, max_output_tokens: u32) -> Self {
        let mut config = self.config.clone();
        config.max_output_tokens = max_output_tokens.max(1);
        config.effort = None;
        Self { config }
    }

    /// One reply with no tools: chat without model tools, and compaction.
    /// Text deltas of the main agent go to `on_text` as they arrive.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Cancelled`] when `cancel` is set, and the
    /// [`ProviderError::ClaudeCode`] failures otherwise.
    pub fn complete_streaming(
        &self,
        messages: &[ModelMessage],
        on_text: &mut dyn FnMut(&str),
        cancel: &CancelToken,
    ) -> Result<String, ProviderError> {
        let mut session = self.start(messages, &[])?;
        match session.run(cancel, Some(on_text))? {
            Progress::Finished(text) => {
                session.finish();
                Ok(text)
            }
            Progress::Calls(_) => Err(failure(ClaudeCodeFailure::Protocol)),
        }
    }

    /// Start one process for `messages`, offering `tools` through the relay
    /// when any are given.
    pub(super) fn start(
        &self,
        messages: &[ModelMessage],
        tools: &[ToolDefinition],
    ) -> Result<Session, ProviderError> {
        let marker = random_hex(8)?;
        let rendered = history::render(messages, &marker).map_err(|error| match error {
            RenderError::TooLarge => ProviderError::RequestTooLarge,
            RenderError::Empty => ProviderError::InvalidConfiguration,
        })?;
        let turn = TurnFolder::create(&self.config.runtime_folder)?;
        fs::write(turn.path.join("system.txt"), rendered.system.as_bytes())
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        let settings = json!({
            "enabledPlugins": BUILTIN_PLUGINS
                .iter()
                .map(|plugin| ((*plugin).to_owned(), json!(false)))
                .collect::<serde_json::Map<_, _>>(),
        });
        fs::write(turn.path.join("settings.json"), settings.to_string())
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;

        let (sender, events) = mpsc::channel();
        let accept_cancel = CancelSignal::new();
        let mut server = None;
        let mut expected_tools = Vec::new();
        let mcp_config = if tools.is_empty() {
            json!({ "mcpServers": {} })
        } else {
            let listener = bind_listener(&turn, self.config.container.as_ref())?;
            let nonce = random_hex(32)?;
            let config = json!({
                "mcpServers": {
                    SERVER_NAME: {
                        "type": "stdio",
                        "command": self.config.relay_program,
                        "args": [RELAY_SUBCOMMAND, listener.address().to_string_lossy()],
                        "env": { RELAY_NONCE_VARIABLE: nonce },
                    }
                }
            });
            spawn_acceptor(listener, nonce, accept_cancel.clone(), sender.clone())?;
            server = Some(McpServer::new(
                SERVER_NAME,
                env!("CARGO_PKG_VERSION"),
                tools.iter().map(mcp_tool).collect(),
            ));
            expected_tools = tools
                .iter()
                .map(|tool| format!("{TOOL_PREFIX}{}", tool.name))
                .collect();
            config
        };
        let launched = self.launch(turn, &mcp_config, rendered.input, tools, sender);
        let Ok((child, turn)) = launched else {
            accept_cancel.cancel();
            return Err(launched.err().unwrap_or(failure(ClaudeCodeFailure::Failed)));
        };
        Ok(Session {
            child: Some(child),
            events,
            writer: None,
            shutdown: None,
            accept_cancel,
            server,
            expected_tools,
            init_seen: false,
            unreturned: Vec::new(),
            held: Vec::new(),
            announced: Vec::new(),
            last_call: None,
            text: String::new(),
            request_timeout: self.config.request_timeout,
            _turn: turn,
        })
    }

    /// Write the MCP configuration, start the process, and start the threads
    /// that feed its input and read its output.
    fn launch(
        &self,
        turn: TurnFolder,
        mcp_config: &serde_json::Value,
        input: String,
        tools: &[ToolDefinition],
        sender: Sender<Event>,
    ) -> Result<(StreamingChild, TurnFolder), ProviderError> {
        fs::write(turn.path.join("mcp.json"), mcp_config.to_string())
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        let spec = self.spec(&turn, tools)?;
        let confinement = self.confinement(&turn)?;
        let limits = ProcessLimits {
            deadline: platform::MAX_DEADLINE,
            max_stdout_bytes: 1,
            max_stderr_bytes: 64 * 1024,
            cleanup_timeout: Duration::from_secs(5),
            tree: Some(TreeLimits {
                max_memory_bytes: 4 * 1024 * 1024 * 1024,
                max_processes: 512,
                max_cpu_time: Duration::from_secs(4 * 60 * 60),
                max_disk_bytes: None,
            }),
        };
        let mut child = platform::spawn_confined_streaming(&spec, &confinement, &limits)
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        let stdin = child.take_stdin();
        let stdout = child.take_stdout();
        let (Some(mut stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(failure(ClaudeCodeFailure::Failed));
        };
        let input = input.into_bytes();
        thread::Builder::new()
            .name("claude-code-stdin".into())
            .spawn(move || {
                // A failed write shows up as a missing result.
                let _ = stdin.write_all(&input);
                let _ = stdin.flush();
            })
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        let stdout_sender = sender;
        thread::Builder::new()
            .name("claude-code-stdout".into())
            .spawn(move || {
                for line in BoundedLines::new(stdout) {
                    let event = line.and_then(|bytes| parse_line(&bytes));
                    let failed = event.is_err();
                    if stdout_sender.send(Event::Stdout(event)).is_err() || failed {
                        return;
                    }
                }
                let _ = stdout_sender.send(Event::StdoutEnded);
            })
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        Ok((child, turn))
    }

    fn spec(
        &self,
        turn: &TurnFolder,
        tools: &[ToolDefinition],
    ) -> Result<ProcessSpec, ProviderError> {
        let path = |name: &str| turn.path.join(name).into_os_string();
        let mut arguments: Vec<OsString> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--tools",
            "",
            "--strict-mcp-config",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        arguments.extend([
            "--mcp-config".into(),
            path("mcp.json"),
            "--setting-sources".into(),
            "".into(),
            "--settings".into(),
            path("settings.json"),
            "--system-prompt-file".into(),
            path("system.txt"),
            "--no-session-persistence".into(),
            "--disable-slash-commands".into(),
            "--permission-prompts".into(),
            "none".into(),
            "--permission-mode".into(),
            "dontAsk".into(),
            "--model".into(),
            self.config.model.clone().into(),
        ]);
        if !tools.is_empty() {
            arguments.extend(["--allowedTools".into(), format!("{TOOL_PREFIX}*").into()]);
        }
        if let Some(effort) = self.config.effort.and_then(effort_argument) {
            arguments.extend(["--effort".into(), effort.into()]);
        }
        let mut environment = base_environment(turn, self.config.container.as_ref())?;
        environment.extend(
            [
                (
                    "CLAUDE_CONFIG_DIR",
                    self.config.config_folder.clone().into_os_string(),
                ),
                ("DISABLE_AUTOUPDATER", "1".into()),
                ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1".into()),
                ("DISABLE_TELEMETRY", "1".into()),
                ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".into()),
                (
                    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
                    self.config.max_output_tokens.to_string().into(),
                ),
                ("MCP_TIMEOUT", CONNECT_WINDOW.as_millis().to_string().into()),
                // The turn runtime may hold a call open while the operator
                // decides an approval; it bounds the turn itself.
                (
                    "MCP_TOOL_TIMEOUT",
                    platform::MAX_DEADLINE.as_millis().to_string().into(),
                ),
            ]
            .into_iter()
            .map(|(name, value)| (OsString::from(name), value)),
        );
        Ok(ProcessSpec {
            program: self.config.program.clone(),
            arguments,
            working_directory: turn.path.join("work"),
            environment,
        })
    }

    fn confinement(&self, turn: &TurnFolder) -> Result<Confinement, ProviderError> {
        let folder = |path: &Path| {
            path.parent()
                .map(Path::to_path_buf)
                .ok_or(ProviderError::InvalidConfiguration)
        };
        let program_folder = folder(&self.config.program)?;
        let relay_folder = folder(&self.config.relay_program)?;
        let mut read_only = vec![program_folder];
        if !read_only.contains(&relay_folder) {
            read_only.push(relay_folder);
        }
        let mut read_write = vec![self.config.config_folder.clone()];
        if cfg!(windows) {
            read_only.push(self.config.runtime_folder.clone());
        } else {
            read_only.push(turn.path.clone());
            read_only.extend(system_paths()?);
            read_write.push(turn.path.join("tmp"));
            read_write.push(PathBuf::from("/dev/null"));
        }
        Ok(Confinement {
            read_only,
            read_write,
            unix_sockets: if cfg!(windows) {
                UnixSocketAccess::PairsOnly
            } else {
                UnixSocketAccess::HostServices
            },
            network: NetworkAccess::Internet,
            container: self.config.container.clone(),
        })
    }
}

/// What a run produced when the event loop returned.
#[derive(Debug)]
pub(super) enum Progress {
    /// Calls the turn runtime must run and answer, with any text the model
    /// wrote before them.
    Calls(Vec<PendingCall>),
    /// The final reply text.
    Finished(String),
}

enum Event {
    Stdout(Result<StreamEvent, StreamError>),
    StdoutEnded,
    McpConnected(Box<McpChannel>),
    Mcp(Result<Vec<u8>, StreamError>),
    McpEnded,
    McpFailed,
}

struct McpChannel {
    writer: omgcode_platform::ipc::LocalWriter,
    shutdown: omgcode_platform::ipc::ShutdownHandle,
}

/// A running process and the state of its turn.
pub(super) struct Session {
    child: Option<StreamingChild>,
    events: Receiver<Event>,
    writer: Option<omgcode_platform::ipc::LocalWriter>,
    shutdown: Option<omgcode_platform::ipc::ShutdownHandle>,
    accept_cancel: CancelSignal,
    server: Option<McpServer>,
    expected_tools: Vec<String>,
    init_seen: bool,
    /// Calls received and not yet returned to the runtime.
    unreturned: Vec<PendingCall>,
    /// Calls returned to the runtime, with the identifier it was given.
    held: Vec<(String, PendingCall)>,
    /// Tool-use identifiers the latest assistant message announced.
    announced: Vec<String>,
    last_call: Option<Instant>,
    /// Main-agent text since the last round.
    text: String,
    request_timeout: Duration,
    _turn: TurnFolder,
}

/// How long to wait for more calls of the same response after one arrives
/// when the assistant message has not announced them all.
const SETTLE: Duration = Duration::from_millis(200);
/// The most calls one round returns, matching the runtime's limit.
pub(super) const MAX_CALLS_PER_ROUND: usize = 8;

impl Session {
    /// Text the model wrote since the last round.
    pub(super) fn take_text(&mut self) -> String {
        std::mem::take(&mut self.text)
    }

    /// The calls returned in the last round, by the identifier the runtime
    /// was given.
    pub(super) fn held(&self) -> &[(String, PendingCall)] {
        &self.held
    }

    /// Record that `calls` were returned under `ids`.
    pub(super) fn hold(&mut self, held: Vec<(String, PendingCall)>) {
        self.held = held;
    }

    /// Answer one held call with its result text.
    pub(super) fn answer(&mut self, call: &PendingCall, text: &str) -> Result<(), ProviderError> {
        let server = self
            .server
            .as_mut()
            .ok_or(failure(ClaudeCodeFailure::Protocol))?;
        match server
            .answer(&call.key, text, false)
            .map_err(|_| failure(ClaudeCodeFailure::Protocol))?
        {
            super::mcp::Answer::Line(line) => self.write(&line),
            super::mcp::Answer::Cancelled => Ok(()),
        }
    }

    /// Run until calls are ready or the final result arrives.
    pub(super) fn run(
        &mut self,
        cancel: &CancelToken,
        mut on_text: Option<&mut dyn FnMut(&str)>,
    ) -> Result<Progress, ProviderError> {
        let waited = Instant::now();
        loop {
            if cancel.is_cancelled() {
                self.kill();
                return Err(ProviderError::Cancelled);
            }
            if waited.elapsed() > self.request_timeout {
                self.kill();
                return Err(failure(ClaudeCodeFailure::Failed));
            }
            if let Some(calls) = self.ready_calls() {
                return Ok(Progress::Calls(calls));
            }
            let event = match self.events.recv_timeout(POLL) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.kill();
                    return Err(failure(ClaudeCodeFailure::Failed));
                }
            };
            let result = match event {
                Event::Stdout(Ok(event)) => self.stream_event(event, &mut on_text),
                Event::Stdout(Err(_)) => Err(failure(ClaudeCodeFailure::Protocol)),
                Event::StdoutEnded => Err(failure(ClaudeCodeFailure::Failed)),
                Event::McpConnected(channel) => {
                    self.writer = Some(channel.writer);
                    self.shutdown = Some(channel.shutdown);
                    Ok(None)
                }
                Event::Mcp(Ok(line)) => self.mcp_line(&line).map(|()| None),
                Event::Mcp(Err(_)) | Event::McpFailed => Err(failure(ClaudeCodeFailure::Protocol)),
                // The relay closed; a later call would fail, and the result
                // decides the turn.
                Event::McpEnded => Ok(None),
            };
            match result {
                Ok(Some(text)) => return Ok(Progress::Finished(text)),
                Ok(None) => {}
                Err(error) => {
                    self.kill();
                    return Err(error);
                }
            }
        }
    }

    /// Calls ready to return: the lockdown was checked, and either the
    /// round is full, every announced call has arrived, or no further call
    /// arrived for [`SETTLE`].
    fn ready_calls(&mut self) -> Option<Vec<PendingCall>> {
        if self.unreturned.is_empty() || !self.init_seen {
            return None;
        }
        let announced = !self.announced.is_empty()
            && self.announced.iter().all(|id| {
                self.unreturned
                    .iter()
                    .any(|call| call.tool_use_id.as_deref() == Some(id.as_str()))
            });
        let settled = self.last_call.is_some_and(|at| at.elapsed() >= SETTLE);
        if self.unreturned.len() < MAX_CALLS_PER_ROUND && !announced && !settled {
            return None;
        }
        let count = self.unreturned.len().min(MAX_CALLS_PER_ROUND);
        Some(self.unreturned.drain(..count).collect())
    }

    fn stream_event(
        &mut self,
        event: StreamEvent,
        on_text: &mut Option<&mut dyn FnMut(&str)>,
    ) -> Result<Option<String>, ProviderError> {
        match event {
            StreamEvent::Init(report) => self.init(&report).map(|()| None),
            StreamEvent::ToolUseStart(name) => self.check_tool(&name).map(|()| None),
            StreamEvent::TextDelta(text) => {
                if let Some(on_text) = on_text {
                    on_text(&text);
                }
                Ok(None)
            }
            StreamEvent::Assistant(message) => {
                if message.error.as_deref() == Some("authentication_failed") {
                    return Err(failure(ClaudeCodeFailure::NotSignedIn));
                }
                let main = message.parent_tool_use_id.is_none();
                let mut announced = Vec::new();
                for block in &message.blocks {
                    match block {
                        AssistantBlock::ToolUse { id, name, .. } => {
                            self.check_tool(name)?;
                            announced.push(id.clone());
                        }
                        AssistantBlock::Text(text) if main => {
                            if !self.text.is_empty() {
                                self.text.push_str("\n\n");
                            }
                            self.text.push_str(text);
                        }
                        AssistantBlock::Text(_) | AssistantBlock::Other => {}
                    }
                }
                if !announced.is_empty() {
                    self.announced = announced;
                }
                Ok(None)
            }
            StreamEvent::Result(report) => {
                if report.is_error {
                    return Err(failure(ClaudeCodeFailure::Failed));
                }
                if !self.init_seen || !self.unreturned.is_empty() || !self.held.is_empty() {
                    return Err(failure(ClaudeCodeFailure::Protocol));
                }
                let text = self.take_text();
                Ok(Some(if text.is_empty() {
                    report.result.unwrap_or_default()
                } else {
                    text
                }))
            }
            StreamEvent::Ignored => Ok(None),
        }
    }

    fn init(&mut self, report: &InitReport) -> Result<(), ProviderError> {
        if self.init_seen {
            return Err(failure(ClaudeCodeFailure::Protocol));
        }
        let offered: Vec<&str> = self
            .expected_tools
            .iter()
            .filter_map(|tool| tool.strip_prefix(TOOL_PREFIX))
            .collect();
        let checked = if self.server.is_some() {
            check_lockdown(report, SERVER_NAME, &offered)
        } else {
            check_toolless_lockdown(report)
        };
        checked.map_err(|_| failure(ClaudeCodeFailure::Lockdown))?;
        self.init_seen = true;
        Ok(())
    }

    fn check_tool(&self, name: &str) -> Result<(), ProviderError> {
        if self.expected_tools.iter().any(|tool| tool == name) {
            Ok(())
        } else {
            Err(failure(ClaudeCodeFailure::Lockdown))
        }
    }

    fn mcp_line(&mut self, line: &[u8]) -> Result<(), ProviderError> {
        let server = self
            .server
            .as_mut()
            .ok_or(failure(ClaudeCodeFailure::Protocol))?;
        let outputs = server
            .handle_line(line)
            .map_err(|_| failure(ClaudeCodeFailure::Protocol))?;
        for output in outputs {
            match output {
                super::mcp::McpOutput::Reply(reply) => self.write(&reply)?,
                super::mcp::McpOutput::Call(call) => {
                    let offered = self
                        .expected_tools
                        .iter()
                        .any(|tool| tool.strip_prefix(TOOL_PREFIX) == Some(call.name.as_str()));
                    if !offered {
                        return Err(failure(ClaudeCodeFailure::Lockdown));
                    }
                    self.unreturned.push(call);
                    self.last_call = Some(Instant::now());
                }
                // A cancelled call stays held until the runtime answers it;
                // the answer is then dropped.
                super::mcp::McpOutput::Cancelled(_) => {}
            }
        }
        Ok(())
    }

    fn write(&mut self, line: &[u8]) -> Result<(), ProviderError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or(failure(ClaudeCodeFailure::Protocol))?;
        writer
            .write_all(line)
            .and_then(|()| writer.flush())
            .map_err(|_| failure(ClaudeCodeFailure::Protocol))
    }

    /// End the run after its result: close the channel, let the process
    /// exit, and end the tree if it does not.
    pub(super) fn finish(&mut self) {
        self.close_channel();
        if let Some(child) = self.child.take() {
            // The result is already decided; a slow or failed exit only ends
            // the tree, which `finish` confirms.
            let _ = child.finish(EXIT_GRACE);
        }
    }

    /// End the tree at once.
    pub(super) fn kill(&mut self) {
        if let Some(child) = &self.child {
            child.cancel_signal().cancel();
        }
        self.close_channel();
        if let Some(child) = self.child.take() {
            let _ = child.finish(Duration::ZERO);
        }
    }

    fn close_channel(&mut self) {
        self.accept_cancel.cancel();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.shutdown();
        }
        self.writer = None;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A per-turn folder under the runtime folder, removed when dropped.
struct TurnFolder {
    path: PathBuf,
}

impl TurnFolder {
    fn create(runtime: &Path) -> Result<Self, ProviderError> {
        let name = random_hex(16)?;
        let path = runtime.join("turns").join(name);
        let made = fs::create_dir_all(runtime.join("turns"))
            .and_then(|()| fs::create_dir(&path))
            .and_then(|()| fs::create_dir(path.join("work")));
        if made.is_err() {
            return Err(failure(ClaudeCodeFailure::Failed));
        }
        let folder = Self { path };
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(folder.path.join("ipc"))
                .and_then(|()| builder.create(folder.path.join("tmp")))
                .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        }
        Ok(folder)
    }
}

impl Drop for TurnFolder {
    fn drop(&mut self) {
        // Best effort: a leftover folder holds only this turn's prompt.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Bind the channel the relay connects to.
fn bind_listener(
    turn: &TurnFolder,
    container: Option<&ContainerName>,
) -> Result<omgcode_platform::ipc::LocalListener, ProviderError> {
    #[cfg(windows)]
    let target = {
        let _ = turn;
        omgcode_platform::ipc::ListenerTarget::Container {
            container: container
                .cloned()
                .ok_or(ProviderError::InvalidConfiguration)?,
        }
    };
    #[cfg(target_os = "linux")]
    let target = {
        let _ = container;
        omgcode_platform::ipc::ListenerTarget::PrivateDirectory {
            directory: turn.path.join("ipc"),
            name: OsString::from("mcp.sock"),
        }
    };
    omgcode_platform::ipc::LocalListener::bind(target)
        .map_err(|_| failure(ClaudeCodeFailure::Failed))
}

/// Accept the relay, check its nonce, then forward its lines.
fn spawn_acceptor(
    listener: omgcode_platform::ipc::LocalListener,
    nonce: String,
    cancel: CancelSignal,
    sender: Sender<Event>,
) -> Result<(), ProviderError> {
    thread::Builder::new()
        .name("claude-code-mcp".into())
        .spawn(move || {
            let accepted = listener
                .accept(Instant::now() + CONNECT_WINDOW, &cancel)
                .and_then(|stream| stream.split());
            let Ok((reader, writer, shutdown)) = accepted else {
                let _ = sender.send(Event::McpFailed);
                return;
            };
            let mut lines = BoundedLines::with_limit(reader, MAX_MCP_LINE_BYTES);
            let presented = match lines.next() {
                Some(Ok(line)) => line,
                _ => Vec::new(),
            };
            if !constant_time_eq(&presented, nonce.as_bytes()) {
                let _ = shutdown.shutdown();
                let _ = sender.send(Event::McpFailed);
                return;
            }
            let channel = Box::new(McpChannel { writer, shutdown });
            if sender.send(Event::McpConnected(channel)).is_err() {
                return;
            }
            for line in lines {
                let failed = line.is_err();
                if sender.send(Event::Mcp(line)).is_err() || failed {
                    return;
                }
            }
            let _ = sender.send(Event::McpEnded);
        })
        .map(|_| ())
        .map_err(|_| failure(ClaudeCodeFailure::Failed))
}

/// The environment basics, built from nothing.
fn base_environment(
    turn: &TurnFolder,
    container: Option<&ContainerName>,
) -> Result<Vec<(OsString, OsString)>, ProviderError> {
    let mut environment = Vec::new();
    #[cfg(windows)]
    {
        let _ = turn;
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
        let profile = container
            .map(omgcode_platform::container::profile_folder)
            .transpose()
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?
            .ok_or(ProviderError::InvalidConfiguration)?;
        // Windows replaces LOCALAPPDATA, TEMP, and TMP with the container's
        // own folders; the value only has to be present.
        let local = std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| profile.clone().into());
        let mut system32 = PathBuf::from(&root);
        system32.push("System32");
        environment.extend([
            (OsString::from("SystemRoot"), root.clone()),
            (OsString::from("windir"), root),
            (OsString::from("LOCALAPPDATA"), local),
            (
                OsString::from("USERPROFILE"),
                profile.clone().into_os_string(),
            ),
            (OsString::from("HOME"), profile.into_os_string()),
            (OsString::from("PATH"), system32.into_os_string()),
        ]);
    }
    #[cfg(not(windows))]
    {
        let _ = container;
        let tmp = turn.path.join("tmp").into_os_string();
        environment.extend([
            (OsString::from("HOME"), tmp.clone()),
            (OsString::from("TMPDIR"), tmp),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("LANG"), OsString::from("C.UTF-8")),
        ]);
    }
    Ok(environment)
}

/// The system folders and devices a Linux run reads: the library and program
/// folders, the network support files, and the random and zero devices. Each
/// is canonical, so no rule names a link.
fn system_paths() -> Result<Vec<PathBuf>, ProviderError> {
    #[cfg(target_os = "linux")]
    {
        let mut paths: Vec<PathBuf> = Vec::new();
        for listed in [
            "/usr",
            "/lib",
            "/lib64",
            "/bin",
            "/dev/zero",
            "/dev/random",
            "/dev/urandom",
        ] {
            if let Ok(path) = fs::canonicalize(listed)
                && !paths.contains(&path)
            {
                paths.push(path);
            }
        }
        let support =
            platform::network_support_paths().map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        for path in support {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        Ok(paths)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Vec::new())
    }
}

/// The `--effort` value for an effort Claude Code takes.
const fn effort_argument(effort: ReasoningEffort) -> Option<&'static str> {
    match effort {
        ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        ReasoningEffort::XHigh => Some("xhigh"),
        ReasoningEffort::Max => Some("max"),
        ReasoningEffort::None | ReasoningEffort::Minimal => None,
    }
}

/// A model name or alias that cannot be read as an option.
fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= MAX_MODEL_BYTES
        && model
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_:[]".contains(&byte))
}

fn mcp_tool(tool: &ToolDefinition) -> McpTool {
    McpTool {
        name: tool.name.to_owned(),
        description: tool.description.to_owned(),
        input_schema: tool.parameters.clone(),
    }
}

fn random_hex(bytes: usize) -> Result<String, ProviderError> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer).map_err(|_| failure(ClaudeCodeFailure::Failed))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

pub(super) const fn failure(kind: ClaudeCodeFailure) -> ProviderError {
    ProviderError::ClaudeCode(kind)
}

/// Copy MCP bytes between this process's standard streams and the channel
/// at `address`, after sending `nonce` (ADR-0109). This is the whole of the
/// `omgcode mcp-relay` subcommand; it parses nothing.
///
/// # Errors
///
/// When the channel cannot be reached or a stream fails.
pub fn relay(
    address: &OsStr,
    nonce: &str,
    input: impl Read + Send + 'static,
    mut output: impl Write,
) -> io::Result<()> {
    let stream = omgcode_platform::ipc::connect(address, Instant::now() + CONNECT_WINDOW)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let (mut reader, mut writer, shutdown) = stream
        .split()
        .map_err(|error| io::Error::other(error.to_string()))?;
    writer.write_all(nonce.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let upstream = thread::Builder::new()
        .name("mcp-relay-input".into())
        .spawn(move || {
            let mut input = input;
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let Some(chunk) = buffer.get(..count) else {
                            break;
                        };
                        if writer
                            .write_all(chunk)
                            .and_then(|()| writer.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let _ = shutdown.shutdown();
        })?;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let chunk = buffer.get(..count).unwrap_or_default();
        output.write_all(chunk)?;
        output.flush()?;
    }
    // The input thread ends when the client closes its side.
    drop(upstream);
    Ok(())
}
