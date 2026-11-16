//! The table of chat turns and the thread that runs each one.
//!
//! A turn owns, for its whole life, the journal handle, the validated clients,
//! and the server token, all on its own thread. The table holds only what other
//! threads need: the state, the cancel token, and the `JoinHandle`. The table
//! lock is held for short bookkeeping and never while the turn runs, while a
//! thread is joined, or while the index is refreshed.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omgcode_core::session::SessionJournal;
use omgcode_core::turn::CancelToken;
use uuid::Uuid;

use super::Inner;
use super::error::DesktopError;
use super::wire::{ChatOutcome, ContextMeterDto, ToolsMode};
use crate::{
    ChatClients, ChatHooks, ChatSettings, ChatTurnInput, ChatTurnOutput, run_chat_turn_with_clients,
};

/// Most chat turns that run at once.
pub(super) const MAX_RUNNING_TURNS: usize = 2;
/// Most finished results kept; the oldest is evicted first.
pub(super) const MAX_FINISHED_TURNS: usize = 64;
/// Name of every turn thread.
pub(super) const TURN_THREAD_NAME: &str = "omgcode-turn";
/// How often shutdown checks whether turn threads have finished.
const SHUTDOWN_POLL: Duration = Duration::from_millis(10);
/// Message of a turn thread that ended without recording a result.
const STOPPED_UNEXPECTEDLY: &str = "The chat turn stopped unexpectedly.";

/// What happened to the threads that were in flight when shutdown began.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShutdownReport {
    /// Turn threads that were in flight at the start and have finished.
    pub finished: usize,
    /// Turn threads still running when the grace period ended. A provider
    /// request cannot be interrupted, so such a thread may still be waiting on
    /// the model; its journal is left with an open turn.
    pub still_running: usize,
}

/// The local server token, held only for the life of one turn.
///
/// It has no `Serialize` and a redacted `Debug`, so it cannot reach a
/// response, an error, or a log line by accident.
pub(super) struct LmToken(String);

impl LmToken {
    pub(super) const fn new(value: String) -> Self {
        Self(value)
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LmToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LmToken(<redacted>)")
    }
}

/// Where one session's turn stands.
pub(super) enum SlotState {
    /// The turn is running.
    Running {
        /// A cancel was requested (it may have no token to act on yet).
        cancel_requested: bool,
        /// The tool-enabled path's cancel token, once it exists.
        cancel: Option<CancelToken>,
    },
    /// The turn ended.
    Finished(Box<Result<ChatOutcome, DesktopError>>),
}

/// One session's slot.
pub(super) struct Slot {
    pub(super) state: SlotState,
    /// The turn thread, until it is joined.
    handle: Option<JoinHandle<()>>,
    /// Order in which finished slots were finished; smaller is older.
    finished_order: u64,
}

/// All slots, keyed by session.
#[derive(Default)]
pub(super) struct TurnTable {
    slots: HashMap<Uuid, Slot>,
    next_order: u64,
    /// Set by shutdown; no new turn starts afterwards.
    closed: bool,
}

impl TurnTable {
    pub(super) const fn is_closed(&self) -> bool {
        self.closed
    }

    pub(super) fn slot(&self, session_id: Uuid) -> Option<&Slot> {
        self.slots.get(&session_id)
    }

    pub(super) fn is_running(&self, session_id: Uuid) -> bool {
        self.slots
            .get(&session_id)
            .is_some_and(|slot| matches!(slot.state, SlotState::Running { .. }))
    }

    pub(super) fn running_ids(&self) -> HashSet<Uuid> {
        self.slots
            .iter()
            .filter(|(_, slot)| matches!(slot.state, SlotState::Running { .. }))
            .map(|(id, _)| *id)
            .collect()
    }

    pub(super) fn running_count(&self) -> usize {
        self.slots
            .values()
            .filter(|slot| matches!(slot.state, SlotState::Running { .. }))
            .count()
    }

    /// Slots whose thread has not been joined yet.
    fn unjoined_count(&self) -> usize {
        self.slots
            .values()
            .filter(|slot| slot.handle.is_some())
            .count()
    }

    /// Insert a running slot, replacing a finished one for the same session and
    /// evicting the oldest joined finished slots beyond the bound.
    pub(super) fn insert_running(&mut self, session_id: Uuid, handle: JoinHandle<()>) {
        self.slots.insert(
            session_id,
            Slot {
                state: SlotState::Running {
                    cancel_requested: false,
                    cancel: None,
                },
                handle: Some(handle),
                finished_order: 0,
            },
        );
        loop {
            let mut finished: Vec<(Uuid, u64)> = self
                .slots
                .iter()
                .filter(|(_, slot)| {
                    matches!(slot.state, SlotState::Finished(_)) && slot.handle.is_none()
                })
                .map(|(id, slot)| (*id, slot.finished_order))
                .collect();
            if finished.len() <= MAX_FINISHED_TURNS {
                return;
            }
            finished.sort_by_key(|(_, order)| *order);
            match finished.first() {
                Some((oldest, _)) => {
                    self.slots.remove(oldest);
                }
                None => return,
            }
        }
    }

    /// Record a cancel request. Returns without effect for a slot that is not running.
    pub(super) fn request_cancel(&mut self, session_id: Uuid) {
        if let Some(Slot {
            state:
                SlotState::Running {
                    cancel_requested,
                    cancel,
                },
            ..
        }) = self.slots.get_mut(&session_id)
        {
            *cancel_requested = true;
            if let Some(token) = cancel {
                token.cancel();
            }
        }
    }

    /// Request cancellation of every running turn.
    fn request_cancel_all(&mut self) {
        for slot in self.slots.values_mut() {
            if let SlotState::Running {
                cancel_requested,
                cancel,
            } = &mut slot.state
            {
                *cancel_requested = true;
                if let Some(token) = cancel {
                    token.cancel();
                }
            }
        }
    }

    /// Store the tool path's cancel token, acting on an earlier request.
    fn store_cancel_token(&mut self, session_id: Uuid, token: CancelToken) {
        if let Some(Slot {
            state:
                SlotState::Running {
                    cancel_requested,
                    cancel,
                },
            ..
        }) = self.slots.get_mut(&session_id)
        {
            if *cancel_requested {
                token.cancel();
            }
            *cancel = Some(token);
        }
    }

    /// End a running slot with `result`.
    fn finish(&mut self, session_id: Uuid, result: Result<ChatOutcome, DesktopError>) {
        let order = self.next_order;
        if let Some(slot) = self.slots.get_mut(&session_id)
            && matches!(slot.state, SlotState::Running { .. })
        {
            slot.state = SlotState::Finished(Box::new(result));
            slot.finished_order = order;
            self.next_order = order.saturating_add(1);
        }
    }
}

impl Inner {
    /// Join turn threads that have finished.
    ///
    /// Handles are taken out under the lock and joined after it is released. A
    /// thread that panicked leaves its slot `running`, so it is recorded as
    /// `failed` with `internal`.
    pub(super) fn reap(&self) -> Result<(), DesktopError> {
        let finished: Vec<(Uuid, JoinHandle<()>)> = {
            let mut turns = self.lock_turns()?;
            let ids: Vec<Uuid> = turns
                .slots
                .iter()
                .filter(|(_, slot)| slot.handle.as_ref().is_some_and(JoinHandle::is_finished))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| {
                    turns
                        .slots
                        .get_mut(&id)
                        .and_then(|slot| slot.handle.take())
                        .map(|handle| (id, handle))
                })
                .collect()
        };
        for (session_id, handle) in finished {
            // The thread already finished, so this join returns at once.
            if handle.join().is_err() {
                self.lock_turns()?.finish(
                    session_id,
                    Err(DesktopError::internal(STOPPED_UNEXPECTEDLY)),
                );
            }
        }
        Ok(())
    }

    /// Request cancellation of every turn, wait up to `grace`, and report.
    pub(super) fn shutdown_turns(&self, grace: Duration) -> ShutdownReport {
        let deadline = Instant::now().checked_add(grace);
        let in_flight = match self.lock_turns() {
            Ok(mut turns) => {
                turns.closed = true;
                turns.request_cancel_all();
                turns.unjoined_count()
            }
            // A poisoned table cannot be inspected or cancelled; nothing is
            // known to be finished.
            Err(_) => {
                return ShutdownReport {
                    finished: 0,
                    still_running: 0,
                };
            }
        };
        let mut remaining = in_flight;
        loop {
            if self.reap().is_err() {
                break;
            }
            remaining = self
                .lock_turns()
                .map_or(remaining, |turns| turns.unjoined_count());
            let expired = deadline.is_none_or(|limit| Instant::now() >= limit);
            if remaining == 0 || expired {
                break;
            }
            thread::sleep(SHUTDOWN_POLL);
        }
        ShutdownReport {
            finished: in_flight.saturating_sub(remaining),
            still_running: remaining,
        }
    }

    /// Hand the tool path's cancel token to the slot.
    fn hand_over_cancel_token(&self, session_id: Uuid, token: CancelToken) {
        // A poisoned table is reported to every caller of the service; the
        // turn simply cannot be cancelled and runs to its deadline.
        if let Ok(mut turns) = self.lock_turns() {
            turns.store_cancel_token(session_id, token);
        }
    }

    /// Record the end of a turn.
    fn record_finish(&self, session_id: Uuid, result: Result<ChatOutcome, DesktopError>) {
        // A poisoned table is reported to every caller of the service; the
        // thread has nothing left to do either way.
        if let Ok(mut turns) = self.lock_turns() {
            turns.finish(session_id, result);
        }
    }
}

/// Everything a turn thread owns.
pub(super) struct TurnJob {
    pub(super) session_id: Uuid,
    pub(super) path: PathBuf,
    pub(super) message: String,
    pub(super) settings: ChatSettings,
    pub(super) token: Option<LmToken>,
    pub(super) clients: ChatClients,
}

/// Start the turn thread.
///
/// The caller holds the `turns` lock; the thread does not need it until it
/// stores its cancel token, so it waits there briefly if it is fast.
pub(super) fn spawn_turn(inner: &Arc<Inner>, job: TurnJob) -> Result<JoinHandle<()>, DesktopError> {
    let worker = Arc::clone(inner);
    thread::Builder::new()
        .name(TURN_THREAD_NAME.to_owned())
        .spawn(move || run_turn(&worker, job))
        .map_err(|_| DesktopError::internal("The chat turn could not be started."))
}

/// The body of a turn thread.
fn run_turn(inner: &Inner, job: TurnJob) {
    let session_id = job.session_id;
    let result = execute(inner, &job);
    // The journal handle is gone, so the index can read the finished journal.
    // A failed refresh leaves the cache stale until the next listing refresh;
    // the journal remains the source of truth, so it is not a turn failure.
    drop(inner.with_index(|state| {
        state
            .index
            .refresh_session(session_id)
            .map_err(|error| DesktopError::from_index(&error))
    }));
    inner.record_finish(session_id, result);
}

/// Open the journal, run the turn, and drop the journal before returning.
fn execute(inner: &Inner, job: &TurnJob) -> Result<ChatOutcome, DesktopError> {
    let token = job.token.as_ref().map(LmToken::as_str);
    let mut journal =
        SessionJournal::open(&job.path).map_err(|error| DesktopError::from_session(&error))?;
    let session_id = job.session_id;
    let mut hand_over = |cancel: CancelToken| inner.hand_over_cancel_token(session_id, cancel);
    let mut hooks = ChatHooks {
        on_cancel_token: Some(&mut hand_over),
    };
    let result = run_chat_turn_with_clients(
        &mut journal,
        &inner.data_root,
        &ChatTurnInput {
            message: &job.message,
            settings: &job.settings,
            token,
        },
        &job.clients,
        &mut hooks,
    );
    drop(journal);
    match result {
        Ok(output) => Ok(outcome_of(&output)),
        Err(error) => {
            // Defence in depth: the application layer keeps the token out of
            // messages, and this removes it again if that ever failed.
            Err(DesktopError::from_app(&error).redacted(token))
        }
    }
}

/// Map a completed turn to the wire outcome.
fn outcome_of(output: &ChatTurnOutput) -> ChatOutcome {
    let context = output.context();
    let count =
        |value: Option<usize>| value.map_or(0, |count| u64::try_from(count).unwrap_or(u64::MAX));
    ChatOutcome {
        finish_reason: output.finish_reason().to_owned(),
        tools_mode: if output.tools_enabled() {
            ToolsMode::Enabled
        } else {
            ToolsMode::Disabled
        },
        tools_disabled_reason: output.disabled_reason().map(str::to_owned),
        rounds: count(output.rounds()),
        tool_calls: count(output.tool_calls()),
        withheld_tool_results: u64::try_from(output.withheld_tool_results()).unwrap_or(u64::MAX),
        context: Some(ContextMeterDto {
            estimated_tokens: context.estimated_input_tokens(),
            trigger_tokens: context.effective_trigger(),
            context_tokens: context.context_tokens(),
            compacted: context.compaction().is_some(),
        }),
    }
}
