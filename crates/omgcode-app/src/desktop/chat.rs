//! Chat commands: defaults, send, status, cancel, and shutdown.

use std::time::Duration;

use omgcode_core::context::DEFAULT_COMPACT_THRESHOLD;
use omgcode_core::session_index::IndexedState;
use uuid::Uuid;

use super::ChatEventSink;
use super::error::DesktopError;
use super::turns::{MAX_RUNNING_TURNS, ShutdownReport, SlotState, TurnJob, spawn_turn};
use super::wire::{ChatSendRequest, ChatSettingsDto, ChatState, ChatStatus, SessionRequest};
use super::{DesktopService, Inner};
use crate::{ChatClients, ChatSettings, LmToken, LocalConnection};

/// Largest message accepted before core applies its own, smaller bound. It only
/// keeps an oversized request from being copied into a turn thread.
const MAX_MESSAGE_INPUT_BYTES: usize = 64 * 1024;
/// Largest custom-instruction text accepted before core applies its bound.
const MAX_INSTRUCTIONS_INPUT_BYTES: usize = 64 * 1024;
/// Output reserve of the default settings.
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1024;

impl DesktopService {
    /// The settings the interface starts from: the fixed endpoint, an empty
    /// model, and the CLI's budgets.
    pub fn chat_defaults(&self) -> ChatSettingsDto {
        ChatSettingsDto {
            endpoint: self.inner.endpoint.clone(),
            model: String::new(),
            context_tokens: 0,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            instructions: String::new(),
            compact_threshold: DEFAULT_COMPACT_THRESHOLD,
            auto_compact: false,
        }
    }

    /// Start one chat turn and return at once with state `running`.
    ///
    /// Everything that can be checked without running the turn is checked
    /// first, so a bad endpoint, model, or session is refused synchronously.
    /// The turn then runs on its own thread; poll [`chat_status`](Self::chat_status).
    ///
    /// # Errors
    ///
    /// `invalid_request` (endpoint differs, empty or oversized message),
    /// `not_found`, `invalid_journal`, `busy`, `archived`, `turn_open`,
    /// `turn_running`, `too_many_turns`, `shutting_down`, `internal` (thread
    /// start failure), and the application layer's `provider`, `configuration`,
    /// and `credential` codes for unusable settings or token.
    pub fn chat_send(&self, request: ChatSendRequest) -> Result<ChatStatus, DesktopError> {
        let inner = &self.inner;
        let session_id = request.session_id;
        if request.settings.endpoint != inner.endpoint {
            return Err(DesktopError::invalid_request(
                "The endpoint is fixed by the application.",
            ));
        }
        if request.message.trim().is_empty() {
            return Err(DesktopError::invalid_request("The message is empty."));
        }
        if request.message.len() > MAX_MESSAGE_INPUT_BYTES
            || request.settings.instructions.len() > MAX_INSTRUCTIONS_INPUT_BYTES
        {
            return Err(DesktopError::invalid_request("The request is too large."));
        }
        inner.reap()?;
        check_slot(inner, session_id)?;

        // A fresh read of this one journal, so archive and open-turn state is
        // current. The path comes only from the index.
        let (session, path) = inner.with_index(|state| {
            let session = state
                .index
                .refresh_session(session_id)
                .map_err(|error| DesktopError::from_index(&error))?
                .ok_or_else(|| DesktopError::not_found("session"))?;
            let path = state
                .index
                .journal_path(session_id)
                .map_err(|error| DesktopError::from_index(&error))?
                .ok_or_else(|| DesktopError::not_found("session"))?;
            Ok((session, path))
        })?;
        match session.state {
            IndexedState::Invalid => return Err(DesktopError::invalid_journal()),
            IndexedState::Busy => return Err(DesktopError::busy()),
            IndexedState::Ok => {}
        }
        if session.archived {
            return Err(DesktopError::archived());
        }
        if session.active_turn.is_some() {
            return Err(DesktopError::turn_open());
        }

        // Resolve the connection and build the clients now, so an unreadable
        // credential, a profile whose endpoint changed since launch, a bad
        // model, context, or endpoint is a synchronous error and no thread
        // starts. The connection is resolved again for every turn, so a
        // rotated credential applies to the next one. The resolved endpoint is
        // the only endpoint the turn uses; it equals the launch endpoint,
        // which was compared with the request above.
        let LocalConnection { endpoint, token } = inner
            .token
            .resolve(&inner.data_root, &inner.endpoint)
            .map_err(|error| DesktopError::from_app(&error))?;
        let settings = chat_settings(request.settings, endpoint);
        let clients = ChatClients::new(&settings, token.as_ref().map(LmToken::expose))
            .map_err(|error| DesktopError::from_app(&error))?;
        let job = TurnJob {
            session_id,
            path,
            message: request.message,
            settings,
            token,
            clients,
        };

        // Reserve the slot and start the thread under one lock hold, so two
        // racing calls cannot both pass the limits. Starting a thread does not
        // block; the thread waits for this lock only to store its cancel token.
        let mut turns = inner.lock_turns()?;
        if turns.is_closed() {
            return Err(DesktopError::shutting_down());
        }
        if turns.is_running(session_id) {
            return Err(DesktopError::turn_running());
        }
        if turns.running_count() >= MAX_RUNNING_TURNS {
            return Err(DesktopError::too_many_turns());
        }
        let handle = spawn_turn(inner, job)?;
        turns.insert_running(session_id, handle);
        drop(turns);
        Ok(status_of(
            session_id,
            Some(&SlotState::Running {
                cancel_requested: false,
                cancel: None,
            }),
        ))
    }

    /// The state of a session's chat turn.
    ///
    /// Reaps finished turn threads first. A session this service has no record
    /// of is `idle`. A finished result stays until the next `chat_send` for the
    /// session.
    ///
    /// # Errors
    ///
    /// `internal` when the service state is unavailable.
    pub fn chat_status(&self, request: SessionRequest) -> Result<ChatStatus, DesktopError> {
        self.inner.reap()?;
        self.read_status(request.session_id)
    }

    /// Request cooperative cancellation of a running turn.
    ///
    /// Every turn, text-only or tool-enabled, streamed or not, hands its
    /// cancel token to the slot before its first model request, including the
    /// compaction requests before a turn (ADR-0051). A request is then seen at
    /// once: the model or compaction request in flight stops within about
    /// 25 ms, and the turn ends as `failed` with the error code `cancelled`
    /// (ADR-0050). A tool-enabled turn also checks the token between model
    /// rounds and tool calls. A cancel that arrives before the token is acted
    /// on when the token is handed over, which ends the turn before it is
    /// recorded. `cancelling` is reported only once a cancel token exists.
    ///
    /// # Errors
    ///
    /// `internal` when the service state is unavailable.
    pub fn chat_cancel(&self, request: SessionRequest) -> Result<ChatStatus, DesktopError> {
        self.inner.lock_turns()?.request_cancel(request.session_id);
        self.inner.reap()?;
        self.read_status(request.session_id)
    }

    /// Register the one chat event subscriber, replacing any earlier one, and
    /// replay the preview of every running turn (ADR-0049).
    ///
    /// The earlier subscriber receives nothing more. The replay holds, for each
    /// running turn that has started a model request, `request_started`, the
    /// text so far as `delta` events, and `preview_truncated` when the bound was
    /// reached, in ascending session order. Later events follow in order.
    ///
    /// Events are sent while the service's event lock is held, so the sink must
    /// return quickly, must not block, and must not call back into the service.
    /// A sink that returns `false` is dropped; no event is sent until the next
    /// subscription. No event or preview text is logged.
    ///
    /// # Errors
    ///
    /// `internal` when the event state is unavailable (a poisoned lock). Turn
    /// threads meet the same condition by dropping events; their turns are not
    /// affected.
    pub fn chat_subscribe(&self, sink: ChatEventSink) -> Result<(), DesktopError> {
        self.inner.lock_events()?.subscribe(sink);
        Ok(())
    }

    /// Request cancellation of every running turn and wait up to `grace` for
    /// the turn threads to finish, polling for completion.
    ///
    /// Returns without waiting past `grace`. Every model and compaction
    /// request stops within about 25 ms (ADR-0051), so a turn waiting on the
    /// model finishes inside the grace period and leaves no open turn. A turn
    /// in a step that cannot be interrupted, such as a read, list, or edit tool
    /// call, can outlast the grace period: it is reported in `still_running`
    /// and may leave its journal with an open turn, which the operator can
    /// close later with the interrupt command. After shutdown, `chat_send` is
    /// refused.
    pub fn shutdown(&self, grace: Duration) -> ShutdownReport {
        self.inner.shutdown_turns(grace)
    }

    fn read_status(&self, session_id: Uuid) -> Result<ChatStatus, DesktopError> {
        let turns = self.inner.lock_turns()?;
        Ok(status_of(
            session_id,
            turns.slot(session_id).map(|slot| &slot.state),
        ))
    }
}

/// Refuse early when a turn for the session runs or the running limit is hit.
fn check_slot(inner: &Inner, session_id: Uuid) -> Result<(), DesktopError> {
    let turns = inner.lock_turns()?;
    if turns.is_closed() {
        return Err(DesktopError::shutting_down());
    }
    if turns.is_running(session_id) {
        return Err(DesktopError::turn_running());
    }
    if turns.running_count() >= MAX_RUNNING_TURNS {
        return Err(DesktopError::too_many_turns());
    }
    Ok(())
}

fn chat_settings(settings: ChatSettingsDto, endpoint: String) -> ChatSettings {
    ChatSettings {
        endpoint,
        model: settings.model,
        context_tokens: settings.context_tokens,
        max_output_tokens: settings.max_output_tokens,
        instructions: settings.instructions,
        compact_threshold: settings.compact_threshold,
        auto_compact: settings.auto_compact,
    }
}

/// The wire status of a slot, or `idle` when there is none.
fn status_of(session_id: Uuid, state: Option<&SlotState>) -> ChatStatus {
    let (state, outcome, error) = match state {
        None => (ChatState::Idle, None, None),
        Some(SlotState::Running {
            cancel_requested,
            cancel,
        }) => (
            if *cancel_requested && cancel.is_some() {
                ChatState::Cancelling
            } else {
                ChatState::Running
            },
            None,
            None,
        ),
        Some(SlotState::Finished(result)) => match result.as_ref() {
            Ok(outcome) => (ChatState::Completed, Some(outcome.clone()), None),
            Err(error) => (ChatState::Failed, None, Some(error.clone())),
        },
    };
    ChatStatus {
        session_id,
        state,
        outcome,
        error,
    }
}
