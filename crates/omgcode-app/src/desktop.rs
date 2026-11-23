//! The front-end-neutral desktop service (ADR-0044, ADR-0045, ADR-0046).
//!
//! [`DesktopService`] is what a thin shell adapter calls, one method per
//! command, from blocking tasks. It grants no authority. Every decision that
//! matters (workspace preferences, tool approvals, checkpointed edits,
//! confinement, journal validation) is made by `omgcode-core`; this module
//! orders calls to core, bounds them, and shapes their results for the
//! interface.
//!
//! # What the service never does
//!
//! - It accepts no filesystem path from a caller. Sessions are addressed by
//!   UUID, workspaces by their registered identifier, approvals by identifier.
//!   A journal path is resolved only through `SessionIndex::journal_path`.
//! - It returns no journal path and no absolute path except workspace folder
//!   display text, which is escaped by core's `display_safe_line`.
//! - It never returns, stores in a response, or logs the local server token.
//!   The token is resolved for one turn from the operator-chosen
//!   [`TokenSource`] (the environment, a provider profile's stored
//!   credential, ADR-0047, or the active connection chosen in the `settings`
//!   window, ADR-0064), held in a zeroing type with a redacted `Debug`,
//!   removed from every recorded and returned message, and dropped when the
//!   turn thread ends. Nothing is cached between turns. The `main` window
//!   cannot choose or see the source, and no window sees the token.
//!
//! # State and locks
//!
//! The service state is shared by the service and by each running chat turn's
//! thread through one `Arc`. It holds:
//!
//! | field | guards |
//! | --- | --- |
//! | `index` | the one `SessionIndex` connection and its refresh bookkeeping |
//! | `turns` | the table of chat turns, their cancel tokens and thread handles |
//! | `reviews` | outstanding single-use approval review tokens |
//! | `events` | the one chat event subscriber and the running turns' previews |
//! | `pending_cache` | an atomic copy of the last pending-approval count read |
//! | `session_choices` | the read-modify-write of `session-connections.json` (ADR-0071) |
//!
//! **Lock order** when two are ever needed: `turns`, then `index`, then
//! `reviews`. The code takes one at a time. The `events` lock is never held
//! together with the `turns` lock (where both are needed, `turns` is taken and
//! released first) and never while `index` or `reviews` is taken. It is held
//! while a subscriber sink runs, so a sink must not block or call the service
//! (see [`ChatEventSink`]). No service lock and no open
//! control store is held across provider I/O, a chat turn, a journal
//! inspection, a thread join, or a sleep. The one deliberate exception is the
//! `index` lock during a refresh: a refresh needs `&mut SessionIndex`, so it
//! holds the lock for at most two seconds plus one chunk of 200 journal
//! inspections. A poisoned lock never panics and is never ignored: the call
//! returns `DesktopError` code `internal`.
//!
//! # Chat turns
//!
//! `chat_send` validates everything that can be checked without running the
//! turn, reserves a slot in the table under the `turns` lock, and starts one
//! named thread (`omgcode-turn`). The thread owns the journal handle, the
//! clients, and the token for the whole turn. It opens the journal, runs the
//! turn, drops the journal (which releases the writer lock), refreshes that
//! one session in the index, and only then records the outcome in its slot and
//! pushes `finished` to the event subscriber. It does not hold the `turns` lock while the turn runs. At most two turns run at
//! once.
//!
//! The slot keeps the thread's `JoinHandle`. `chat_status`, `chat_send`, and
//! the session commands reap finished threads: they take the handle out of the
//! slot under the lock, join it after releasing the lock, and record a thread
//! that panicked as `failed` with `internal`. Finished results stay until the
//! next `chat_send` for that session; at most 64 finished entries are kept and
//! the oldest is evicted first.
//!
//! Cancellation is cooperative. A turn hands its `CancelToken` to the slot when
//! it streams, or when it is tool-enabled. A streamed model request watches the
//! token and stops within about 25 ms (ADR-0050). A text-only turn whose
//! request is not streamed has no token, so a cancel request on it changes
//! nothing and the status stays `running`.
//!
//! # Shutdown
//!
//! [`DesktopService::shutdown`] requests cancellation of every running turn
//! and polls `JoinHandle::is_finished` until the grace period ends. A streamed
//! request is interrupted, but a request that is not streamed cannot be, so
//! such a turn can outlast the grace period. Shutdown then returns without
//! waiting; the abandoned thread leaves
//! its journal with an open turn, which the operator can close later with the
//! interrupt command or the trusted CLI. After shutdown no new turn starts.

mod approvals;
mod chat;
mod data_folder;
mod error;
mod events;
mod instructions;
mod preferences;
mod review_tokens;
mod session_connections;
mod sessions;
mod settings;
mod settings_changes;
mod settings_chat;
mod state;
mod turns;
mod wire;
mod workspaces;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use omgcode_core::control::ProviderKind;
use omgcode_core::session_index::SessionIndex;
use uuid::Uuid;

pub use self::error::DesktopError;
pub use self::events::ChatEventSink;
use self::events::EventHub;
use self::review_tokens::ReviewTokens;
pub use self::settings_changes::{Confirmation, FolderPurpose, SettingsPrompt};
use self::state::IndexState;
pub use self::turns::ShutdownReport;
use self::turns::TurnTable;
pub use self::wire::{
    ApiKeySetRequest, ApiKeyText, ApprovalDecideRequest, ApprovalDetail, ApprovalField,
    ApprovalList, ApprovalOperationDto, ApprovalRequest, ApprovalStateDto, ApprovalView,
    ApprovalsSummary, Binding, BranchState, BranchView, ChatBudgetsDto, ChatBudgetsSaveRequest,
    ChatDefaults, ChatDefaultsView, ChatEvent, ChatModels, ChatOutcome, ChatPreferencesDto,
    ChatPreferencesSaveRequest, ChatSendRequest, ChatSettingsDto, ChatState, ChatStatus,
    ComposerPreferencesDto, ConnectionAddRequest, ConnectionChoice, ConnectionChooseRequest,
    ConnectionOrigin, ConnectionRemoveRequest, ConnectionSelectRequest, ConnectionSourceView,
    ConnectionStatus, ConnectionStatusRequest, ConnectionView, ContainerGrantView, ContextMeterDto,
    Decision, EntryKind, MessageRoleDto, ModelChoice, NewConnectionKind, PackageProfileView,
    PermissionFlags, PermissionsChangeRequest, PreferencesSource, ProviderProfileView, SearchHit,
    SearchRequest, SearchResult, SearchRoleDto, SessionContext, SessionCreateRequest,
    SessionModelSelectRequest, SessionRequest, SessionSkills, SessionStatus, SessionSummary,
    SessionTree, SettingsOverview, SettingsRevisionRequest, SettingsScope, SkillOverrideView,
    SkillSettingView, SnippetPart, ToolOutcomeDto, ToolchainProfileView, ToolsMode,
    TranscriptEntry, TranscriptPage, TranscriptRequest, TreeRequest, TreeSort, TreeView,
    WorkspaceChoice, WorkspaceGroup, WorkspaceInstructionsRequest, WorkspaceList,
    WorkspaceRevisionRequest, WorkspaceSettingsView,
};
use crate::credential_source::resolve_connection_and_kind;
use crate::{
    AppError, ErrorKind, LmToken, LocalConnection, LocalEndpoint, TokenSource,
    resolve_local_endpoint,
};

/// Fixed settings of one desktop process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopConfig {
    /// Trusted application storage. It comes from the operator, never from the
    /// interface.
    pub data_root: PathBuf,
    /// The launch endpoint. With an environment or injected token source it
    /// is the chat endpoint for the process lifetime, and a chat request whose
    /// endpoint differs byte-for-byte is refused. With a profile token source
    /// it is the profile's recorded endpoint, resolved once at launch; each
    /// turn resolves the profile again and refuses the turn with
    /// `endpoint_changed` when the profile's endpoint no longer equals it.
    /// With the active source it is the default used while no connection is
    /// selected in `settings` (ADR-0064).
    pub endpoint: String,
    /// Where each chat turn gets its endpoint and optional local server token.
    /// It is fixed for the process lifetime and never comes from the
    /// interface; only the `settings` window can change what an active source
    /// selects, among recorded profiles.
    pub token_source: TokenSource,
    /// Whether chat turns send streamed model requests and push their
    /// provisional text to the subscriber (ADR-0049). When `false`, turns send
    /// non-streaming requests and only `finished` events are pushed.
    pub stream: bool,
}

/// A replacement token supplier for an embedder or a test that must avoid the
/// process environment and the credential store.
///
/// [`DesktopService::open`] resolves `DesktopConfig::token_source` with
/// [`crate::resolve_local_connection`]; [`DesktopService::open_with_token_source`] uses this
/// function instead and ignores the configured source.
pub type InjectedTokenFn = fn() -> Result<Option<String>, AppError>;

/// How a turn obtains its token.
#[derive(Clone)]
enum TokenResolver {
    /// The production path: [`crate::resolve_local_connection`].
    Source(TokenSource),
    /// A test or embedder supplier.
    Injected(InjectedTokenFn),
}

impl TokenResolver {
    /// Resolve the endpoint and token for one turn. Nothing is retained.
    ///
    /// `launch_endpoint` is the endpoint this process was started with. An
    /// environment or injected source uses it. A profile source takes the
    /// profile's recorded endpoint instead, which must still equal the launch
    /// endpoint: a profile removed and added again with another endpoint while
    /// the process ran is refused with `endpoint_changed` (ADR-0048), because
    /// the webview and the window show the launch value.
    fn resolve(
        &self,
        data_root: &Path,
        launch_endpoint: &str,
    ) -> Result<LocalConnection, AppError> {
        self.resolve_with_kind(data_root, launch_endpoint)
            .map(|(connection, _)| connection)
    }

    /// [`Self::resolve`] with the kind of the selected profile, from the same
    /// read of the settings store. `None` for the launch or default endpoint
    /// and for a source fixed at launch, as [`Self::current_endpoint`] reports.
    fn resolve_with_kind(
        &self,
        data_root: &Path,
        launch_endpoint: &str,
    ) -> Result<(LocalConnection, Option<ProviderKind>), AppError> {
        match self {
            Self::Source(source) => {
                let (connection, kind) =
                    resolve_connection_and_kind(data_root, &source.connection(launch_endpoint))?;
                // An active source may change between turns; `chat_send`
                // compares its endpoint with the one the window was shown.
                if source.is_fixed() && connection.endpoint != launch_endpoint {
                    return Err(AppError::new(
                        ErrorKind::Credential,
                        "endpoint_changed: the provider profile's endpoint changed since launch; restart the application",
                    ));
                }
                Ok((connection, if source.is_fixed() { None } else { kind }))
            }
            Self::Injected(supplier) => Ok((
                LocalConnection {
                    endpoint: launch_endpoint.to_owned(),
                    token: supplier()?.map(LmToken::new),
                },
                None,
            )),
        }
    }

    /// Whether the endpoint is fixed for the process. Only the active source
    /// changes between turns.
    const fn is_fixed(&self) -> bool {
        match self {
            Self::Source(source) => source.is_fixed(),
            Self::Injected(_) => true,
        }
    }

    /// The endpoint the next turn would use, without its token. A fixed
    /// source answers with the launch endpoint; the active source reads the
    /// settings store.
    fn current_endpoint(
        &self,
        data_root: &Path,
        launch_endpoint: &str,
    ) -> Result<LocalEndpoint, AppError> {
        match self {
            Self::Source(source) if !source.is_fixed() => {
                resolve_local_endpoint(data_root, &source.connection(launch_endpoint))
            }
            Self::Source(_) | Self::Injected(_) => Ok(LocalEndpoint {
                endpoint: launch_endpoint.to_owned(),
                profile: None,
            }),
        }
    }
}

/// The state shared by the service and its turn threads.
///
/// Owners: the [`DesktopService`] and one clone per running turn thread. The
/// last owner frees it. There is no reference cycle: the thread handles that
/// the table keeps do not own this `Arc`.
struct Inner {
    data_root: PathBuf,
    endpoint: String,
    token: TokenResolver,
    /// Whether turns send streamed requests ([`DesktopConfig::stream`]).
    stream: bool,
    index: Mutex<IndexState>,
    turns: Mutex<TurnTable>,
    reviews: Mutex<ReviewTokens>,
    /// The one chat event subscriber and the running turns' previews.
    events: Mutex<EventHub>,
    /// The last pending-approval count read, served when the settings store is
    /// busy. Advisory only; it authorizes nothing.
    pending_cache: AtomicU64,
    /// Set while one settings change waits for the operator or applies, so
    /// a second change is refused rather than opening a second dialog.
    settings_change: AtomicBool,
    /// The workspace the `settings` window is scoped to, set by the `main`
    /// window's **Workspace settings** button. Display state only: it grants
    /// and removes no access. Keep every critical section to one read or write.
    settings_scope: Mutex<Option<Uuid>>,
    /// Serializes the read-modify-write cycles of the per-session choices file
    /// (ADR-0071). Held for file input and output only.
    session_choices: Mutex<()>,
    /// Serializes the read-modify-write cycles of the chat preferences file
    /// and the workspace instruction files, which the `main` and `settings`
    /// windows both write (ADR-0080). Held for file input and output only.
    preferences: Mutex<()>,
}

/// The desktop command service. See the [module documentation](self).
///
/// It is `Send + Sync`; a shell may keep one in application state and call it
/// from blocking tasks.
pub struct DesktopService {
    inner: Arc<Inner>,
}

impl DesktopService {
    /// Open the service for `config`, creating the session index directory.
    ///
    /// The index is not refreshed here. The local server token is resolved from
    /// `config.token_source` per chat turn; nothing is read or cached now.
    ///
    /// # Errors
    ///
    /// `busy` when another process holds the index database, `internal` when
    /// the index location is unusable.
    pub fn open(config: DesktopConfig) -> Result<Self, DesktopError> {
        let resolver = TokenResolver::Source(config.token_source.clone());
        Self::open_with(config, resolver)
    }

    /// Open the service with an [`InjectedTokenFn`] that replaces
    /// `config.token_source`, so a test can avoid the process environment and
    /// the credential store.
    ///
    /// # Errors
    ///
    /// The same as [`open`](Self::open).
    pub fn open_with_token_source(
        config: DesktopConfig,
        supplier: InjectedTokenFn,
    ) -> Result<Self, DesktopError> {
        Self::open_with(config, TokenResolver::Injected(supplier))
    }

    fn open_with(config: DesktopConfig, token: TokenResolver) -> Result<Self, DesktopError> {
        let index = SessionIndex::open(&config.data_root)
            .map_err(|error| DesktopError::from_index(&error))?;
        Ok(Self {
            inner: Arc::new(Inner {
                data_root: config.data_root,
                endpoint: config.endpoint,
                token,
                stream: config.stream,
                index: Mutex::new(IndexState::new(index)),
                turns: Mutex::new(TurnTable::default()),
                reviews: Mutex::new(ReviewTokens::default()),
                events: Mutex::new(EventHub::default()),
                pending_cache: AtomicU64::new(0),
                settings_change: AtomicBool::new(false),
                settings_scope: Mutex::new(None),
                session_choices: Mutex::new(()),
                preferences: Mutex::new(()),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_can_be_shared_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DesktopService>();
    }
}
