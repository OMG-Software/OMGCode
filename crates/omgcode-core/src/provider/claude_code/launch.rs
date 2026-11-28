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

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omgcode_platform::container::ContainerName;
use omgcode_platform::process::{
    self as platform, CancelSignal, Confinement, NetworkAccess, ProcessError, ProcessLimits,
    ProcessSpec, StreamingChild, TreeLimits, UnixSocketAccess,
};
use serde_json::json;

use super::ClaudeCodeFailure;
use super::history::{self, RenderError};
use super::mcp::{MAX_MCP_LINE_BYTES, McpServer, McpTool, PendingCall};
use super::stream::{
    AssistantBlock, BoundedLines, InitReport, StreamError, StreamEvent, check_lockdown,
    check_toolless_lockdown, parse_line,
};
use super::watch::{self, Watcher};
use crate::control::{ClaudeCodeWatch, TurnRecord, WatchCursor};
use crate::prompt::ModelMessage;
use crate::provider::{MAX_STREAM_BYTES, ModelReply, ProviderError, ReasoningEffort};
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
/// How long the worker threads may take to end once their reads and writes
/// were unblocked.
const WORKER_GRACE: Duration = Duration::from_secs(5);
/// Events the readers may queue while the turn runtime runs calls or waits
/// for an approval. A full queue blocks the readers, which in turn blocks
/// the process's output; one event holds at most one bounded line.
const EVENT_QUEUE: usize = 8;
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
    /// The settings the connection was resolved from. Each process holds
    /// the connection's turn lock and ends when a later state withdraws it.
    pub watch: ClaudeCodeWatch,
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
    /// [`ProviderError::InvalidConfiguration`] for a relative path, a relay
    /// path that is not UTF-8 (the MCP configuration is JSON text), a model
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
            || config.relay_program.to_str().is_none()
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

    /// One reply with no tools, as [`Self::complete_streaming`] without
    /// forwarding text.
    ///
    /// # Errors
    ///
    /// As [`Self::complete_streaming`].
    pub fn complete(
        &self,
        messages: &[ModelMessage],
        cancel: &CancelToken,
    ) -> Result<ModelReply, ProviderError> {
        self.complete_streaming(messages, &mut |_| {}, cancel)
    }

    /// One reply with no tools: chat without model tools, and compaction.
    /// Text deltas of the main agent go to `on_text` as they arrive. The
    /// reply's finish reason is `stop` and it reports no usage.
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
    ) -> Result<ModelReply, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let mut session = self.start(messages, &[])?;
        match session.run(cancel, Some(on_text))? {
            Progress::Finished(content) => {
                session.finish()?;
                Ok(ModelReply {
                    content,
                    finish_reason: "stop".to_owned(),
                    usage: None,
                })
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
        // The lock, the turn's record, and the settings check come first, so
        // a connection the settings withdrew gets no turn folder and starts
        // no process. The record, written under the lock and before any
        // process starts, outlives an OMGCode process that exits or fails
        // before the tree was observed ended.
        let held = watch::hold_turn(&self.config.watch)?;
        let turn = match self.prepare_turn_folder(&rendered.system) {
            Ok(turn) => turn,
            Err(error) => {
                watch::release_unstarted(held.lock, held.record);
                return Err(error);
            }
        };

        let (sender, events) = mpsc::sync_channel(EVENT_QUEUE);
        // The session owns every worker and the tree from here on, so an
        // early return drops it, and its `Drop` ends and joins them all.
        let mut session = Session::new(events, turn, self.config.request_timeout);
        session.turn_lock = Some(held.lock);
        session.turn_record = Some(held.record);
        session.turn_watch = Some(self.config.watch.clone());
        let cursor = held.cursor;
        let mcp_config = if tools.is_empty() {
            json!({ "mcpServers": {} })
        } else {
            let listener = bind_listener(&mut session.turn, self.config.container.as_ref())?;
            let address = listener
                .address()
                .to_str()
                .ok_or(failure(ClaudeCodeFailure::Failed))?
                .to_owned();
            let nonce = random_hex(32)?;
            let relay = self
                .config
                .relay_program
                .to_str()
                .ok_or(ProviderError::InvalidConfiguration)?;
            let config = json!({
                "mcpServers": {
                    SERVER_NAME: {
                        "type": "stdio",
                        "command": relay,
                        "args": [RELAY_SUBCOMMAND, address],
                        "env": { RELAY_NONCE_VARIABLE: nonce },
                    }
                }
            });
            let acceptor = spawn_acceptor(
                listener,
                nonce,
                session.accept_cancel.clone(),
                Arc::clone(&session.shutdown),
                sender.clone(),
            )?;
            session.workers.push(acceptor);
            session.server = Some(McpServer::new(
                SERVER_NAME,
                env!("CARGO_PKG_VERSION"),
                tools.iter().map(mcp_tool).collect(),
            ));
            session.expected_tools = tools
                .iter()
                .map(|tool| format!("{TOOL_PREFIX}{}", tool.name))
                .collect();
            config
        };
        self.launch(
            &mut session,
            &mcp_config,
            rendered.input,
            tools,
            sender,
            cursor,
        )?;
        Ok(session)
    }

    /// Make the turn's folder with its system prompt and its settings, which
    /// switch every built-in plugin off.
    fn prepare_turn_folder(&self, system: &str) -> Result<TurnFolder, ProviderError> {
        let turn = TurnFolder::create(&self.config.runtime_folder)?;
        fs::write(turn.path.join("system.txt"), system.as_bytes())
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        let settings = json!({
            "enabledPlugins": BUILTIN_PLUGINS
                .iter()
                .map(|plugin| ((*plugin).to_owned(), json!(false)))
                .collect::<serde_json::Map<_, _>>(),
        });
        fs::write(turn.path.join("settings.json"), settings.to_string())
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        Ok(turn)
    }

    /// Write the MCP configuration, start the process, and start the threads
    /// that feed its input and read its output, all owned by `session`.
    fn launch(
        &self,
        session: &mut Session,
        mcp_config: &serde_json::Value,
        input: String,
        tools: &[ToolDefinition],
        sender: SyncSender<Event>,
        cursor: WatchCursor,
    ) -> Result<(), ProviderError> {
        let turn = &session.turn;
        fs::write(turn.path.join("mcp.json"), mcp_config.to_string())
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        let spec = self.spec(turn, tools)?;
        let confinement = self.confinement(turn)?;
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
        let mut child = spawned(
            session,
            platform::spawn_confined_streaming(&spec, &confinement, &limits),
        )?;
        let stdin = child.take_stdin();
        let stdout = child.take_stdout();
        let stop = child.cancel_signal();
        // From here a failure drops the session, which ends the tree.
        session.child = Some(child);
        session.watcher = Some(Watcher::start(self.config.watch.clone(), cursor, stop)?);
        let (Some(mut stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(failure(ClaudeCodeFailure::Failed));
        };
        let input = input.into_bytes();
        let input_sender = sender.clone();
        let feeder = thread::Builder::new()
            .name("claude-code-stdin".into())
            .spawn(move || {
                let written = stdin.write_all(&input).and_then(|()| stdin.flush());
                // Reported before the input closes, so the result, which
                // the process writes only after reading its whole input,
                // always comes later. A closed queue means the turn ended.
                let _ = input_sender.send(Event::InputWritten(written.is_ok()));
                drop(stdin);
            })
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        session.workers.push(feeder);
        let reader = thread::Builder::new()
            .name("claude-code-stdout".into())
            .spawn(move || {
                for line in BoundedLines::new(stdout) {
                    let event = line.and_then(|bytes| parse_line(&bytes));
                    let failed = event.is_err();
                    if sender.send(Event::Stdout(event)).is_err() || failed {
                        return;
                    }
                }
                // A closed queue means the turn already ended.
                let _ = sender.send(Event::StdoutEnded);
            })
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        session.workers.push(reader);
        Ok(())
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
    /// Whether the whole conversation reached the process's input.
    InputWritten(bool),
    McpConnected(Box<omgcode_platform::ipc::LocalWriter>),
    Mcp(Result<Vec<u8>, StreamError>),
    McpEnded,
    McpFailed,
}

impl Event {
    /// Whether the round fails on this output whatever came before it: a
    /// line that is not a stream event, the end of the process's output, or
    /// input that never fully reached it.
    const fn ends_the_turn(&self) -> bool {
        matches!(
            self,
            Self::Stdout(Err(_)) | Self::StdoutEnded | Self::InputWritten(false)
        )
    }
}

/// What [`Session::next_event`] found.
enum Next {
    Event(Event),
    /// Calls to return to the runtime; the queue was empty.
    Calls(Vec<PendingCall>),
    /// Nothing arrived within one poll.
    Idle,
    /// The queue is gone or every sender ended.
    Ended,
}

/// The channel's shutdown, published by the acceptor before its first
/// blocking read, so ending the turn can unblock every read and write.
type ShutdownSlot = Arc<Mutex<Option<omgcode_platform::ipc::ShutdownHandle>>>;

/// A running process and the state of its turn.
pub(super) struct Session {
    child: Option<StreamingChild>,
    /// `None` once the turn ended, so a reader blocked on the full queue
    /// wakes and ends.
    events: Option<Receiver<Event>>,
    writer: Option<omgcode_platform::ipc::LocalWriter>,
    shutdown: ShutdownSlot,
    accept_cancel: CancelSignal,
    /// The acceptor, input, and output threads, joined when the turn ends.
    workers: Vec<JoinHandle<()>>,
    server: Option<McpServer>,
    expected_tools: Vec<String>,
    init_seen: bool,
    /// Whether the whole conversation was written; a result before that is
    /// refused.
    input_written: bool,
    /// Text-delta bytes forwarded during the whole process.
    streamed_bytes: u64,
    /// The process's events an answer kept back, in arrival order; the
    /// round handles them before reading the queue again.
    deferred: VecDeque<Event>,
    /// Events left in the round's current batch; calls are handed off
    /// only between batches.
    batch_left: usize,
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
    /// The shared turn lock, held from before the launch until the tree and
    /// the workers ended, so a revocation's wait sees the process gone.
    turn_lock: Option<File>,
    /// The connection the lock belongs to.
    turn_watch: Option<ClaudeCodeWatch>,
    /// The turn's durable record, written before the launch and cleared only
    /// once the tree was observed ended.
    turn_record: Option<TurnRecord>,
    /// Whether a tree of this session was not observed ended, also one the
    /// launch could not stop before the session owned it.
    unconfirmed: bool,
    /// Ends the tree when the settings withdraw the connection.
    watcher: Option<Watcher>,
    /// Whether a stopped watcher had withdrawn the connection.
    withdrawn: bool,
    /// Removed after the tree and the workers ended.
    turn: TurnFolder,
}

/// How long to wait for more calls of the same response after one arrives
/// when the assistant message has not announced them all.
const SETTLE: Duration = Duration::from_millis(200);
/// The most calls one round returns, matching the runtime's limit.
pub(super) const MAX_CALLS_PER_ROUND: usize = 8;

impl Session {
    fn new(events: Receiver<Event>, turn: TurnFolder, request_timeout: Duration) -> Self {
        Self {
            child: None,
            events: Some(events),
            writer: None,
            shutdown: Arc::new(Mutex::new(None)),
            accept_cancel: CancelSignal::new(),
            workers: Vec::new(),
            server: None,
            expected_tools: Vec::new(),
            init_seen: false,
            input_written: false,
            streamed_bytes: 0,
            deferred: VecDeque::new(),
            batch_left: 0,
            unreturned: Vec::new(),
            held: Vec::new(),
            announced: Vec::new(),
            last_call: None,
            text: String::new(),
            request_timeout,
            turn_lock: None,
            turn_watch: None,
            turn_record: None,
            unconfirmed: false,
            watcher: None,
            withdrawn: false,
            turn,
        }
    }

    /// Whether the settings withdrew the connection during this process.
    fn withdrawn(&self) -> bool {
        self.withdrawn || self.watcher.as_ref().is_some_and(Watcher::withdrawn)
    }

    /// `result`, unless the settings withdrew the connection: a withdrawn
    /// turn fails, whatever the process did meanwhile.
    fn unless_withdrawn<T>(
        &mut self,
        result: Result<T, ProviderError>,
    ) -> Result<T, ProviderError> {
        if self.withdrawn() {
            self.kill();
            return Err(failure(ClaudeCodeFailure::Withdrawn));
        }
        result
    }

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

    /// Answer one held call with its result text. The write ends at the
    /// request timeout or when `cancel` is set.
    pub(super) fn answer(
        &mut self,
        call: &PendingCall,
        text: &str,
        cancel: &CancelToken,
    ) -> Result<(), ProviderError> {
        // A withdrawn connection gets no more results.
        let result = self.unless_withdrawn(Ok(()));
        let result = result.and_then(|()| self.answer_call(call, text, cancel));
        self.unless_withdrawn(result)
    }

    fn answer_call(
        &mut self,
        call: &PendingCall,
        text: &str,
        cancel: &CancelToken,
    ) -> Result<(), ProviderError> {
        let deadline = deadline_after(self.request_timeout);
        // A cancellation already waiting marks the call cancelled first, so
        // its answer is withheld.
        self.absorb_relay_events(cancel, deadline)?;
        let server = self
            .server
            .as_mut()
            .ok_or(failure(ClaudeCodeFailure::Protocol))?;
        match server
            .answer(&call.key, text, false)
            .map_err(|_| failure(ClaudeCodeFailure::Protocol))?
        {
            super::mcp::Answer::Line(line) => self.write(&line, cancel, deadline),
            super::mcp::Answer::Cancelled => Ok(()),
        }
    }

    /// Run until calls are ready or the final result arrives. A turn the
    /// settings withdrew fails with [`ClaudeCodeFailure::Withdrawn`], also
    /// when calls or a result arrived meanwhile.
    pub(super) fn run(
        &mut self,
        cancel: &CancelToken,
        on_text: Option<&mut dyn FnMut(&str)>,
    ) -> Result<Progress, ProviderError> {
        let result = self.unless_withdrawn(Ok(()));
        let result = result.and_then(|()| self.run_round(cancel, on_text));
        self.unless_withdrawn(result)
    }

    fn run_round(
        &mut self,
        cancel: &CancelToken,
        mut on_text: Option<&mut dyn FnMut(&str)>,
    ) -> Result<Progress, ProviderError> {
        let deadline = deadline_after(self.request_timeout);
        loop {
            if cancel.is_cancelled() {
                self.kill();
                return Err(ProviderError::Cancelled);
            }
            if Instant::now() > deadline {
                self.kill();
                return Err(failure(ClaudeCodeFailure::Failed));
            }
            let next = match self.next_event(cancel, deadline) {
                Ok(next) => next,
                Err(error) => {
                    self.kill();
                    return Err(error);
                }
            };
            let event = match next {
                Next::Event(event) => event,
                Next::Calls(calls) => return Ok(Progress::Calls(calls)),
                Next::Idle => continue,
                Next::Ended => {
                    self.kill();
                    return Err(failure(ClaudeCodeFailure::Failed));
                }
            };
            let result = match event {
                Event::Stdout(Ok(event)) => self.stream_event(event, &mut on_text),
                Event::Stdout(Err(_)) => Err(failure(ClaudeCodeFailure::Protocol)),
                Event::StdoutEnded | Event::InputWritten(false) => {
                    Err(failure(ClaudeCodeFailure::Failed))
                }
                Event::InputWritten(true) => {
                    self.input_written = true;
                    Ok(None)
                }
                Event::McpConnected(writer) => {
                    self.writer = Some(*writer);
                    Ok(None)
                }
                Event::Mcp(Ok(line)) => self.mcp_line(&line, cancel, deadline).map(|()| None),
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

    /// The next thing the round acts on. Events come in arrival order, in
    /// batches: a batch is at most the events kept back by an answer and a
    /// full queue, which is everything already waiting when it began. Calls
    /// are handed to the runtime only between batches, so a steady stream of
    /// events cannot hold back a ready call, and only after the relay's
    /// events already queued were handled, so a cancellation the relay sent
    /// before the hand-off retires its call first, also one queued right
    /// behind a batch's last event.
    ///
    /// # Errors
    ///
    /// The error of a relay event handled at the hand-off.
    fn next_event(
        &mut self,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<Next, ProviderError> {
        loop {
            if self.batch_left == 0 {
                if let Some(calls) = self.hand_off(cancel, deadline)? {
                    return Ok(Next::Calls(calls));
                }
                self.batch_left = self.deferred.len() + EVENT_QUEUE;
            }
            if let Some(event) = self.deferred.pop_front() {
                self.batch_left -= 1;
                return Ok(Next::Event(event));
            }
            let Some(events) = &self.events else {
                return Ok(Next::Ended);
            };
            match events.try_recv() {
                Ok(event) => {
                    self.batch_left -= 1;
                    return Ok(Next::Event(event));
                }
                Err(TryRecvError::Disconnected) => return Ok(Next::Ended),
                Err(TryRecvError::Empty) => {}
            }
            // Nothing is waiting: the batch ends, and ready calls go first.
            self.batch_left = 0;
            if let Some(calls) = self.hand_off(cancel, deadline)? {
                return Ok(Next::Calls(calls));
            }
            if !self.deferred.is_empty() {
                // The hand-off kept back output that arrived before what the
                // queue holds now; the next batch starts with it.
                continue;
            }
            let Some(events) = &self.events else {
                return Ok(Next::Ended);
            };
            return Ok(match events.recv_timeout(POLL) {
                Ok(event) => {
                    // A new batch starts with this event, so whatever queued
                    // behind it is handled before the next hand-off.
                    self.batch_left = EVENT_QUEUE;
                    Next::Event(event)
                }
                Err(RecvTimeoutError::Timeout) => Next::Idle,
                Err(RecvTimeoutError::Disconnected) => Next::Ended,
            });
        }
    }

    /// The calls to hand to the runtime, when some are ready once the
    /// relay's events already queued were handled. Those events can retire a
    /// ready call or add another; the process's output among them waits for
    /// the next batch, in arrival order. Output kept back that ends the turn
    /// holds the calls back, so the round fails on it before any call
    /// reaches the runtime.
    fn hand_off(
        &mut self,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<Option<Vec<PendingCall>>, ProviderError> {
        if !self.calls_ready() {
            return Ok(None);
        }
        self.absorb_relay_events(cancel, deadline)?;
        if !self.calls_ready() || self.deferred.iter().any(Event::ends_the_turn) {
            return Ok(None);
        }
        let count = self.unreturned.len().min(MAX_CALLS_PER_ROUND);
        Ok(Some(self.unreturned.drain(..count).collect()))
    }

    /// Handle the relay's events already waiting, at most a full queue, and
    /// keep the process's output for the next round in arrival order. A
    /// cancellation the relay sent before an answer then withholds it. The
    /// two sources share the queue only by arrival, so handling the relay's
    /// events first changes no meaningful order.
    fn absorb_relay_events(
        &mut self,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<(), ProviderError> {
        for _ in 0..EVENT_QUEUE {
            let Some(events) = &self.events else {
                return Ok(());
            };
            let Ok(event) = events.try_recv() else {
                return Ok(());
            };
            match event {
                Event::McpConnected(writer) => self.writer = Some(*writer),
                Event::Mcp(Ok(line)) => self.mcp_line(&line, cancel, deadline)?,
                Event::Mcp(Err(_)) | Event::McpFailed => {
                    return Err(failure(ClaudeCodeFailure::Protocol));
                }
                Event::McpEnded => {}
                output => self.deferred.push_back(output),
            }
        }
        Ok(())
    }

    /// Whether calls are ready to return: the lockdown was checked, and
    /// either the round is full, every announced call has arrived, or no
    /// further call arrived for [`SETTLE`].
    fn calls_ready(&self) -> bool {
        if self.unreturned.is_empty() || !self.init_seen {
            return false;
        }
        let announced = !self.announced.is_empty()
            && self.announced.iter().all(|id| {
                self.unreturned
                    .iter()
                    .any(|call| call.tool_use_id.as_deref() == Some(id.as_str()))
            });
        let settled = self.last_call.is_some_and(|at| at.elapsed() >= SETTLE);
        self.unreturned.len() >= MAX_CALLS_PER_ROUND || announced || settled
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
                // The process's output budget is its own; the host keeps one
                // of its own for everything it forwards.
                self.streamed_bytes = self.streamed_bytes.saturating_add(byte_count(&text));
                if self.streamed_bytes > MAX_STREAM_BYTES {
                    return Err(ProviderError::ResponseTooLarge);
                }
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
                        AssistantBlock::Text(text) if main => self.append_text(text)?,
                        AssistantBlock::Text(_) | AssistantBlock::Other => {}
                    }
                }
                if !announced.is_empty() {
                    self.announced = announced;
                }
                Ok(None)
            }
            StreamEvent::Result(report) => {
                if report.is_error || !self.input_written {
                    return Err(failure(ClaudeCodeFailure::Failed));
                }
                if !self.init_seen || !self.unreturned.is_empty() || !self.held.is_empty() {
                    return Err(failure(ClaudeCodeFailure::Protocol));
                }
                let text = self.take_text();
                if !text.is_empty() {
                    return Ok(Some(text));
                }
                // With no streamed text the result must carry the reply; an
                // explicitly empty one is a valid empty reply.
                report
                    .result
                    .map(Some)
                    .ok_or(failure(ClaudeCodeFailure::Protocol))
            }
            StreamEvent::Ignored => Ok(None),
        }
    }

    /// Append one text block of the main agent, within the host's budget.
    fn append_text(&mut self, text: &str) -> Result<(), ProviderError> {
        let separator = if self.text.is_empty() { 0 } else { 2 };
        let total = byte_count(&self.text)
            .saturating_add(separator)
            .saturating_add(byte_count(text));
        if total > MAX_STREAM_BYTES {
            return Err(ProviderError::ResponseTooLarge);
        }
        if separator > 0 {
            self.text.push_str("\n\n");
        }
        self.text.push_str(text);
        Ok(())
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

    fn mcp_line(
        &mut self,
        line: &[u8],
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<(), ProviderError> {
        let server = self
            .server
            .as_mut()
            .ok_or(failure(ClaudeCodeFailure::Protocol))?;
        let outputs = server
            .handle_line(line)
            .map_err(|_| failure(ClaudeCodeFailure::Protocol))?;
        for output in outputs {
            match output {
                super::mcp::McpOutput::Reply(reply) => self.write(&reply, cancel, deadline)?,
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
                super::mcp::McpOutput::Cancelled(key) => self.retire_cancelled(&key)?,
            }
        }
        Ok(())
    }

    /// A call cancelled before it was returned never reaches the runtime:
    /// it leaves the round and its pending request is retired. A call
    /// already returned stays held until the runtime answers it, and that
    /// answer is dropped.
    fn retire_cancelled(&mut self, key: &super::mcp::RequestKey) -> Result<(), ProviderError> {
        let Some(index) = self.unreturned.iter().position(|call| call.key == *key) else {
            return Ok(());
        };
        let call = self.unreturned.remove(index);
        let server = self
            .server
            .as_mut()
            .ok_or(failure(ClaudeCodeFailure::Protocol))?;
        match server.answer(&call.key, "", false) {
            Ok(super::mcp::Answer::Cancelled) => Ok(()),
            Ok(super::mcp::Answer::Line(_)) | Err(_) => Err(failure(ClaudeCodeFailure::Protocol)),
        }
    }

    /// Write one line to the relay. When `cancel` is set or `deadline`
    /// passes, a watcher shuts the channel, which ends a write the relay
    /// does not read on either platform, and ends the process tree. The
    /// tree's end closes the relay's side of the channel, which ends the
    /// write even when the shutdown request fails.
    ///
    /// A write counts as finished only once the writer records it in the
    /// [`WriteRace`], after `write_all` and `flush` return. A write whose
    /// bytes went out but that the watcher stopped first still fails the
    /// turn, and its tree is ended: the relay may have the answer, but the
    /// turn never reports one it did not record.
    fn write(
        &mut self,
        line: &[u8],
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<(), ProviderError> {
        let shutdown = self
            .shutdown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let tree = self.child.as_ref().map(StreamingChild::cancel_signal);
        let (Some(writer), Some(shutdown)) = (self.writer.as_mut(), shutdown) else {
            return Err(failure(ClaudeCodeFailure::Protocol));
        };
        // The turn fails once the write is stopped, so the tree ends now
        // rather than after the write returns: an ignored shutdown failure
        // cannot hold it.
        raced_write(
            || writer.write_all(line).and_then(|()| writer.flush()),
            cancel,
            || Instant::now() >= deadline,
            || {
                // A failed shutdown leaves the tree's end, raised next,
                // which closes the relay's side of the channel.
                let _ = shutdown.shutdown();
                if let Some(tree) = &tree {
                    tree.cancel();
                }
            },
            |_| {},
        )
    }

    /// End the run after its result: close the channel, let the process
    /// exit, end the tree if it does not, and wait for the workers.
    ///
    /// # Errors
    ///
    /// [`ClaudeCodeFailure::Withdrawn`] when the settings withdrew the
    /// connection before the process ended, and [`ClaudeCodeFailure::Failed`]
    /// when the end of the tree or of a worker could not be confirmed, when
    /// the tree's outcome could not be read, or when the turn's record could
    /// not be cleared.
    pub(super) fn finish(&mut self) -> Result<(), ProviderError> {
        self.close_channel();
        let (ended, read) = self.end_tree(EXIT_GRACE);
        let workers = self.stop_workers();
        let joined = !workers.panicked && workers.outstanding.is_empty();
        let released = self.release_turn(ended, workers.outstanding);
        if self.withdrawn {
            Err(failure(ClaudeCodeFailure::Withdrawn))
        } else if ended && read && joined && released {
            Ok(())
        } else {
            Err(failure(ClaudeCodeFailure::Failed))
        }
    }

    /// End the tree at once and wait for the workers. The caller already
    /// returns an error, so an unconfirmed end only leaves the platform's
    /// own fallback termination, and the turn lock stays held.
    pub(super) fn kill(&mut self) {
        if let Some(child) = &self.child {
            child.cancel_signal().cancel();
        }
        self.close_channel();
        // An unconfirmed end has already dropped the tree, whose own
        // fallback termination ran before this returns.
        let (ended, _read) = self.end_tree(Duration::ZERO);
        let workers = self.stop_workers();
        // The caller reports its own error; the turn's hold on the
        // connection still follows what was observed.
        let _released = self.release_turn(ended, workers.outstanding);
    }

    /// Finish the tree, waiting up to `grace` for it to end on its own.
    /// Returns whether the whole tree was observed ended, and whether its
    /// outcome was read: the platform can observe the tree empty and then
    /// fail its usage accounting or standard-error capture, which fails the
    /// turn but proves the tree's end.
    fn end_tree(&mut self, grace: Duration) -> (bool, bool) {
        let Some(child) = self.child.take() else {
            return (true, true);
        };
        let end = child.tree_end();
        let read = child.finish(grace).is_ok();
        (end.observed(), read)
    }

    /// Stop the watcher, then end the turn's hold on the connection by what
    /// was observed. A tree observed `ended` clears the turn's record; the
    /// lock is then released once every worker in `outstanding` ended, by a
    /// thread of its own, or at once when none is. A tree whose end was not
    /// confirmed keeps its record, which outlives this process, and its
    /// lock, until this process exits, so a revocation's wait never reports
    /// that turn ended. Returns whether the record was cleared and nothing
    /// was left held.
    fn release_turn(&mut self, ended: bool, outstanding: Vec<JoinHandle<()>>) -> bool {
        if let Some(mut watcher) = self.watcher.take() {
            watcher.stop();
            self.withdrawn |= watcher.withdrawn();
        }
        self.unconfirmed |= !ended;
        let record = self.turn_record.take();
        let Some(lock) = self.turn_lock.take() else {
            return !self.unconfirmed && outstanding.is_empty();
        };
        if self.unconfirmed {
            // The record stays where it is; only an acknowledgement removes
            // it.
            drop(record);
            hold_until_exit(lock);
            return false;
        }
        // The record goes before the lock, so a revocation that takes the
        // lock next finds none. One that cannot be removed stays and fails
        // every later wait until acknowledged.
        let cleared = record.is_none_or(|record| record.clear().is_ok());
        if outstanding.is_empty() {
            drop(lock);
            cleared
        } else {
            hold_until_ended(lock, outstanding);
            false
        }
    }

    fn close_channel(&mut self) {
        self.accept_cancel.cancel();
        let shutdown = self
            .shutdown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(shutdown) = shutdown {
            // A failed shutdown leaves the reads to the tree's end, which
            // closes the relay's side.
            let _ = shutdown.shutdown();
        }
        self.writer = None;
    }

    /// Drop the queue, which wakes a reader blocked on it, then wait for
    /// every worker for at most [`WORKER_GRACE`].
    fn stop_workers(&mut self) -> WorkersStopped {
        self.events = None;
        let deadline = deadline_after(WORKER_GRACE);
        let mut stopped = WorkersStopped {
            panicked: false,
            outstanding: Vec::new(),
        };
        for worker in std::mem::take(&mut self.workers) {
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(POLL);
            }
            if worker.is_finished() {
                // A worker that panicked has ended too.
                stopped.panicked |= worker.join().is_err();
            } else {
                // Its reads and writes were already unblocked, so it ends
                // with the tree; the turn's lock waits for it.
                stopped.outstanding.push(worker);
            }
        }
        stopped
    }
}

/// How the workers of one session ended.
struct WorkersStopped {
    /// Whether a worker that ended had panicked.
    panicked: bool,
    /// The workers still running after [`WORKER_GRACE`].
    outstanding: Vec<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The turn locks of trees whose end could not be confirmed. They stay held
/// until this process exits, so no revocation reports those turns ended; the
/// record each one left keeps failing revocations after that.
static UNCONFIRMED_TURNS: Mutex<Vec<File>> = Mutex::new(Vec::new());

/// Keep `lock` held until this process exits.
fn hold_until_exit(lock: File) {
    UNCONFIRMED_TURNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(lock);
}

/// Keep `lock` held until every one of `workers` ended, on a thread that
/// joins them and then releases it, or until this process exits when that
/// thread cannot start. The thread is the workers' supervisor and needs none
/// of its own: it holds nothing but the lock, which this process's exit
/// releases together with every thread.
fn hold_until_ended(lock: File, workers: Vec<JoinHandle<()>>) {
    let (sender, received) = mpsc::channel::<(File, Vec<JoinHandle<()>>)>();
    let spawned = thread::Builder::new()
        .name("claude-code-turn-end".into())
        .spawn(move || {
            if let Ok((lock, workers)) = received.recv() {
                for worker in workers {
                    // A worker that panicked has ended too.
                    let _ended = worker.join();
                }
                drop(lock);
            }
        });
    // The lock goes to the thread only once it runs, so a failed start
    // cannot release it.
    let held = (lock, workers);
    let unsent = match spawned {
        Ok(_supervisor) => sender.send(held).err().map(|unsent| unsent.0),
        Err(_) => Some(held),
    };
    if let Some((lock, _workers)) = unsent {
        hold_until_exit(lock);
    }
}

/// The child a launch started, or the turn's failure. A tree that started
/// and could not be stopped is no child of the session, so the session
/// records its end as unconfirmed itself: it then keeps the turn's lock
/// until this process exits and its record after that.
fn spawned(
    session: &mut Session,
    started: Result<StreamingChild, ProcessError>,
) -> Result<StreamingChild, ProviderError> {
    started.map_err(|error| {
        session.unconfirmed |= matches!(error, ProcessError::CleanupUnconfirmed(_));
        failure(ClaudeCodeFailure::Failed)
    })
}

/// Where a relay write's writer is between its I/O and its result, for
/// tests that move a deadline or a cancellation to that point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WritePoint {
    /// The I/O returned; the writer has not yet sampled the cancellation and
    /// the deadline.
    BeforeRecord,
    /// The writer recorded the write's end.
    AfterRecord,
}

/// Run `write` on this thread while a watcher calls `stop`, once, when
/// `cancel` is set or `expired` turns `true` before the write is recorded
/// finished. The write is recorded finished after `write` returns, so one
/// whose bytes went out but that is recorded only after the stop still fails.
/// A write already cancelled or expired never starts. Once `write` returns,
/// the writer samples both once, just before the record, and that one sample
/// decides: a write sampled late is stopped and fails, also when the watcher
/// never saw it late, and one sampled in time is recorded finished unless the
/// watcher stopped it first. A cancellation or a deadline after that sample
/// does not fail a write recorded in time. Every write refused for either
/// reason calls `stop` before this returns. `observe` is told each
/// [`WritePoint`]; tests use it to move a deadline there, and every other
/// caller passes nothing.
///
/// # Errors
///
/// [`ProviderError::Cancelled`] or [`ClaudeCodeFailure::Failed`] when the
/// write was refused or stopped for a cancellation or its expiry,
/// [`ClaudeCodeFailure::Failed`] when the watcher cannot start, and
/// [`ClaudeCodeFailure::Protocol`] when `write` failed.
fn raced_write(
    write: impl FnOnce() -> io::Result<()>,
    cancel: &CancelToken,
    expired: impl Fn() -> bool + Sync,
    stop: impl Fn() + Sync,
    mut observe: impl FnMut(WritePoint),
) -> Result<(), ProviderError> {
    let refused = || {
        if cancel.is_cancelled() {
            ProviderError::Cancelled
        } else {
            failure(ClaudeCodeFailure::Failed)
        }
    };
    if cancel.is_cancelled() || expired() {
        stop();
        return Err(refused());
    }
    let race = WriteRace::new();
    let (written, finished) = thread::scope(|scope| {
        let (race, expired, stop) = (&race, &expired, &stop);
        let watcher = thread::Builder::new()
            .name("claude-code-write".into())
            .spawn_scoped(scope, move || {
                while race.running() {
                    if cancel.is_cancelled() || expired() {
                        // Only a write still running is stopped.
                        if race.stop() {
                            stop();
                        }
                        return;
                    }
                    thread::park_timeout(POLL);
                }
            });
        let Ok(watcher) = watcher else {
            return Err(failure(ClaudeCodeFailure::Failed));
        };
        let written = write();
        observe(WritePoint::BeforeRecord);
        // One sample, taken just before the record, decides the write. A
        // late one stops the write here, also when the watcher never saw it;
        // the stop is called once, by whichever of the two recorded it.
        let late = cancel.is_cancelled() || expired();
        let finished = if late {
            if race.stop() {
                stop();
            }
            false
        } else {
            race.finish()
        };
        observe(WritePoint::AfterRecord);
        watcher.thread().unpark();
        Ok((written, finished))
    })?;
    if !finished {
        return Err(refused());
    }
    written.map_err(|_| failure(ClaudeCodeFailure::Protocol))
}

/// Which of a relay write and its watcher ended the write: the writer by
/// finishing it, or the watcher by stopping it. The first to record wins,
/// so a write recorded as finished is never reported as stopped.
struct WriteRace(AtomicU8);

impl WriteRace {
    const RUNNING: u8 = 0;
    const FINISHED: u8 = 1;
    const STOPPED: u8 = 2;

    const fn new() -> Self {
        Self(AtomicU8::new(Self::RUNNING))
    }

    fn running(&self) -> bool {
        self.0.load(Ordering::SeqCst) == Self::RUNNING
    }

    /// Record the write as finished. `false` when the watcher stopped it
    /// first.
    fn finish(&self) -> bool {
        self.record(Self::FINISHED)
    }

    /// Record the write as stopped. `false` when it already finished.
    fn stop(&self) -> bool {
        self.record(Self::STOPPED)
    }

    fn record(&self, end: u8) -> bool {
        self.0
            .compare_exchange(Self::RUNNING, end, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// `now + limit`, or now when that does not fit.
fn deadline_after(limit: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(limit).unwrap_or(now)
}

/// The length of `text` in bytes, as the budgets count it.
fn byte_count(text: &str) -> u64 {
    u64::try_from(text.len()).unwrap_or(u64::MAX)
}

/// A per-turn folder under the runtime folder, removed when dropped.
struct TurnFolder {
    path: PathBuf,
    /// The relay socket's private folder on Linux, created only for a turn
    /// that offers tools and removed with the turn.
    socket_folder: Option<PathBuf>,
}

impl TurnFolder {
    fn create(runtime: &Path) -> Result<Self, ProviderError> {
        let name = random_hex(16)?;
        let path = runtime.join("turns").join(name);
        // The creation is exclusive, so an existing entry is never adopted
        // and removed; once it succeeds the guard owns the folder, and a
        // later failure removes it.
        fs::create_dir_all(runtime.join("turns"))
            .and_then(|()| fs::create_dir(&path))
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        let folder = Self {
            path,
            socket_folder: None,
        };
        fs::create_dir(folder.path.join("work")).map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(folder.path.join("tmp"))
                .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        }
        Ok(folder)
    }

    /// Create the relay socket's private folder, mode `0700`, under
    /// [`socket_parent`]. The name is random and the creation exclusive, so
    /// an entry that already exists fails the turn rather than being reused.
    #[cfg(target_os = "linux")]
    fn create_socket_folder(&mut self) -> Result<PathBuf, ProviderError> {
        use std::os::unix::fs::DirBuilderExt;
        let path = socket_parent(std::env::var_os("XDG_RUNTIME_DIR"))
            .join(format!("{SOCKET_FOLDER_PREFIX}{}", random_hex(8)?));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| failure(ClaudeCodeFailure::Failed))?;
        self.socket_folder = Some(path.clone());
        Ok(path)
    }
}

impl Drop for TurnFolder {
    fn drop(&mut self) {
        // Best effort: a leftover folder holds only this turn's prompt, and a
        // leftover socket folder at most a socket nobody accepts on.
        let _ = fs::remove_dir_all(&self.path);
        if let Some(socket_folder) = &self.socket_folder {
            let _ = fs::remove_dir_all(socket_folder);
        }
    }
}

/// The relay socket's file name.
#[cfg(target_os = "linux")]
const SOCKET_NAME: &str = "mcp.sock";
/// The start of the relay socket folder's name; 16 random hex digits follow.
#[cfg(target_os = "linux")]
const SOCKET_FOLDER_PREFIX: &str = "omgcode-";
/// The longest socket parent that leaves the socket path within the 107
/// bytes `sun_path` holds: the parent, a separator, the folder name, a
/// separator, and [`SOCKET_NAME`].
#[cfg(target_os = "linux")]
const MAX_SOCKET_PARENT_BYTES: usize =
    107 - 1 - SOCKET_FOLDER_PREFIX.len() - 16 - 1 - SOCKET_NAME.len();

/// The folder that holds the relay socket's folder on Linux: the canonical
/// `runtime` folder (`XDG_RUNTIME_DIR`) when it is absolute, short enough,
/// and UTF-8, or `/tmp`. The relay configuration is JSON, so a socket path
/// that is not UTF-8 cannot reach the relay. The application data folder is
/// not used, because a socket path beneath it does not fit in `sun_path`.
#[cfg(target_os = "linux")]
fn socket_parent(runtime: Option<OsString>) -> PathBuf {
    runtime
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .and_then(|path| fs::canonicalize(path).ok())
        .filter(|path| {
            path.to_str()
                .is_some_and(|text| text.len() <= MAX_SOCKET_PARENT_BYTES)
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Bind the channel the relay connects to.
fn bind_listener(
    turn: &mut TurnFolder,
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
            directory: turn.create_socket_folder()?,
            name: OsString::from(SOCKET_NAME),
        }
    };
    omgcode_platform::ipc::LocalListener::bind(target)
        .map_err(|_| failure(ClaudeCodeFailure::Failed))
}

#[cfg(all(test, target_os = "linux"))]
mod socket_tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    use super::{MAX_SOCKET_PARENT_BYTES, SOCKET_FOLDER_PREFIX, SOCKET_NAME, socket_parent};

    #[test]
    fn the_longest_accepted_parent_leaves_a_socket_path_of_107_bytes() {
        let parent = format!("/{}", "p".repeat(MAX_SOCKET_PARENT_BYTES - 1));
        let socket = PathBuf::from(parent)
            .join(format!("{SOCKET_FOLDER_PREFIX}{}", "0".repeat(16)))
            .join(SOCKET_NAME);
        assert_eq!(socket.as_os_str().len(), 107);
    }

    #[test]
    fn an_unset_relative_or_missing_runtime_folder_falls_back_to_tmp() {
        let tmp = PathBuf::from("/tmp");
        assert_eq!(socket_parent(None), tmp);
        assert_eq!(socket_parent(Some(OsString::from("relative/run"))), tmp);
        assert_eq!(
            socket_parent(Some(OsString::from("/nonexistent/omgcode-runtime"))),
            tmp
        );
    }

    #[test]
    fn a_runtime_folder_too_long_for_the_socket_falls_back_to_tmp() {
        let base = tempfile::tempdir().expect("base");
        let mut long = fs::canonicalize(base.path()).expect("canonical");
        while long.as_os_str().len() <= MAX_SOCKET_PARENT_BYTES {
            long.push("long-runtime-folder");
        }
        fs::create_dir_all(&long).expect("long folder");
        assert_eq!(
            socket_parent(Some(long.into_os_string())),
            PathBuf::from("/tmp")
        );
    }

    #[test]
    fn a_runtime_folder_that_is_not_utf8_falls_back_to_tmp() {
        use std::os::unix::ffi::OsStringExt;

        // Beneath `/tmp` itself, not `TMPDIR`, which can be too long for
        // the socket path on its own and would hide the encoding check.
        let base = tempfile::Builder::new()
            .prefix("enc-")
            .tempdir_in("/tmp")
            .expect("base");
        let mut name = b"run-".to_vec();
        name.push(0xFF);
        let folder = fs::canonicalize(base.path())
            .expect("canonical")
            .join(OsString::from_vec(name));
        fs::create_dir(&folder).expect("folder");
        assert!(folder.as_os_str().len() <= MAX_SOCKET_PARENT_BYTES);
        assert_eq!(
            socket_parent(Some(folder.into_os_string())),
            PathBuf::from("/tmp")
        );
    }

    #[test]
    fn a_short_runtime_folder_is_used_canonically() {
        let base = tempfile::tempdir().expect("base");
        let canonical = fs::canonicalize(base.path()).expect("canonical");
        let link = canonical.join("link");
        std::os::unix::fs::symlink(&canonical, &link).expect("link");
        assert!(link.as_os_str().len() <= MAX_SOCKET_PARENT_BYTES);
        assert_eq!(socket_parent(Some(link.into_os_string())), canonical);
    }
}

/// Accept the relay, publish the channel's shutdown, check its nonce, then
/// forward its lines. Every send fails once the turn dropped its queue, and
/// a failed send ends the worker.
fn spawn_acceptor(
    listener: omgcode_platform::ipc::LocalListener,
    nonce: String,
    cancel: CancelSignal,
    slot: ShutdownSlot,
    sender: SyncSender<Event>,
) -> Result<JoinHandle<()>, ProviderError> {
    thread::Builder::new()
        .name("claude-code-mcp".into())
        .spawn(move || {
            let accepted = listener
                .accept(deadline_after(CONNECT_WINDOW), &cancel)
                .and_then(|stream| stream.split());
            let Ok((reader, writer, shutdown)) = accepted else {
                let _ = sender.send(Event::McpFailed);
                return;
            };
            {
                // `Session::close_channel` sets `cancel` before it takes the
                // slot, so under the lock either it sees this handle or this
                // worker sees the cancellation.
                let mut published = slot.lock().unwrap_or_else(PoisonError::into_inner);
                if cancel.is_cancelled() {
                    let _ = shutdown.shutdown();
                    return;
                }
                *published = Some(shutdown.clone());
            }
            let mut lines = BoundedLines::with_limit(reader, MAX_MCP_LINE_BYTES);
            let presented = match lines.next() {
                Some(Ok(line)) => line,
                _ => Vec::new(),
            };
            if !constant_time_eq(&presented, nonce.as_bytes()) {
                // The turn fails on `McpFailed`, which also ends the tree.
                let _ = shutdown.shutdown();
                let _ = sender.send(Event::McpFailed);
                return;
            }
            if sender.send(Event::McpConnected(Box::new(writer))).is_err() {
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

#[cfg(test)]
mod tests;
