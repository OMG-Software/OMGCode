//! Chat commands: defaults, model list, send, status, cancel, and shutdown.

use std::time::Duration;

use omgcode_core::context::DEFAULT_COMPACT_THRESHOLD;
use omgcode_core::session_index::IndexedState;
use uuid::Uuid;

use omgcode_core::skills::{MAX_SELECTED_SKILLS, SkillName};
use omgcode_core::turn::ApprovalWaitSlot;

use super::ChatEventSink;
use super::error::DesktopError;
use super::preferences;
use super::session_connections::SessionConnection;
use super::turns::{MAX_RUNNING_TURNS, ShutdownReport, SlotState, TurnJob, spawn_turn};
use super::wire::{
    ChatDefaults, ChatModels, ChatPreferencesDto, ChatPreferencesSaveRequest, ChatSendRequest,
    ChatSettingsDto, ChatState, ChatStatus, ModelChoice, PreferencesSource, SessionRequest,
    enter_to_send_default,
};
use super::{DesktopService, Inner};
use crate::{
    AppError, ChatClients, ChatSettings, LmToken, LocalConnection, validate_skill_selection,
};
use omgcode_core::control::ProviderKind;
use omgcode_core::provider::list_local_models;

/// Largest message accepted before core applies its own, smaller bound. It only
/// keeps an oversized request from being copied into a turn thread.
const MAX_MESSAGE_INPUT_BYTES: usize = 64 * 1024;
/// Largest custom-instruction text accepted before core applies its bound.
const MAX_INSTRUCTIONS_INPUT_BYTES: usize = 64 * 1024;
/// Output reserve of the default settings.
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1024;

impl DesktopService {
    /// The settings the interface starts from: the endpoint the next turn
    /// would use with the saved chat preferences, or with an empty model and
    /// the CLI's budgets when none are saved (ADR-0059 decision 8).
    ///
    /// The endpoint is the launch endpoint when it is fixed, and otherwise
    /// the active connection selected in `settings` (ADR-0064). When the
    /// active connection cannot be read now, the launch default is shown and
    /// `chat_send` reports the precise failure.
    ///
    /// A preferences file that cannot be read or fails validation is reported
    /// as [`PreferencesSource::Unreadable`] and the built-in defaults apply,
    /// so a damaged file never blocks chat. Saving again replaces it.
    pub fn chat_defaults(&self) -> ChatDefaults {
        let (preferences, source) = match preferences::load(&self.inner.data_root) {
            Ok(Some(saved)) => (saved, PreferencesSource::Saved),
            Ok(None) => (built_in_preferences(), PreferencesSource::Defaults),
            Err(_) => (built_in_preferences(), PreferencesSource::Unreadable),
        };
        let endpoint = self
            .inner
            .token
            .current_endpoint(&self.inner.data_root, &self.inner.endpoint)
            .map_or_else(|_| self.inner.endpoint.clone(), |current| current.endpoint);
        ChatDefaults {
            settings: ChatSettingsDto {
                endpoint,
                model: preferences.model,
                context_tokens: preferences.context_tokens,
                max_output_tokens: preferences.max_output_tokens,
                instructions: preferences.instructions,
                compact_threshold: preferences.compact_threshold,
                auto_compact: preferences.auto_compact,
            },
            preferences: source,
            enter_to_send: preferences.enter_to_send,
        }
    }

    /// List the models of the connection the session's next turn would use, so
    /// the composer offers a choice instead of a typed name (ADR-0066,
    /// ADR-0071).
    ///
    /// The connection is resolved for the session exactly as
    /// [`chat_send`](Self::chat_send) resolves it, its credential included, and the one request goes only to
    /// its validated endpoint, loopback or the fixed Ollama Cloud one. The list is untrusted provider text and
    /// grants nothing; `chat_send` still validates the chosen model. The call
    /// blocks for at most the core's ten-second deadline, so run it on a
    /// blocking task.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session, `session_connection_stale` for a
    /// recorded choice that no longer holds, the application layer's
    /// `configuration` and `credential` codes for an unusable connection, and `provider` for a refused endpoint, a server
    /// that cannot be reached in time, a failure status, or a list outside the
    /// supported schema.
    pub fn chat_models(&self, request: SessionRequest) -> Result<ChatModels, DesktopError> {
        let inner = &self.inner;
        inner.resolve_path(request.session_id)?;
        let SessionConnection {
            connection: LocalConnection { endpoint, token },
            kind,
            ..
        } = inner.resolve_session(request.session_id)?;
        let models = list_local_models(&endpoint, token.as_ref().map(LmToken::expose))
            .map_err(|error| DesktopError::from_app(&AppError::provider(error)))?;
        Ok(ChatModels {
            session_id: request.session_id,
            endpoint,
            kind: kind.map(ProviderKind::as_str),
            models: models
                .into_iter()
                .map(|model| ModelChoice {
                    id: model.id,
                    cloud_tag: model.cloud_tag,
                })
                .collect(),
        })
    }

    /// Save the chat preferences the composer starts from next time.
    ///
    /// They grant nothing; `chat_send` validates every value when it is used.
    /// The file is replaced atomically in application storage.
    ///
    /// # Errors
    ///
    /// `invalid_request` for a value over its bound, `preferences` when the
    /// file cannot be written.
    pub fn chat_preferences_save(
        &self,
        request: ChatPreferencesSaveRequest,
    ) -> Result<ChatDefaults, DesktopError> {
        preferences::save(&self.inner.data_root, &request.preferences)?;
        Ok(self.chat_defaults())
    }

    /// Start one chat turn and return at once with state `running`.
    ///
    /// Everything that can be checked without running the turn is checked
    /// first, so a bad endpoint, model, or session is refused synchronously.
    /// The turn then runs on its own thread; poll [`chat_status`](Self::chat_status).
    ///
    /// # Errors
    ///
    /// `invalid_request` (a fixed endpoint differs, empty or oversized
    /// message), `connection_changed` (the active connection's endpoint is
    /// not the one the window sent), `not_found`, `invalid_journal`, `busy`,
    /// `archived`, `turn_open`, `turn_running`, `too_many_turns`,
    /// `shutting_down`, `internal` (thread start failure), and the
    /// application layer's `provider`, `configuration`, and `credential`
    /// codes for unusable settings or token.
    pub fn chat_send(&self, request: ChatSendRequest) -> Result<ChatStatus, DesktopError> {
        let inner = &self.inner;
        let session_id = request.session_id;
        if inner.token.is_fixed() && request.settings.endpoint != inner.endpoint {
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
        let skills = selected_skills(&request.skills)?;
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
        // the only endpoint the turn uses. A fixed one equals the launch
        // endpoint, which was compared with the request above. The active
        // connection must equal what the window sent, so a change made in
        // `settings` after the window read it is never used unseen.
        let SessionConnection {
            connection: LocalConnection { endpoint, token },
            ..
        } = inner.resolve_session(session_id)?;
        if endpoint != request.settings.endpoint {
            return Err(DesktopError::connection_changed());
        }
        let settings = chat_settings(request.settings, endpoint);
        let clients = ChatClients::new(&settings, token.as_ref().map(LmToken::expose))
            .map_err(|error| DesktopError::from_app(&error))?;
        let approval_wait = ApprovalWaitSlot::new();
        let job = TurnJob {
            session_id,
            path,
            message: request.message,
            settings,
            token,
            clients,
            skills,
            approval_wait: approval_wait.clone(),
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
        turns.insert_running(session_id, handle, approval_wait);
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

/// The preferences that apply when none are saved: an empty model and the
/// CLI's budgets.
fn built_in_preferences() -> ChatPreferencesDto {
    ChatPreferencesDto {
        model: String::new(),
        context_tokens: 0,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        instructions: String::new(),
        compact_threshold: DEFAULT_COMPACT_THRESHOLD,
        auto_compact: false,
        enter_to_send: enter_to_send_default(),
    }
}

/// Parse the turn's skill names and check the selection shape (at most four,
/// none repeated). Whether each is registered and enabled for the session's
/// workspace is decided by the turn itself, which loads them from core.
fn selected_skills(names: &[String]) -> Result<Vec<SkillName>, DesktopError> {
    if names.len() > MAX_SELECTED_SKILLS {
        return Err(DesktopError::invalid_request(format!(
            "Select at most {MAX_SELECTED_SKILLS} skills for one turn."
        )));
    }
    let parsed = names
        .iter()
        .map(|name| {
            SkillName::new(name)
                .map_err(|_| DesktopError::invalid_request("A skill name is not valid."))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_skill_selection(&parsed).map_err(|error| DesktopError::from_app(&error))?;
    Ok(parsed)
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
