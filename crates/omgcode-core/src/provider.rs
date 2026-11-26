pub mod chatgpt;
mod effort;
mod hosted;
mod models;
mod responses;
mod stream;

pub use effort::ReasoningEffort;
pub use hosted::{
    ANTHROPIC_ENDPOINT, DEEPSEEK_ENDPOINT, GOOGLE_ENDPOINT, META_ENDPOINT, MINIMAX_ENDPOINT,
    MOONSHOT_ENDPOINT, OLLAMA_CLOUD_ENDPOINT, OPENAI_ENDPOINT, OPENROUTER_ENDPOINT,
    OutputLimitField, TENCENT_ENDPOINT, THINKING_MACHINES_ENDPOINT, XAI_ENDPOINT, Z_AI_ENDPOINT,
};
pub(crate) use hosted::{
    disables_thinking, fixed_endpoint, fixed_takes_effort, matches_fixed, validate_custom_url,
};
pub use models::{ListedModel, MAX_LISTED_MODELS, list_local_models, list_subscription_models};
pub use responses::ResponseFailure;

use crate::prompt::{BASE_PROMPT, ModelMessage, ModelRole};
use crate::storage::parse_strict_json;
use crate::turn::{
    CancelToken, ModelError, ToolDefinition, ToolModel, ToolModelReply, ToolModelRequest,
};
use omgcode_platform::credentials::Zeroizing;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Largest encoded request body. The configured context budget, not this
/// cap, is meant to bound a request: 4 MiB holds a 1M-token window of text
/// (ADR-0099). A plan-route turn echoes each answered call, so 32 KiB write
/// arguments make requests grow past the old 256 KiB.
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// The whole of one streamed reply may hold at most this many bytes, Chat
/// Completions or Responses. Every token arrives in its own event with a
/// repeated envelope, so a 32 KiB tool-call argument (ADR-0099) streamed in
/// small pieces is several mebibytes; the buffered limit is too small.
const MAX_STREAM_BYTES: u64 = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The deadline of one plan-route Responses request (ADR-0093). The route
/// always streams, and reasoning models routinely take longer than the
/// thirty seconds a Chat Completions request has. It equals the default turn
/// timeout, which the turn checks between operations, so one slow request
/// can use a default turn's whole budget; the docs tell plan users to raise
/// the turn timeout for multi-step turns.
const RESPONSES_TIMEOUT: Duration = Duration::from_secs(120);
/// The most of a failed plan-route response body read to find its code.
const MAX_ERROR_BODY_BYTES: u64 = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a request looks at its cancel token while it waits. It is also
/// the longest a cancellation waits for the next wake-up.
const CANCEL_POLL: Duration = Duration::from_millis(25);
const MESSAGE_OVERHEAD_ESTIMATE: u64 = 256;
const PROMPT_HEADROOM_ESTIMATE: u64 = 512;
/// Upper bound on advertised tool definitions in one request.
const MAX_TOOL_DEFINITIONS: usize = 10;
/// Upper bound on the encoded bytes of the effort field, either shape: the
/// longest is `,"reasoning":{"effort":"minimal"}` (33 bytes).
const EFFORT_FIELD_BYTES: u64 = 48;
/// Upper bound on raw tool calls accepted from one response before validation.
/// The turn runtime applies its own, smaller per-round limit.
const MAX_RESPONSE_TOOL_CALLS: usize = 64;

/// A validated model endpoint: a literal loopback HTTP endpoint
/// ([`validate_base_url`], ADR-0048), one of the fixed hosted endpoints
/// (ADR-0071, ADR-0092), or an operator-typed HTTPS base URL that passes the
/// `openai_compatible` rule (ADR-0092). There is no other destination.
///
/// A hosted endpoint sends prompts, tool results, and command output off this
/// computer, so a client built on it requires a token. A token comes only from
/// a provider profile, whose kind decides which hosted rule its recorded
/// endpoint had to pass; the launch and environment paths refuse a hosted
/// endpoint (`cloud_needs_profile`).
///
/// The representation is private, so [`ModelEndpoint::parse`] is the only
/// constructor and every value has passed one of the rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelEndpoint(EndpointRule);

/// Which rule admitted a [`ModelEndpoint`]. Private: a caller cannot pair a
/// rule with a URL it did not pass.
#[derive(Clone, Debug, Eq, PartialEq)]
enum EndpointRule {
    /// A literal loopback IP, an explicit port, and the `/v1` path.
    Loopback(Url),
    /// An HTTPS base URL: a fixed provider endpoint, or one that passed the
    /// operator-typed rule. `field` names the output budget field it takes,
    /// `thinking_off` whether requests switch the provider's thinking off, and
    /// `effort` whether a request may carry a reasoning effort (ADR-0102).
    Hosted {
        url: Url,
        field: OutputLimitField,
        thinking_off: bool,
        effort: bool,
    },
}

impl ModelEndpoint {
    /// Validate endpoint text. A fixed hosted endpoint is an exact match
    /// (one trailing `/` allowed); any other `https://` text must pass the
    /// operator-typed rule, and anything else the loopback rule.
    ///
    /// # Errors
    ///
    /// `InvalidConfiguration` for any other text.
    pub fn parse(text: &str) -> Result<Self, ProviderError> {
        if let Some((url, field)) = fixed_endpoint(text) {
            return Ok(Self(EndpointRule::Hosted {
                url,
                field,
                thinking_off: disables_thinking(text),
                effort: fixed_takes_effort(text),
            }));
        }
        if text.starts_with("https://") {
            return validate_custom_url(text).map(|url| {
                Self(EndpointRule::Hosted {
                    url,
                    field: OutputLimitField::MaxTokens,
                    thinking_off: false,
                    // The operator's own server; its documentation cannot be
                    // checked here, and the server decides (ADR-0102).
                    effort: true,
                })
            });
        }
        validate_base_url(text).map(|url| Self(EndpointRule::Loopback(url)))
    }

    /// Whether this is a hosted endpoint, which needs a token and sends every
    /// request off this computer.
    pub const fn is_hosted(&self) -> bool {
        matches!(self.0, EndpointRule::Hosted { .. })
    }

    /// The host name or address with any non-default port, for a confirmation
    /// that says where requests go: `llm.example.org:8443` names a different
    /// service from `llm.example.org`. It matches the browser's `URL.host`,
    /// which the settings window shows. It is not a secret.
    pub fn host(&self) -> Option<String> {
        let url = self.url();
        let host = url.host_str()?;
        Some(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        })
    }

    /// The endpoint text as validated. It is not a secret.
    pub fn as_str(&self) -> &str {
        self.url().as_str()
    }

    const fn url(&self) -> &Url {
        match &self.0 {
            EndpointRule::Loopback(url) | EndpointRule::Hosted { url, .. } => url,
        }
    }

    /// The request field that carries the output budget.
    pub(crate) const fn output_field(&self) -> OutputLimitField {
        match self.0 {
            EndpointRule::Loopback(_) => OutputLimitField::MaxTokens,
            EndpointRule::Hosted { field, .. } => field,
        }
    }

    /// Whether requests send `"thinking": {"type": "disabled"}`, because the
    /// provider would otherwise require reasoning text this client does not
    /// keep (ADR-0092).
    pub(crate) const fn disables_thinking(&self) -> bool {
        match self.0 {
            EndpointRule::Loopback(_) => false,
            EndpointRule::Hosted { thinking_off, .. } => thinking_off,
        }
    }

    /// Whether a request to this endpoint may carry a reasoning effort
    /// (ADR-0102): every loopback server and operator-typed endpoint, and the
    /// fixed endpoints whose provider documents the field.
    pub const fn takes_reasoning_effort(&self) -> bool {
        match self.0 {
            EndpointRule::Loopback(_) => true,
            EndpointRule::Hosted { effort, .. } => effort,
        }
    }

    /// The URL of one resource below the base URL, such as `/models`. The
    /// suffix is appended to the base path, so `https://api.z.ai/api/paas/v4`
    /// gives `/api/paas/v4/models` and `http://127.0.0.1:1234/v1` gives
    /// `/v1/models`.
    fn with_suffix(&self, suffix: &str) -> Url {
        let mut url = self.url().clone();
        let path = format!("{}{suffix}", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url
    }

    /// Refuse a hosted endpoint that has no token, before any connection.
    fn check_token(&self, token: Option<&str>) -> Result<(), ProviderError> {
        if self.is_hosted() && token.is_none() {
            return Err(ProviderError::InvalidToken);
        }
        Ok(())
    }

    /// Whether requests must use HTTPS. The loopback rule admits only HTTP.
    const fn https_only(&self) -> bool {
        self.is_hosted()
    }
}

/// Explicit settings for one local LM Studio Chat Completions connection.
///
/// `context_tokens` is a required conservative budget supplied by the caller.
/// This adapter estimates input tokens from UTF-8 bytes and fixed overhead; it
/// does not claim to reproduce the selected model's tokenizer.
pub struct LocalModelConfig {
    /// A literal loopback HTTP URL with an explicit port and `/v1` path, or
    /// a hosted endpoint (see [`ModelEndpoint`]), which needs a token.
    pub base_url: String,
    /// The model identifier already available to the selected LM Studio server.
    pub model: String,
    /// Caller-validated context budget for this model, in tokens.
    pub context_tokens: u64,
    /// Maximum generated tokens for each request.
    pub max_output_tokens: u32,
}

/// A validated local LM Studio Chat Completions client.
///
/// It holds the validated settings only. Every request builds its own HTTP
/// client inside a single-thread runtime made for that request (ADR-0050,
/// ADR-0051), so no pooled connection outlives the runtime that drives it.
pub struct LocalModelClient {
    endpoint: Url,
    https_only: bool,
    output_field: OutputLimitField,
    thinking: Option<Thinking>,
    /// Whether the endpoint takes a reasoning effort (ADR-0102).
    takes_effort: bool,
    /// The effort every request sends; `None` sends no field.
    reasoning_effort: Option<ReasoningEffort>,
    model: String,
    context_tokens: u64,
    max_output_tokens: u32,
    authorization: Option<HeaderValue>,
    request_timeout: Duration,
    protocol: Protocol,
}

/// The wire protocol a client speaks.
enum Protocol {
    /// OpenAI Chat Completions with a fixed token or none.
    ChatCompletions,
    /// The ChatGPT plan route's Responses API (ADR-0093). The source is asked
    /// for a current access token before every request. It is shared by the
    /// chat and summary clients of one turn and dropped with the last of them;
    /// it holds no reference back to either client.
    Responses(Arc<dyn AccessTokenSource>),
}

impl fmt::Debug for LocalModelClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalModelClient")
            .field("model", &self.model)
            .field("context_tokens", &self.context_tokens)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("has_token", &self.authorization.is_some())
            .field(
                "protocol",
                &match self.protocol {
                    Protocol::ChatCompletions => "chat_completions",
                    Protocol::Responses(_) => "chatgpt_responses",
                },
            )
            .finish()
    }
}

/// Supplies the current access token of a signed-in ChatGPT account
/// (ADR-0093).
///
/// The implementation may wait for a lock and refresh the token over the
/// network, so it blocks. It must watch `cancel` and return `Cancelled` once
/// it is set. It is called outside any async runtime. The token zeroes
/// itself on drop and must never be logged or placed in an error.
pub trait AccessTokenSource: Send + Sync {
    /// Return an access token valid for at least the next few minutes.
    ///
    /// # Errors
    ///
    /// `SignInRequired` when the account must sign in again,
    /// `AccessTokenUnavailable` when the token cannot be read or refreshed
    /// now, and `Cancelled` for a set token.
    fn access_token(&self, cancel: &CancelToken) -> Result<Zeroizing<String>, ProviderError>;
}

/// A successful assistant reply and provider-reported usage, when available.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelReply {
    /// Plain assistant text. This value is untrusted model output.
    pub content: String,
    /// The provider finish reason (`stop` or `length`).
    pub finish_reason: String,
    /// Optional usage counters returned by the server.
    pub usage: Option<Usage>,
}

/// Optional token counters reported by the provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    /// Input tokens, when reported.
    pub prompt_tokens: Option<u64>,
    /// Generated tokens, when reported.
    pub completion_tokens: Option<u64>,
    /// Combined tokens, when reported.
    pub total_tokens: Option<u64>,
}

/// Safe failure categories for local model configuration and inference.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProviderError {
    /// The server URL, model, or explicit budgets are invalid.
    #[error("invalid local model configuration")]
    InvalidConfiguration,
    /// The optional bearer token cannot be represented safely in an HTTP header.
    #[error("invalid local model token")]
    InvalidToken,
    /// The serialized request exceeds the wire byte limit.
    #[error("local model request exceeds 4 MiB")]
    RequestTooLarge,
    /// The conservative request estimate exceeds the configured context budget.
    #[error("local model request exceeds the configured context budget")]
    ContextLimit,
    /// The request failed before a valid HTTP response arrived.
    #[error("local model transport failed")]
    Transport,
    /// The local server returned a non-success status.
    #[error("local model server returned HTTP status {0}")]
    HttpStatus(u16),
    /// The server response exceeds the byte limit.
    #[error("local model response exceeds 1 MiB")]
    ResponseTooLarge,
    /// The server response does not match the supported response schema.
    #[error("invalid local model response")]
    MalformedResponse,
    /// The server requested a tool or function call, which this adapter does not support.
    #[error("local model tool calls are not supported")]
    UnsupportedToolCall,
    /// A streamed response ended before its `[DONE]` marker, or a Responses
    /// stream before `response.completed`.
    #[error("local model stream ended before completion")]
    IncompleteStream,
    /// The ChatGPT plan route refused or ended the request with a documented
    /// code (ADR-0093). No provider text is kept.
    #[error("ChatGPT plan request failed: {0}")]
    ResponseFailed(ResponseFailure),
    /// The ChatGPT account must sign in again: it is signed out, or its
    /// refresh token was refused.
    #[error("ChatGPT sign-in is required: sign out and sign in again")]
    SignInRequired,
    /// The ChatGPT access token could not be read or refreshed now.
    #[error("the ChatGPT access token is unavailable")]
    AccessTokenUnavailable,
    /// The caller's cancel token stopped a request. Text a streamed request
    /// already delivered stays delivered, and no reply exists.
    #[error("local model request was cancelled")]
    Cancelled,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ModelMessage],
    stream: bool,
    #[serde(flatten)]
    limit: OutputLimit,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    /// Sent only when the operator chose an effort (ADR-0102).
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
}

/// The text request plus the advertised function tools.
#[derive(Serialize)]
struct ToolChatRequest<'a> {
    model: &'a str,
    messages: &'a [ModelMessage],
    stream: bool,
    #[serde(flatten)]
    limit: OutputLimit,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
    tools: &'a [WireTool<'a>],
    tool_choice: &'static str,
}

/// The output budget under the field name the endpoint takes. Exactly one of
/// the two is written.
#[derive(Serialize)]
struct OutputLimit {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
}

/// The provider's thinking-mode switch, sent only where the endpoint needs it
/// off (ADR-0092). It serializes as `{"type": "disabled"}`.
#[derive(Clone, Copy, Debug, Serialize)]
struct Thinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// One OpenAI-compatible function tool definition.
#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: &'a ToolDefinition,
}

impl LocalModelClient {
    /// Create a client for a literal loopback LM Studio endpoint.
    ///
    /// The constructor does not start LM Studio, load a model, or download one.
    /// It rejects hostnames, non-loopback addresses, redirects, proxies, and
    /// URLs that could route a credential or prompt to another destination.
    pub fn new(config: LocalModelConfig, token: Option<String>) -> Result<Self, ProviderError> {
        Self::with_timeout(config, token, REQUEST_TIMEOUT)
    }

    /// Build the same validated transport with a short deadline in unit tests.
    /// Production callers always use the fixed thirty-second deadline in `new`.
    fn with_timeout(
        config: LocalModelConfig,
        token: Option<String>,
        request_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let model_endpoint = ModelEndpoint::parse(&config.base_url)?;
        model_endpoint.check_token(token.as_deref())?;
        if config.model.is_empty()
            || config.model.len() > 256
            || config.model.chars().any(char::is_control)
            || config.context_tokens == 0
            || config.max_output_tokens == 0
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        let endpoint = model_endpoint.with_suffix("/chat/completions");

        let authorization = token.as_deref().map(bearer_header).transpose()?;

        Ok(Self {
            endpoint,
            https_only: model_endpoint.https_only(),
            output_field: model_endpoint.output_field(),
            thinking: model_endpoint
                .disables_thinking()
                .then_some(Thinking { kind: "disabled" }),
            takes_effort: model_endpoint.takes_reasoning_effort(),
            reasoning_effort: None,
            model: config.model,
            context_tokens: config.context_tokens,
            max_output_tokens: config.max_output_tokens,
            authorization,
            request_timeout,
            protocol: Protocol::ChatCompletions,
        })
    }

    /// Create a client for a signed-in ChatGPT account's plan (ADR-0093).
    ///
    /// `config.base_url` must be OpenAI's fixed endpoint, the one a
    /// subscription profile records. Every request goes to `{base}/responses`
    /// with an access token from `tokens`, which is asked again before each
    /// request. `max_output_tokens` is used only for the context estimate:
    /// the plan route refuses an output limit.
    ///
    /// # Errors
    ///
    /// `InvalidConfiguration` for any other endpoint or an invalid model or
    /// budget. Nothing is sent.
    pub fn chatgpt(
        config: LocalModelConfig,
        tokens: Arc<dyn AccessTokenSource>,
    ) -> Result<Self, ProviderError> {
        if !matches_fixed(&config.base_url, OPENAI_ENDPOINT) {
            return Err(ProviderError::InvalidConfiguration);
        }
        Self::responses_with_timeout(config, tokens, RESPONSES_TIMEOUT)
    }

    /// [`Self::chatgpt`] without the endpoint check, so unit tests can point
    /// the same client at a loopback fixture with a short deadline.
    fn responses_with_timeout(
        config: LocalModelConfig,
        tokens: Arc<dyn AccessTokenSource>,
        request_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let model_endpoint = ModelEndpoint::parse(&config.base_url)?;
        if config.model.is_empty()
            || config.model.len() > 256
            || config.model.chars().any(char::is_control)
            || config.context_tokens == 0
            || config.max_output_tokens == 0
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        Ok(Self {
            endpoint: model_endpoint.with_suffix("/responses"),
            https_only: model_endpoint.https_only(),
            output_field: model_endpoint.output_field(),
            model: config.model,
            context_tokens: config.context_tokens,
            max_output_tokens: config.max_output_tokens,
            authorization: None,
            // A Chat Completions switch (ADR-0092); the Responses body has no such field.
            thinking: None,
            // The Responses API takes `reasoning.effort` (ADR-0102).
            takes_effort: model_endpoint.takes_reasoning_effort(),
            reasoning_effort: None,
            request_timeout,
            protocol: Protocol::Responses(tokens),
        })
    }

    /// Whether this client speaks the ChatGPT plan route's Responses API.
    pub const fn is_chatgpt(&self) -> bool {
        matches!(self.protocol, Protocol::Responses(_))
    }

    /// The selected model identifier. This is descriptive, not an entitlement.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Send `effort` with every request of this client, or no effort field
    /// for `None` (ADR-0102). The value is not checked against the model:
    /// the server decides, and refuses a value its model does not take.
    ///
    /// # Errors
    ///
    /// `InvalidConfiguration` for an effort on an endpoint that takes none.
    /// Nothing is sent.
    pub fn with_reasoning_effort(
        mut self,
        effort: Option<ReasoningEffort>,
    ) -> Result<Self, ProviderError> {
        if effort.is_some() && !self.takes_effort {
            return Err(ProviderError::InvalidConfiguration);
        }
        self.reasoning_effort = effort;
        Ok(self)
    }

    /// The effort every request sends, or `None` when no field is sent.
    pub const fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    /// The encoded bytes the effort field adds to a request, counted in the
    /// size and context estimates.
    const fn effort_bytes(&self) -> u64 {
        if self.reasoning_effort.is_some() {
            EFFORT_FIELD_BYTES
        } else {
            0
        }
    }

    /// Send one bounded, non-streaming Chat Completions request.
    ///
    /// The adapter sends only model, role-tagged messages, `stream=false`, and
    /// `max_tokens`. It never sends tool definitions or hosted execution fields.
    /// Model output remains untrusted plain text and is never interpreted as an
    /// action by this adapter.
    ///
    /// `cancel` stops the request exactly as it stops
    /// [`Self::complete_streaming`] (ADR-0051): it is checked before anything
    /// is sent, while the response head is awaited, and while each piece of
    /// the body is awaited, at least every 25 ms. A set token drops the
    /// request, closing the connection, and the result is `Cancelled`; a body
    /// read in part is discarded. The method must not be called from inside
    /// another async runtime.
    ///
    /// # Errors
    ///
    /// A request refused before sending is `InvalidConfiguration`,
    /// `ContextLimit`, or `RequestTooLarge`. A connection, read, or deadline
    /// failure is `Transport`, a non-success status is `HttpStatus`, a body
    /// over 1 MiB is `ResponseTooLarge`, a reply outside the supported schema
    /// is `MalformedResponse` or `UnsupportedToolCall`, and a set `cancel` is
    /// `Cancelled`.
    pub fn complete(
        &self,
        messages: &[ModelMessage],
        cancel: &CancelToken,
    ) -> Result<ModelReply, ProviderError> {
        if let Protocol::Responses(tokens) = &self.protocol {
            let outcome =
                self.responses_request(tokens.as_ref(), messages, None, &mut |_| {}, cancel)?;
            return Ok(text_reply(outcome));
        }
        let body = self.text_body(messages, false)?;
        let bytes =
            self.on_request_runtime(body.len(), cancel, self.whole_exchange(body, cancel))?;
        parse_response(&bytes)
    }

    /// Send one bounded streaming request and pass assistant text to `on_text`.
    ///
    /// The request is the same as [`Self::complete`] with `stream` set to `true`
    /// and an `Accept: text/event-stream` header; `stream_options` is never
    /// sent. The response is read as server-sent events under the strict rules
    /// of ADR-0049. `on_text` receives each non-empty piece, in order, only
    /// after the payload that carried it passed validation. That text is
    /// provisional: if the stream fails later, the text already delivered stays
    /// delivered, this method returns the error, and no reply exists.
    ///
    /// The request runs on an async client inside a single-thread runtime made
    /// for this one call (ADR-0050). `cancel` is checked before anything is
    /// sent, while the response head is awaited, and while each piece of the
    /// body is awaited, at least every 25 ms. When it is set the request is
    /// dropped, which closes the connection, and the result is `Cancelled`.
    /// Dropping the request undoes no effect on the server. The method must not
    /// be called from inside another async runtime.
    ///
    /// # Errors
    ///
    /// Besides the errors of [`Self::complete`], a stream that ends before
    /// `[DONE]` is `IncompleteStream`, a response whose content type is not
    /// `text/event-stream` is `MalformedResponse`, a read error or the request
    /// deadline is `Transport`, and a set `cancel` is `Cancelled`.
    pub fn complete_streaming(
        &self,
        messages: &[ModelMessage],
        on_text: &mut dyn FnMut(&str),
        cancel: &CancelToken,
    ) -> Result<ModelReply, ProviderError> {
        if let Protocol::Responses(tokens) = &self.protocol {
            let outcome =
                self.responses_request(tokens.as_ref(), messages, None, on_text, cancel)?;
            return Ok(text_reply(outcome));
        }
        let body = self.text_body(messages, true)?;
        let outcome = self.on_request_runtime(
            body.len(),
            cancel,
            self.stream_exchange(body, stream::StreamMode::Text, on_text, cancel),
        )?;
        Ok(ModelReply {
            content: outcome.content,
            finish_reason: outcome.finish_reason,
            usage: outcome.usage,
        })
    }

    /// The output budget under the endpoint's field name.
    const fn output_limit(&self) -> OutputLimit {
        match self.output_field {
            OutputLimitField::MaxTokens => OutputLimit {
                max_tokens: Some(self.max_output_tokens),
                max_completion_tokens: None,
            },
            OutputLimitField::MaxCompletionTokens => OutputLimit {
                max_tokens: None,
                max_completion_tokens: Some(self.max_output_tokens),
            },
        }
    }

    /// Encode the text request with the chosen `stream` setting.
    fn text_body(
        &self,
        messages: &[ModelMessage],
        streaming: bool,
    ) -> Result<Vec<u8>, ProviderError> {
        self.check_messages(messages, self.effort_bytes())?;
        serde_json::to_vec(&ChatRequest {
            model: &self.model,
            messages,
            stream: streaming,
            limit: self.output_limit(),
            thinking: self.thinking,
            reasoning_effort: self.reasoning_effort,
        })
        .map_err(|_| ProviderError::MalformedResponse)
    }

    /// Send one bounded request that advertises the supplied function tools.
    ///
    /// Tool definitions are sent in the OpenAI-compatible `tools` field with
    /// `tool_choice` set to `auto`. The response is parsed strictly: duplicate
    /// keys, legacy `function_call` output, truncated tool calls, and more than
    /// 64 calls are refused. Returned tool calls stay raw and unvalidated; the
    /// caller must validate each one before any dispatch. This method executes
    /// nothing and never trusts provider metadata or role labels. `cancel`
    /// stops the request as it does for [`Self::complete`].
    pub fn complete_with_tools(
        &self,
        messages: &[ModelMessage],
        tools: &[ToolDefinition],
        cancel: &CancelToken,
    ) -> Result<ToolModelReply, ProviderError> {
        if let Protocol::Responses(tokens) = &self.protocol {
            let outcome = self.responses_request(
                tokens.as_ref(),
                messages,
                Some(tools),
                &mut |_| {},
                cancel,
            )?;
            return Ok(tool_reply(outcome));
        }
        let body = self.tool_body(messages, tools, false)?;
        let bytes =
            self.on_request_runtime(body.len(), cancel, self.whole_exchange(body, cancel))?;
        parse_tool_response(&bytes)
    }

    /// Send one bounded streaming request that advertises the supplied tools.
    ///
    /// This is [`Self::complete_with_tools`] with the stream rules of
    /// [`Self::complete_streaming`]. Tool calls arrive in pieces and are
    /// assembled by `index`; the returned calls hold only the members that were
    /// received and stay raw and unvalidated, so the caller's validation judges
    /// them exactly as it judges a non-streaming call. Only assistant text is
    /// passed to `on_text`; call names and arguments never are. `cancel` stops
    /// the request exactly as it does for [`Self::complete_streaming`], and a
    /// partly received tool call is discarded with it.
    pub fn complete_with_tools_streaming(
        &self,
        messages: &[ModelMessage],
        tools: &[ToolDefinition],
        on_text: &mut dyn FnMut(&str),
        cancel: &CancelToken,
    ) -> Result<ToolModelReply, ProviderError> {
        if let Protocol::Responses(tokens) = &self.protocol {
            let outcome =
                self.responses_request(tokens.as_ref(), messages, Some(tools), on_text, cancel)?;
            return Ok(tool_reply(outcome));
        }
        let body = self.tool_body(messages, tools, true)?;
        let outcome = self.on_request_runtime(
            body.len(),
            cancel,
            self.stream_exchange(body, stream::StreamMode::Tools, on_text, cancel),
        )?;
        Ok(ToolModelReply {
            content: outcome.content,
            tool_calls: outcome.tool_calls,
            finish_reason: outcome.finish_reason,
        })
    }

    /// Encode the tool request with the chosen `stream` setting.
    fn tool_body(
        &self,
        messages: &[ModelMessage],
        tools: &[ToolDefinition],
        streaming: bool,
    ) -> Result<Vec<u8>, ProviderError> {
        if tools.is_empty() || tools.len() > MAX_TOOL_DEFINITIONS {
            return Err(ProviderError::InvalidConfiguration);
        }
        let tools: Vec<WireTool<'_>> = tools
            .iter()
            .map(|definition| WireTool {
                kind: "function",
                function: definition,
            })
            .collect();
        let tool_bytes = serde_json::to_vec(&tools).map_err(|_| ProviderError::RequestTooLarge)?;
        self.check_messages(
            messages,
            u64::try_from(tool_bytes.len())
                .map_err(|_| ProviderError::RequestTooLarge)?
                .checked_add(self.effort_bytes())
                .ok_or(ProviderError::RequestTooLarge)?,
        )?;
        serde_json::to_vec(&ToolChatRequest {
            model: &self.model,
            messages,
            stream: streaming,
            limit: self.output_limit(),
            thinking: self.thinking,
            reasoning_effort: self.reasoning_effort,
            tools: &tools,
            tool_choice: "auto",
        })
        .map_err(|_| ProviderError::MalformedResponse)
    }

    /// Require the compiled base prompt as the only system message and bound the request.
    ///
    /// `extra_bytes` covers serialized request fields beyond the messages, such
    /// as tool definitions, in both the context estimate and the byte limit.
    fn check_messages(
        &self,
        messages: &[ModelMessage],
        extra_bytes: u64,
    ) -> Result<(), ProviderError> {
        let Some((base_message, following_messages)) = messages.split_first() else {
            return Err(ProviderError::InvalidConfiguration);
        };
        if base_message.role != ModelRole::System
            || base_message.content != BASE_PROMPT
            || following_messages
                .iter()
                .any(|message| message.role == ModelRole::System)
        {
            return Err(ProviderError::InvalidConfiguration);
        }

        let estimate = estimate_context(messages, self.max_output_tokens)?
            .checked_add(extra_bytes)
            .ok_or(ProviderError::ContextLimit)?;
        if estimate > self.context_tokens {
            return Err(ProviderError::ContextLimit);
        }
        let size = estimate_serialized_request_size(messages, &self.model)?
            .checked_add(extra_bytes)
            .ok_or(ProviderError::RequestTooLarge)?;
        if size > MAX_REQUEST_BYTES as u64 {
            return Err(ProviderError::RequestTooLarge);
        }
        Ok(())
    }

    /// Drive one request on a single-thread runtime made for this call.
    ///
    /// The request size is checked first, and nothing is created, let alone
    /// connected, for a token already set. The runtime is dropped on return;
    /// the exchange builds its HTTP client inside it, so a pooled connection
    /// never outlives the runtime that drives it. A runtime that cannot be
    /// built is `Transport`.
    fn on_request_runtime<T>(
        &self,
        body_len: usize,
        cancel: &CancelToken,
        exchange: impl Future<Output = Result<T, ProviderError>>,
    ) -> Result<T, ProviderError> {
        if body_len > MAX_REQUEST_BYTES {
            return Err(ProviderError::RequestTooLarge);
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| ProviderError::Transport)?;
        runtime.block_on(exchange)
    }

    /// Build the HTTP client for one request. Call it only inside the
    /// request's runtime.
    ///
    /// No proxy, no redirect, no retry, the connect timeout, and the request
    /// deadline. The async client's `timeout` is a total deadline from the
    /// start of the request until the body has finished (reqwest 0.13.5,
    /// `async_impl/client.rs`).
    fn http_client(&self) -> Result<Client, ProviderError> {
        Client::builder()
            .no_proxy()
            .https_only(self.https_only)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(self.request_timeout)
            .build()
            .map_err(|_| ProviderError::Transport)
    }

    /// Post one encoded body to the validated loopback endpoint and wait for
    /// the response head while watching `cancel`.
    async fn send(
        &self,
        client: &Client,
        body: Vec<u8>,
        streaming: bool,
        authorization: Option<&HeaderValue>,
        cancel: &CancelToken,
    ) -> Result<Response, ProviderError> {
        // Each attempt sends a copy: a refused connect consumed nothing, but
        // sending takes ownership of the body. Each attempt's timeout is the
        // time left, so the request keeps one deadline (ADR-0078).
        let build = |remaining: Duration| {
            let mut request = client
                .post(self.endpoint.clone())
                .timeout(remaining)
                .header(reqwest::header::CONTENT_TYPE, "application/json");
            if streaming {
                request = request.header(reqwest::header::ACCEPT, "text/event-stream");
            }
            if let Some(authorization) = authorization {
                request = request.header(AUTHORIZATION, authorization.clone());
            }
            request.body(body.clone())
        };
        send_request(self.request_timeout, build, cancel).await
    }

    /// Send a non-streaming request and read its whole body within 1 MiB.
    ///
    /// Dropping the response or the pending request on any return closes the
    /// connection.
    async fn whole_exchange(
        &self,
        body: Vec<u8>,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, ProviderError> {
        let client = self.http_client()?;
        let response = self
            .send(&client, body, false, self.authorization.as_ref(), cancel)
            .await?;
        read_whole_body(response, MAX_RESPONSE_BYTES, cancel).await
    }

    /// Send a streaming request and feed the response body to the stream
    /// parser.
    ///
    /// Every wait goes through [`until_cancelled`], and the token is also
    /// checked before each piece of the body, so a fast stream cannot starve
    /// the check. Dropping the response or the pending request on any return
    /// closes the connection. Text already passed to `on_text` stays delivered.
    async fn stream_exchange(
        &self,
        body: Vec<u8>,
        mode: stream::StreamMode,
        on_text: &mut dyn FnMut(&str),
        cancel: &CancelToken,
    ) -> Result<stream::StreamOutcome, ProviderError> {
        let client = self.http_client()?;
        let mut response = self
            .send(&client, body, true, self.authorization.as_ref(), cancel)
            .await?;
        check_streaming_head(response.status(), response.headers())?;

        let mut parser = stream::StreamParser::new(mode);
        loop {
            // A fast stream never lets the poll interval elapse, so look here.
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let chunk = until_cancelled(response.chunk(), cancel)
                .await?
                .map_err(|_| ProviderError::Transport)?;
            let Some(chunk) = chunk else {
                // The body ended without `[DONE]`.
                return parser.finish();
            };
            if parser.push(&chunk, on_text)? == stream::Flow::Done {
                // A cancellation after `[DONE]` no longer changes the reply.
                return parser.finish();
            }
        }
    }
}

impl LocalModelClient {
    /// Send one plan-route request and read its stream (ADR-0093).
    ///
    /// The tool count and the context and size bounds are checked first, then
    /// the cancel token, and only then is `tokens` asked for an access token,
    /// so a refused request never reads or refreshes a token. The token is
    /// asked outside the request's runtime because a refresh drives its own.
    fn responses_request(
        &self,
        tokens: &dyn AccessTokenSource,
        messages: &[ModelMessage],
        tools: Option<&[ToolDefinition]>,
        on_text: &mut dyn FnMut(&str),
        cancel: &CancelToken,
    ) -> Result<responses::ResponsesOutcome, ProviderError> {
        let tool_bytes = match tools {
            None => 0,
            Some(tools) => {
                if tools.is_empty() || tools.len() > MAX_TOOL_DEFINITIONS {
                    return Err(ProviderError::InvalidConfiguration);
                }
                let tool_bytes =
                    serde_json::to_vec(tools).map_err(|_| ProviderError::RequestTooLarge)?;
                let tool_bytes =
                    u64::try_from(tool_bytes.len()).map_err(|_| ProviderError::RequestTooLarge)?;
                // Each tool result also repeats its call (ADR-0097).
                tool_bytes
                    .checked_add(responses::answered_call_bytes(messages)?)
                    .ok_or(ProviderError::RequestTooLarge)?
            }
        };
        let extra_bytes = tool_bytes
            .checked_add(self.effort_bytes())
            .ok_or(ProviderError::RequestTooLarge)?;
        self.check_messages(messages, extra_bytes)?;
        let body = responses::request_body(&self.model, self.reasoning_effort, messages, tools)?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(ProviderError::RequestTooLarge);
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let authorization = {
            let token = tokens.access_token(cancel)?;
            bearer_header(&token)?
        };
        let mode = if tools.is_some() {
            responses::ResponsesMode::Tools
        } else {
            responses::ResponsesMode::Text
        };
        self.on_request_runtime(
            body.len(),
            cancel,
            self.responses_exchange(body, &authorization, mode, on_text, cancel),
        )
    }

    /// Post one plan-route request and feed its body to the stream reader.
    ///
    /// A non-success status reads at most 64 KiB of the body for a documented
    /// error code and reports nothing else from it. A success head may omit
    /// `Content-Type`, as the plan route does, but may not name another media
    /// type ([`check_responses_head`]). Cancellation and the deadline work as
    /// for [`Self::stream_exchange`].
    async fn responses_exchange(
        &self,
        body: Vec<u8>,
        authorization: &HeaderValue,
        mode: responses::ResponsesMode,
        on_text: &mut dyn FnMut(&str),
        cancel: &CancelToken,
    ) -> Result<responses::ResponsesOutcome, ProviderError> {
        let client = self.http_client()?;
        let mut response = self
            .send(&client, body, true, Some(authorization), cancel)
            .await?;
        let status = response.status();
        if !status.is_success() {
            let failure = read_error_body(&mut response, cancel)
                .await?
                .as_deref()
                .and_then(responses::failure_in_error_body);
            return Err(failure.map_or(
                ProviderError::HttpStatus(status.as_u16()),
                ProviderError::ResponseFailed,
            ));
        }
        check_responses_head(response.headers())?;
        let mut parser = responses::ResponsesParser::new(mode);
        loop {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let chunk = until_cancelled(response.chunk(), cancel)
                .await?
                .map_err(|_| ProviderError::Transport)?;
            let Some(chunk) = chunk else {
                return parser.finish();
            };
            if parser.push(&chunk, on_text)? == responses::ResponsesFlow::Completed {
                return parser.finish();
            }
        }
    }
}

/// Read at most [`MAX_ERROR_BODY_BYTES`] of a failed response. A read error
/// or a longer body gives `None`, so the caller reports the status alone.
async fn read_error_body(
    response: &mut Response,
    cancel: &CancelToken,
) -> Result<Option<Vec<u8>>, ProviderError> {
    let mut bytes = Vec::new();
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let Ok(chunk) = until_cancelled(response.chunk(), cancel).await? else {
            return Ok(None);
        };
        let Some(chunk) = chunk else {
            return Ok(Some(bytes));
        };
        let total = u64::try_from(bytes.len().saturating_add(chunk.len())).unwrap_or(u64::MAX);
        if total > MAX_ERROR_BODY_BYTES {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
}

/// A plan-route outcome as a text reply. A tool call never reaches here: the
/// text mode refuses it.
fn text_reply(outcome: responses::ResponsesOutcome) -> ModelReply {
    ModelReply {
        content: outcome.content,
        finish_reason: outcome.finish_reason,
        usage: outcome.usage,
    }
}

/// A plan-route outcome as a tool-capable reply.
fn tool_reply(outcome: responses::ResponsesOutcome) -> ToolModelReply {
    ToolModelReply {
        content: outcome.content,
        tool_calls: outcome.tool_calls,
        finish_reason: outcome.finish_reason,
    }
}

/// Read the whole body of a non-streaming response within `limit` bytes:
/// [`MAX_RESPONSE_BYTES`] for a chat reply, more for a model list.
///
/// A non-success status is refused before the body is read. The token is
/// checked before each piece of the body as well as while each is awaited; a
/// cancelled read discards what it read. Dropping the response on any return
/// closes the connection.
async fn read_whole_body(
    mut response: Response,
    limit: u64,
    cancel: &CancelToken,
) -> Result<Vec<u8>, ProviderError> {
    let status = response.status();
    if !status.is_success() {
        return Err(ProviderError::HttpStatus(status.as_u16()));
    }
    let mut bytes = Vec::new();
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let chunk = until_cancelled(response.chunk(), cancel)
            .await?
            .map_err(|_| ProviderError::Transport)?;
        let Some(chunk) = chunk else {
            return Ok(bytes);
        };
        let total = bytes
            .len()
            .checked_add(chunk.len())
            .and_then(|total| u64::try_from(total).ok())
            .ok_or(ProviderError::ResponseTooLarge)?;
        if total > limit {
            return Err(ProviderError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
}

/// Send the request that `build` makes, skipping local ports Windows refuses
/// as still in use for this server (ADR-0078), while watching `cancel`.
///
/// Every send failure is `Transport`; its details never leave this module.
///
/// `build` must set the time it receives as the request's timeout, so every
/// attempt shares the deadline `timeout` from now, including reading the body.
async fn send_request(
    timeout: Duration,
    build: impl FnMut(Duration) -> RequestBuilder,
    cancel: &CancelToken,
) -> Result<Response, ProviderError> {
    let deadline = std::time::Instant::now() + timeout;
    until_cancelled(
        crate::connect::send_skipping_reused_ports(deadline, build),
        cancel,
    )
    .await?
    .map_err(|_| ProviderError::Transport)
}

/// Await `future` while watching `cancel` every [`CANCEL_POLL`].
///
/// The future is pinned and polled through a timeout on `&mut`, so an elapsed
/// poll does not drop it and the request is not restarted. When the token is
/// set the future is dropped and the result is `Cancelled`. Whatever deadline
/// the future carries itself still applies.
async fn until_cancelled<F: Future>(
    future: F,
    cancel: &CancelToken,
) -> Result<F::Output, ProviderError> {
    let mut future = std::pin::pin!(future);
    loop {
        match tokio::time::timeout(CANCEL_POLL, future.as_mut()).await {
            Ok(output) => return Ok(output),
            Err(_elapsed) => {
                if cancel.is_cancelled() {
                    return Err(ProviderError::Cancelled);
                }
            }
        }
    }
}

/// Map the local client onto the turn runtime's model interface.
///
/// The adapter keeps the most recent provider failure so a caller can report
/// it precisely; the runtime records only the coarse [`ModelError`].
pub struct LocalToolModel<'a> {
    client: &'a LocalModelClient,
    on_text: Option<&'a mut dyn FnMut(&str)>,
    last_error: Option<ProviderError>,
}

impl<'a> LocalToolModel<'a> {
    /// Wrap a validated client for one tool-enabled turn.
    pub const fn new(client: &'a LocalModelClient) -> Self {
        Self {
            client,
            on_text: None,
            last_error: None,
        }
    }

    /// Wrap a validated client so each request is streamed.
    ///
    /// `on_text` receives the provisional assistant text of every request of
    /// the turn, as described for
    /// [`LocalModelClient::complete_with_tools_streaming`].
    pub const fn streaming(client: &'a LocalModelClient, on_text: &'a mut dyn FnMut(&str)) -> Self {
        Self {
            client,
            on_text: Some(on_text),
            last_error: None,
        }
    }

    /// Return the provider failure behind the last model error, if any.
    pub const fn last_error(&self) -> Option<ProviderError> {
        self.last_error
    }
}

impl ToolModel for LocalToolModel<'_> {
    /// Send one request and remember the precise failure for reporting.
    fn complete(&mut self, request: ToolModelRequest<'_>) -> Result<ToolModelReply, ModelError> {
        let result = match self.on_text.as_mut() {
            Some(on_text) => self.client.complete_with_tools_streaming(
                request.messages,
                request.tools,
                &mut **on_text,
                request.cancel,
            ),
            None => {
                self.client
                    .complete_with_tools(request.messages, request.tools, request.cancel)
            }
        };
        result.map_err(|error| {
            self.last_error = Some(error);
            model_error(error)
        })
    }
}

/// Collapse provider failures into the runtime's bounded categories.
const fn model_error(error: ProviderError) -> ModelError {
    match error {
        ProviderError::RequestTooLarge | ProviderError::ContextLimit => ModelError::RequestTooLarge,
        ProviderError::Transport
        | ProviderError::HttpStatus(_)
        | ProviderError::IncompleteStream
        | ProviderError::ResponseFailed(_)
        | ProviderError::SignInRequired
        | ProviderError::AccessTokenUnavailable => ModelError::Transport,
        ProviderError::Cancelled => ModelError::Cancelled,
        ProviderError::InvalidConfiguration
        | ProviderError::InvalidToken
        | ProviderError::ResponseTooLarge
        | ProviderError::MalformedResponse
        | ProviderError::UnsupportedToolCall => ModelError::InvalidResponse,
    }
}

/// Build the sensitive `Authorization` value for an optional local token.
///
/// An empty token, a control character, or a value HTTP cannot carry is
/// `InvalidToken`. The header is marked sensitive so it is never logged.
fn bearer_header(token: &str) -> Result<HeaderValue, ProviderError> {
    if token.is_empty() || token.chars().any(char::is_control) {
        return Err(ProviderError::InvalidToken);
    }
    let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| ProviderError::InvalidToken)?;
    header.set_sensitive(true);
    Ok(header)
}

/// Reject destinations which could redirect prompts or credentials off loopback.
///
/// This is the one implementation of the local endpoint rule. The client
/// applies it to every request configuration, and a provider profile applies
/// it before it records an endpoint (ADR-0048), so the two cannot disagree.
pub(crate) fn validate_base_url(base_url: &str) -> Result<Url, ProviderError> {
    if base_url.chars().any(char::is_control) || !base_url.starts_with("http://") {
        return Err(ProviderError::InvalidConfiguration);
    }
    let after_scheme = &base_url["http://".len()..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.contains('%') {
        return Err(ProviderError::InvalidConfiguration);
    }

    let (host, raw_port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed
            .find(']')
            .ok_or(ProviderError::InvalidConfiguration)?;
        let host = &bracketed[..close];
        let suffix = &bracketed[close + 1..];
        let port = suffix
            .strip_prefix(':')
            .ok_or(ProviderError::InvalidConfiguration)?;
        (host, port)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or(ProviderError::InvalidConfiguration)?;
        if host.contains(':') {
            return Err(ProviderError::InvalidConfiguration);
        }
        (host, port)
    };
    let address: IpAddr = host
        .parse()
        .map_err(|_| ProviderError::InvalidConfiguration)?;
    let _port = raw_port
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(ProviderError::InvalidConfiguration)?;
    if !address.is_loopback() {
        return Err(ProviderError::InvalidConfiguration);
    }

    let parsed = Url::parse(base_url).map_err(|_| ProviderError::InvalidConfiguration)?;
    if parsed.scheme() != "http"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "/v1" | "/v1/")
    {
        return Err(ProviderError::InvalidConfiguration);
    }
    Ok(parsed)
}

/// Bound input with a byte-based estimate and reserve output plus template headroom.
fn estimate_context(
    messages: &[ModelMessage],
    max_output_tokens: u32,
) -> Result<u64, ProviderError> {
    let message_count = u64::try_from(messages.len()).map_err(|_| ProviderError::ContextLimit)?;
    let content_bytes = messages.iter().try_fold(0_u64, |total, message| {
        let size =
            u64::try_from(message.content.len()).map_err(|_| ProviderError::RequestTooLarge)?;
        total
            .checked_add(size)
            .ok_or(ProviderError::RequestTooLarge)
    })?;
    content_bytes
        .checked_add(
            message_count
                .checked_mul(MESSAGE_OVERHEAD_ESTIMATE)
                .ok_or(ProviderError::ContextLimit)?,
        )
        .and_then(|total| total.checked_add(PROMPT_HEADROOM_ESTIMATE))
        .and_then(|total| total.checked_add(u64::from(max_output_tokens)))
        .ok_or(ProviderError::ContextLimit)
}

/// Bound escaped strings before allocating the complete encoded request.
fn estimate_serialized_request_size(
    messages: &[ModelMessage],
    model: &str,
) -> Result<u64, ProviderError> {
    let encoded_strings = messages
        .iter()
        .try_fold(json_string_len(model)?, |total, message| {
            total
                .checked_add(json_string_len(&message.content)?)
                .ok_or(ProviderError::RequestTooLarge)
        })?;
    let message_overhead = u64::try_from(messages.len())
        .map_err(|_| ProviderError::RequestTooLarge)?
        .checked_mul(64)
        .ok_or(ProviderError::RequestTooLarge)?;
    encoded_strings
        .checked_add(message_overhead)
        .and_then(|total| total.checked_add(128))
        .ok_or(ProviderError::RequestTooLarge)
}

/// Count JSON escape expansion with checked arithmetic rather than allocating it.
fn json_string_len(value: &str) -> Result<u64, ProviderError> {
    value.chars().try_fold(2_u64, |length, character| {
        let encoded_len = match character {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => u64::try_from(character.len_utf8()).map_err(|_| ProviderError::RequestTooLarge)?,
        };
        length
            .checked_add(encoded_len)
            .ok_or(ProviderError::RequestTooLarge)
    })
}

/// Accept only a successful `text/event-stream` head for a streamed request.
///
/// A server that answers with one JSON document is refused rather than parsed:
/// the caller asked for a stream and nothing is repaired. Content-type
/// parameters such as `charset` are allowed and the media type is compared
/// case-insensitively.
fn check_streaming_head(status: StatusCode, headers: &HeaderMap) -> Result<(), ProviderError> {
    if !status.is_success() {
        return Err(ProviderError::HttpStatus(status.as_u16()));
    }
    if headers
        .get(reqwest::header::CONTENT_TYPE)
        .is_some_and(names_event_stream)
    {
        Ok(())
    } else {
        Err(ProviderError::MalformedResponse)
    }
}

/// The plan route's head check (ADR-0096). Its caller has already turned a
/// non-success status into an error.
///
/// OpenAI's plan route answers a streamed Responses request with no
/// `Content-Type` header at all (observed on 2026-11-24), so an absent header
/// is accepted. A header that names any other media type is still refused, as
/// [`check_streaming_head`] refuses it. Accepting the absence repairs nothing:
/// the body must still pass the strict event reader, which refuses any line
/// outside the server-sent-event fields and any ending but
/// `response.completed`.
fn check_responses_head(headers: &HeaderMap) -> Result<(), ProviderError> {
    match headers.get(reqwest::header::CONTENT_TYPE) {
        None => Ok(()),
        Some(value) if names_event_stream(value) => Ok(()),
        Some(_) => Err(ProviderError::MalformedResponse),
    }
}

/// Whether a `Content-Type` value names `text/event-stream`. Parameters such
/// as `charset` are allowed and the media type is compared case-insensitively.
fn names_event_stream(value: &HeaderValue) -> bool {
    value
        .to_str()
        .ok()
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// Parse one tool-capable reply strictly, leaving each call unvalidated.
///
/// Duplicate keys at any depth are refused before any value is inspected, so a
/// call cannot carry two names or two argument strings. Content may be `null`
/// only when the reply contains tool calls. A reply cut short by the token
/// limit cannot carry tool calls, because their arguments may be truncated.
fn parse_tool_response(bytes: &[u8]) -> Result<ToolModelReply, ProviderError> {
    let value = parse_strict_json(bytes).map_err(|_| ProviderError::MalformedResponse)?;
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| choices.len() == 1)
        .ok_or(ProviderError::MalformedResponse)?;
    let choice = choices.first().ok_or(ProviderError::MalformedResponse)?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or(ProviderError::MalformedResponse)?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(ProviderError::MalformedResponse);
    }
    // Legacy single function calls and choice-level calls are not supported.
    reject_actions(choice.get("tool_calls"), choice.get("function_call"))?;
    reject_actions(None, message.get("function_call"))?;

    let tool_calls = match message.get("tool_calls") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(calls)) if calls.len() <= MAX_RESPONSE_TOOL_CALLS => {
            calls.iter().map(without_position_index).collect()
        }
        Some(_) => return Err(ProviderError::MalformedResponse),
    };
    let content = match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        None | Some(Value::Null) if !tool_calls.is_empty() => String::new(),
        _ => return Err(ProviderError::MalformedResponse),
    };
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .ok_or(ProviderError::MalformedResponse)?;
    let accepted = if tool_calls.is_empty() {
        matches!(finish_reason, "stop" | "length")
    } else {
        matches!(finish_reason, "tool_calls" | "stop")
    };
    if !accepted {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(ToolModelReply {
        content,
        tool_calls,
        finish_reason: finish_reason.to_owned(),
    })
}

/// One buffered call without the numeric `index` member that Ollama adds
/// (ADR-0064). A stream uses `index` to assemble calls and never keeps it, so
/// both paths hand [`crate::turn::ToolCall::parse`] the same three members.
/// Any other member, or an `index` that is not a non-negative integer, stays,
/// and that strict parser refuses it.
fn without_position_index(call: &Value) -> Value {
    let mut call = call.clone();
    if let Some(object) = call.as_object_mut()
        && object.get("index").is_some_and(Value::is_u64)
    {
        object.remove("index");
    }
    call
}

/// Parse one bounded assistant response; never expose server error bodies.
fn parse_response(bytes: &[u8]) -> Result<ModelReply, ProviderError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| ProviderError::MalformedResponse)?;
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| choices.len() == 1)
        .ok_or(ProviderError::MalformedResponse)?;
    let choice = choices.first().ok_or(ProviderError::MalformedResponse)?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or(ProviderError::MalformedResponse)?;
    reject_actions(message.get("tool_calls"), message.get("function_call"))?;
    reject_actions(choice.get("tool_calls"), choice.get("function_call"))?;
    if matches!(
        choice.get("finish_reason").and_then(Value::as_str),
        Some("tool_calls" | "function_call")
    ) {
        return Err(ProviderError::UnsupportedToolCall);
    }

    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or(ProviderError::MalformedResponse)?;
    if role != "assistant" {
        return Err(ProviderError::MalformedResponse);
    }
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or(ProviderError::MalformedResponse)?;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|reason| matches!(*reason, "stop" | "length"))
        .ok_or(ProviderError::MalformedResponse)?;
    let usage = match value.get("usage") {
        None | Some(Value::Null) => None,
        Some(usage) => Some(parse_usage(usage)?),
    };

    Ok(ModelReply {
        content: content.to_owned(),
        finish_reason: finish_reason.to_owned(),
        usage,
    })
}

/// Accept explicit empty action fields while refusing all actual action requests.
fn reject_actions(
    tool_calls: Option<&Value>,
    function_call: Option<&Value>,
) -> Result<(), ProviderError> {
    match tool_calls {
        None | Some(Value::Null) => {}
        // LM Studio includes an empty array in ordinary text-only replies.
        Some(Value::Array(calls)) if calls.is_empty() => {}
        Some(Value::Array(_)) => return Err(ProviderError::UnsupportedToolCall),
        Some(_) => return Err(ProviderError::MalformedResponse),
    }
    if function_call.is_some_and(|call| !call.is_null()) {
        return Err(ProviderError::UnsupportedToolCall);
    }
    Ok(())
}

/// Preserve optional provider counters without interpreting them as verified cost.
fn parse_usage(value: &Value) -> Result<Usage, ProviderError> {
    let object = value.as_object().ok_or(ProviderError::MalformedResponse)?;
    /// Refuse non-integer or negative counters; absence is not a fabricated zero.
    fn optional_count(
        object: &serde_json::Map<String, Value>,
        key: &str,
    ) -> Result<Option<u64>, ProviderError> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or(ProviderError::MalformedResponse),
        }
    }

    Ok(Usage {
        prompt_tokens: optional_count(object, "prompt_tokens")?,
        completion_tokens: optional_count(object, "completion_tokens")?,
        total_tokens: optional_count(object, "total_tokens")?,
    })
}

/// Destinations every endpoint rule refuses. The provider profile tests use
/// the same list, so the rule is proved to be shared. An `https://` text is
/// not listed: the hosted rule admits a well-formed one, and the profile's
/// kind decides whether it may be recorded.
#[cfg(test)]
pub(crate) const REFUSED_ENDPOINTS: &[&str] = &[
    "http://localhost:1234/v1",
    "http://192.0.2.1:1234/v1",
    "http://127.0.0.1/v1",
    "http://127.0.0.1:0/v1",
    "http://127.0.0.1:1234/v1/other",
    "http://user@127.0.0.1:1234/v1",
    "http://127.0.0.1:1234/v1?query=1",
    "http://127.0.0.1:1234/v1#fragment",
    "http://127.0.0.1:1234/v1\u{7}",
    "",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The client refuses every listed destination; the profile record tests
    /// run the same list through `validate_base_url`'s other caller.
    #[test]
    fn the_client_refuses_every_listed_endpoint() {
        for base_url in REFUSED_ENDPOINTS {
            let result = LocalModelClient::new(
                LocalModelConfig {
                    base_url: (*base_url).to_owned(),
                    model: "m".to_owned(),
                    context_tokens: 4096,
                    max_output_tokens: 64,
                },
                None,
            );
            assert_eq!(
                result.err(),
                Some(ProviderError::InvalidConfiguration),
                "{base_url:?}"
            );
            assert!(validate_base_url(base_url).is_err(), "{base_url:?}");
        }
        assert!(validate_base_url("http://127.0.0.1:1234/v1").is_ok());
        assert!(validate_base_url("http://[::1]:1234/v1/").is_ok());
    }

    /// The endpoint type accepts a loopback endpoint, a fixed hosted endpoint
    /// with its output field, or a well-formed HTTPS base URL, and refuses
    /// every malformed variant (ADR-0071, ADR-0092). Which hosted URL a
    /// profile may record is decided by its kind, in the control store.
    #[test]
    fn the_endpoint_type_accepts_loopback_fixed_or_well_formed_https_endpoints() {
        assert!(matches!(
            ModelEndpoint::parse("http://127.0.0.1:1234/v1"),
            Ok(ModelEndpoint(EndpointRule::Loopback(_)))
        ));
        assert!(matches!(
            ModelEndpoint::parse("http://[::1]:1234/v1/"),
            Ok(ModelEndpoint(EndpointRule::Loopback(_)))
        ));
        for text in ["https://ollama.com/v1", "https://ollama.com/v1/"] {
            let endpoint = ModelEndpoint::parse(text).expect(text);
            assert!(endpoint.is_hosted(), "{text}");
            assert_eq!(endpoint.host().as_deref(), Some("ollama.com"));
            assert_eq!(
                endpoint.as_str(),
                "https://ollama.com/v1".to_owned() + &text[21..]
            );
        }
        for (text, field) in [
            (OPENAI_ENDPOINT, OutputLimitField::MaxCompletionTokens),
            (XAI_ENDPOINT, OutputLimitField::MaxCompletionTokens),
            (DEEPSEEK_ENDPOINT, OutputLimitField::MaxTokens),
            (Z_AI_ENDPOINT, OutputLimitField::MaxTokens),
            (
                "https://provider-api.example.org/v1",
                OutputLimitField::MaxTokens,
            ),
            ("https://ollama.com:8443/v1", OutputLimitField::MaxTokens),
        ] {
            let endpoint = ModelEndpoint::parse(text).expect(text);
            assert!(endpoint.is_hosted(), "{text}");
            assert_eq!(endpoint.output_field(), field, "{text}");
        }
        // The host a confirmation names keeps a non-default port, as the
        // browser's `URL.host` does, so it names the service that receives
        // the key.
        for (text, host) in [
            (
                "https://provider-api.example.org:8443/v1",
                "provider-api.example.org:8443",
            ),
            ("https://[2001:db8::1]:8443/v1", "[2001:db8::1]:8443"),
            ("https://192.0.2.10/v1", "192.0.2.10"),
            ("http://127.0.0.1:1234/v1", "127.0.0.1:1234"),
        ] {
            let endpoint = ModelEndpoint::parse(text).expect(text);
            assert_eq!(endpoint.host().as_deref(), Some(host), "{text}");
        }
        for text in [
            "http://ollama.com/v1",
            "https://ollama.com/v1//",
            "https://ollama.com:443/v1",
            "https://user@ollama.com/v1",
            "https://ollama.com/v1?q=1",
            "https://ollama.com/v1#f",
            "https://OLLAMA.com/v1",
            "HTTPS://ollama.com/v1",
            " https://ollama.com/v1",
            "https://ollama.com/v1\n",
            "http://evil.example/v1",
        ] {
            assert_eq!(
                ModelEndpoint::parse(text),
                Err(ProviderError::InvalidConfiguration),
                "{text:?}"
            );
        }
        // Every loopback refusal stays a refusal.
        for text in REFUSED_ENDPOINTS {
            assert!(ModelEndpoint::parse(text).is_err(), "{text:?}");
        }
    }

    fn cloud_config() -> LocalModelConfig {
        LocalModelConfig {
            base_url: OLLAMA_CLOUD_ENDPOINT.to_owned(),
            model: "minimax-m3".to_owned(),
            context_tokens: 131_072,
            max_output_tokens: 64,
        }
    }

    /// A cloud client needs a token and builds without any connection. The
    /// request path is the OpenAI-compatible chat path on the cloud host.
    #[test]
    fn a_cloud_client_requires_a_token_before_any_connection() {
        assert_eq!(
            LocalModelClient::new(cloud_config(), None).err(),
            Some(ProviderError::InvalidToken)
        );
        assert_eq!(
            LocalModelClient::new(cloud_config(), Some("bad\nkey".to_owned())).err(),
            Some(ProviderError::InvalidToken)
        );
        let client = LocalModelClient::new(cloud_config(), Some("not-a-real-key".to_owned()))
            .expect("cloud client");
        assert_eq!(
            client.endpoint.as_str(),
            "https://ollama.com/v1/chat/completions"
        );
        assert!(client.https_only);
        let debug = format!("{client:?}");
        assert!(!debug.contains("not-a-real-key"), "{debug}");
        // A loopback client keeps its token optional and plain HTTP.
        let local = LocalModelClient::new(
            LocalModelConfig {
                base_url: "http://127.0.0.1:1234/v1".to_owned(),
                ..cloud_config()
            },
            None,
        )
        .expect("local client");
        assert!(!local.https_only);
        // The cloud endpoint with a trailing slash selects the same URL.
        let slash = LocalModelClient::new(
            LocalModelConfig {
                base_url: "https://ollama.com/v1/".to_owned(),
                ..cloud_config()
            },
            Some("not-a-real-key".to_owned()),
        )
        .expect("slash client");
        assert_eq!(slash.endpoint, client.endpoint);
    }

    /// Each hosted endpoint's chat path is its base path plus
    /// `/chat/completions`, every hosted client needs a token, and the output
    /// budget goes in the one field the endpoint takes (ADR-0092).
    #[test]
    fn hosted_clients_append_the_chat_path_and_name_the_output_field() {
        for (base, path, field) in [
            (
                OPENAI_ENDPOINT,
                "https://api.openai.com/v1/chat/completions",
                "max_completion_tokens",
            ),
            (
                DEEPSEEK_ENDPOINT,
                "https://api.deepseek.com/chat/completions",
                "max_tokens",
            ),
            (
                Z_AI_ENDPOINT,
                "https://api.z.ai/api/paas/v4/chat/completions",
                "max_tokens",
            ),
            (
                GOOGLE_ENDPOINT,
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
                "max_tokens",
            ),
            (
                MOONSHOT_ENDPOINT,
                "https://api.moonshot.ai/v1/chat/completions",
                "max_completion_tokens",
            ),
            (
                THINKING_MACHINES_ENDPOINT,
                "https://tinker.thinkingmachines.dev/services/tinker-prod/oai/api/v1/chat/completions",
                "max_completion_tokens",
            ),
            (
                "https://provider-api.example.org/v1/",
                "https://provider-api.example.org/v1/chat/completions",
                "max_tokens",
            ),
            (
                "http://127.0.0.1:1234/v1",
                "http://127.0.0.1:1234/v1/chat/completions",
                "max_tokens",
            ),
        ] {
            let config = || LocalModelConfig {
                base_url: base.to_owned(),
                ..cloud_config()
            };
            let hosted = base.starts_with("https://");
            if hosted {
                assert_eq!(
                    LocalModelClient::new(config(), None).err(),
                    Some(ProviderError::InvalidToken),
                    "{base}"
                );
            }
            let client = LocalModelClient::new(config(), Some("not-a-real-key".to_owned()))
                .unwrap_or_else(|error| panic!("{base}: {error}"));
            assert_eq!(client.endpoint.as_str(), path);
            assert_eq!(client.https_only, hosted, "{base}");
            let body: Value = serde_json::from_slice(
                &client
                    .text_body(
                        &[ModelMessage {
                            role: ModelRole::System,
                            content: BASE_PROMPT.to_owned(),
                        }],
                        false,
                    )
                    .expect("body"),
            )
            .expect("json");
            let other = if field == "max_tokens" {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            assert_eq!(body.get(field), Some(&Value::from(64)), "{base}");
            assert!(body.get(other).is_none(), "{base}");
        }
    }

    /// DeepSeek thinks by default and answers 400 to a tool request that does
    /// not send earlier reasoning back, which this client never keeps, so every
    /// DeepSeek request (text or tools, first or later in a turn) switches
    /// thinking off. No other endpoint gets the field (ADR-0092).
    #[test]
    fn only_deepseek_requests_switch_thinking_off() {
        // A later request in a tool turn: earlier assistant text and a tool result.
        let history = [
            ModelMessage {
                role: ModelRole::System,
                content: BASE_PROMPT.to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "read the notes".to_owned(),
            },
            ModelMessage {
                role: ModelRole::Assistant,
                content: "Reading notes.md.".to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "tool result: notes".to_owned(),
            },
        ];
        let tools = crate::turn::tool_definitions();
        for (base, off) in [
            (DEEPSEEK_ENDPOINT, true),
            ("https://api.deepseek.com/", true),
            (OPENAI_ENDPOINT, false),
            (MOONSHOT_ENDPOINT, false),
            ("https://provider-api.example.org/v1", false),
            ("http://127.0.0.1:1234/v1", false),
        ] {
            let client = LocalModelClient::new(
                LocalModelConfig {
                    base_url: base.to_owned(),
                    ..cloud_config()
                },
                Some("not-a-real-key".to_owned()),
            )
            .unwrap_or_else(|error| panic!("{base}: {error}"));
            for streaming in [false, true] {
                let bodies = [
                    client.text_body(&history, streaming).expect("text body"),
                    client
                        .tool_body(&history, &tools, streaming)
                        .expect("tool body"),
                ];
                for bytes in bodies {
                    let body: Value = serde_json::from_slice(&bytes).expect("json");
                    let expected = off.then(|| serde_json::json!({"type": "disabled"}));
                    assert_eq!(body.get("thinking").cloned(), expected, "{base}");
                }
            }
        }
    }

    /// The endpoints that take an effort (ADR-0102): every loopback and
    /// operator-typed endpoint, and only the fixed ones whose provider
    /// documents `reasoning_effort`.
    #[test]
    fn only_documented_endpoints_take_a_reasoning_effort() {
        for (base, takes) in [
            ("http://127.0.0.1:1234/v1", true),
            ("http://127.0.0.1:11434/v1", true),
            ("https://provider-api.example.org/v1", true),
            (OLLAMA_CLOUD_ENDPOINT, true),
            (OPENAI_ENDPOINT, true),
            ("https://api.openai.com/v1/", true),
            (GOOGLE_ENDPOINT, true),
            (MOONSHOT_ENDPOINT, true),
            (MINIMAX_ENDPOINT, true),
            (OPENROUTER_ENDPOINT, true),
            (Z_AI_ENDPOINT, true),
            (XAI_ENDPOINT, true),
            (ANTHROPIC_ENDPOINT, false),
            (DEEPSEEK_ENDPOINT, false),
            (TENCENT_ENDPOINT, false),
            (META_ENDPOINT, false),
            (THINKING_MACHINES_ENDPOINT, false),
        ] {
            let endpoint = ModelEndpoint::parse(base).expect(base);
            assert_eq!(endpoint.takes_reasoning_effort(), takes, "{base}");
            let client = LocalModelClient::new(
                LocalModelConfig {
                    base_url: base.to_owned(),
                    ..cloud_config()
                },
                Some("not-a-real-key".to_owned()),
            )
            .unwrap_or_else(|error| panic!("{base}: {error}"));
            let result = client.with_reasoning_effort(Some(ReasoningEffort::High));
            if takes {
                assert_eq!(
                    result.expect(base).reasoning_effort(),
                    Some(ReasoningEffort::High)
                );
            } else {
                assert_eq!(result.err(), Some(ProviderError::InvalidConfiguration));
            }
        }
        // No effort is always accepted, including where none can be sent.
        let client = LocalModelClient::new(
            LocalModelConfig {
                base_url: DEEPSEEK_ENDPOINT.to_owned(),
                ..cloud_config()
            },
            Some("not-a-real-key".to_owned()),
        )
        .expect("deepseek client")
        .with_reasoning_effort(None)
        .expect("no effort");
        assert_eq!(client.reasoning_effort(), None);
    }

    /// A chosen effort is one top-level `reasoning_effort` field on every
    /// Chat Completions body; without one the body is exactly as before.
    #[test]
    fn a_chosen_effort_is_sent_as_reasoning_effort_on_chat_completions() {
        let history = [
            ModelMessage {
                role: ModelRole::System,
                content: BASE_PROMPT.to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "hello".to_owned(),
            },
        ];
        let tools = crate::turn::tool_definitions();
        for base in [
            "http://127.0.0.1:1234/v1",
            OPENAI_ENDPOINT,
            "https://provider-api.example.org/v1",
        ] {
            let plain = LocalModelClient::new(
                LocalModelConfig {
                    base_url: base.to_owned(),
                    ..cloud_config()
                },
                Some("not-a-real-key".to_owned()),
            )
            .expect(base);
            for effort in ReasoningEffort::ALL {
                let chosen = LocalModelClient::new(
                    LocalModelConfig {
                        base_url: base.to_owned(),
                        ..cloud_config()
                    },
                    Some("not-a-real-key".to_owned()),
                )
                .and_then(|client| client.with_reasoning_effort(Some(effort)))
                .expect(base);
                for streaming in [false, true] {
                    let pairs = [
                        (
                            chosen.text_body(&history, streaming).expect("text"),
                            plain.text_body(&history, streaming).expect("text"),
                        ),
                        (
                            chosen
                                .tool_body(&history, &tools, streaming)
                                .expect("tools"),
                            plain.tool_body(&history, &tools, streaming).expect("tools"),
                        ),
                    ];
                    for (with, without) in pairs {
                        let with: Value = serde_json::from_slice(&with).expect("json");
                        let mut without: Value = serde_json::from_slice(&without).expect("json");
                        assert_eq!(with["reasoning_effort"], effort.as_str(), "{base}");
                        assert!(with.get("reasoning").is_none(), "{base}");
                        assert!(without.get("reasoning_effort").is_none(), "{base}");
                        without["reasoning_effort"] = with["reasoning_effort"].clone();
                        assert_eq!(with, without, "{base}");
                    }
                }
            }
        }
    }

    /// The effort field counts in the context estimate, so a request that
    /// fits exactly without one is refused with one rather than sent over
    /// the budget.
    #[test]
    fn the_effort_field_counts_in_the_context_estimate() {
        let history = [
            ModelMessage {
                role: ModelRole::System,
                content: BASE_PROMPT.to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "hello".to_owned(),
            },
        ];
        let exact = estimate_context(&history, 64).expect("estimate");
        let client = |effort| {
            LocalModelClient::new(
                LocalModelConfig {
                    base_url: "http://127.0.0.1:1234/v1".to_owned(),
                    context_tokens: exact,
                    ..cloud_config()
                },
                None,
            )
            .and_then(|client| client.with_reasoning_effort(effort))
            .expect("client")
        };
        assert!(client(None).text_body(&history, false).is_ok());
        assert_eq!(
            client(Some(ReasoningEffort::Low)).text_body(&history, false),
            Err(ProviderError::ContextLimit)
        );
    }

    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    /// Advertised tool definitions consume context and wire budget before any request.
    #[test]
    fn tool_definitions_count_against_the_context_and_wire_limits() {
        let messages = [
            ModelMessage {
                role: ModelRole::System,
                content: BASE_PROMPT.to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "read the notes".to_owned(),
            },
        ];
        let text_estimate = estimate_context(&messages, 64).expect("estimate");
        // The port is never contacted: every check here fails before connecting.
        let client = LocalModelClient::new(
            LocalModelConfig {
                base_url: "http://127.0.0.1:9/v1".to_owned(),
                model: "m".to_owned(),
                context_tokens: text_estimate,
                max_output_tokens: 64,
            },
            None,
        )
        .expect("client");
        assert_eq!(client.check_messages(&messages, 0), Ok(()));
        assert_eq!(
            client
                .complete_with_tools(
                    &messages,
                    &crate::turn::tool_definitions(),
                    &CancelToken::new()
                )
                .err(),
            Some(ProviderError::ContextLimit)
        );
        assert_eq!(
            client.check_messages(&messages, MAX_REQUEST_BYTES as u64),
            Err(ProviderError::ContextLimit)
        );
        let roomy = LocalModelClient::new(
            LocalModelConfig {
                base_url: "http://127.0.0.1:9/v1".to_owned(),
                model: "m".to_owned(),
                context_tokens: u64::MAX / 2,
                max_output_tokens: 64,
            },
            None,
        )
        .expect("client");
        assert_eq!(
            roomy.check_messages(&messages, MAX_REQUEST_BYTES as u64),
            Err(ProviderError::RequestTooLarge)
        );
        assert_eq!(
            roomy
                .complete_with_tools(&messages, &[], &CancelToken::new())
                .err(),
            Some(ProviderError::InvalidConfiguration)
        );
    }

    /// Accept one connection within `end`, read one whole bounded request,
    /// and return the stream with 250 ms read and write deadlines.
    fn accept_fixture_request(listener: &TcpListener, end: Instant) -> std::net::TcpStream {
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < end, "fixture accept deadline");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fixture accept: {error}"),
            }
        };
        stream
            .set_nonblocking(false)
            .expect("blocking accepted stream");
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .expect("read deadline");
        stream
            .set_write_timeout(Some(Duration::from_millis(250)))
            .expect("write deadline");
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            assert!(
                bytes.len() <= MAX_REQUEST_BYTES && Instant::now() < end,
                "bounded request"
            );
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&bytes[..header_end]).expect("ASCII headers");
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().expect("body length"))
                    })
                    .expect("Content-Length");
                assert!(length <= MAX_REQUEST_BYTES, "bounded declared request");
                if bytes.len() >= header_end + 4 + length {
                    break;
                }
            }
            let count = stream.read(&mut buffer).expect("request bytes");
            assert!(count > 0, "request must be complete");
            bytes.extend_from_slice(&buffer[..count]);
        }
        stream
    }

    /// Give the server its own deadline and cancellation path, independent of HTTP timeouts.
    fn slow_response(
        drip_body: bool,
        streamed: bool,
    ) -> (LocalModelClient, mpsc::Sender<()>, thread::JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("private fixture");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("address");
        let (cancel, cancelled) = mpsc::channel();
        let task = thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(2);
            let mut stream = accept_fixture_request(&listener, end);
            if drip_body {
                let body: &[u8] = if streamed {
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"completed slowly\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                } else {
                    br#"{"choices":[{"message":{"role":"assistant","content":"completed slowly"},"finish_reason":"stop"}]}"#
                };
                let content_type = if streamed {
                    "Content-Type: text/event-stream\r\n"
                } else {
                    ""
                };
                let headers = format!(
                    "HTTP/1.1 200 OK\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if stream.write_all(headers.as_bytes()).is_err() {
                    return true;
                }
                for chunk in body.chunks(8) {
                    if Instant::now() >= end || stream.write_all(chunk).is_err() {
                        break;
                    }
                    match cancelled.recv_timeout(Duration::from_millis(80)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            } else {
                // Withhold headers, but never wait indefinitely for a faulty client.
                match cancelled.recv_timeout(Duration::from_secs(2)) {
                    Ok(())
                    | Err(mpsc::RecvTimeoutError::Disconnected | mpsc::RecvTimeoutError::Timeout) =>
                        {}
                }
            }
            true
        });
        let client = LocalModelClient::with_timeout(
            LocalModelConfig {
                base_url: format!("http://{address}/v1"),
                model: "fixture".to_owned(),
                context_tokens: 131_072,
                max_output_tokens: 32,
            },
            None,
            Duration::from_millis(250),
        )
        .expect("client");
        (client, cancel, task)
    }

    /// Observe fixture cleanup even when the timeout assertion is expected to fail.
    fn assert_bounded_transport_failure(drip_body: bool, streamed: bool) {
        let (client, cancel, task) = slow_response(drip_body, streamed);
        let started = Instant::now();
        let messages = [
            ModelMessage {
                role: ModelRole::System,
                content: BASE_PROMPT.to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "request".to_owned(),
            },
        ];
        let result = if streamed {
            client.complete_streaming(&messages, &mut |_: &str| {}, &CancelToken::new())
        } else {
            client.complete(&messages, &CancelToken::new())
        };
        let elapsed = started.elapsed();
        // Receiver disconnection also requests cleanup; either outcome is safe.
        // A disconnected receiver means the fixture is already exiting. The
        // bounded join below still observes completion in either case.
        let _ = cancel.send(());
        let end = Instant::now() + Duration::from_secs(3);
        while !task.is_finished() {
            assert!(Instant::now() < end, "fixture cleanup deadline");
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            task.join().expect("fixture thread"),
            "fixture received complete request"
        );
        assert_eq!(result, Err(ProviderError::Transport));
        assert!(
            elapsed < Duration::from_secs(1),
            "request exceeded its independent total deadline: {elapsed:?}"
        );
    }

    #[test]
    fn request_deadline_bounds_stalled_response_headers() {
        assert_bounded_transport_failure(false, false);
    }

    #[test]
    fn request_deadline_bounds_stalled_streamed_response_headers() {
        assert_bounded_transport_failure(false, true);
    }

    #[test]
    fn request_deadline_bounds_a_dripping_response_body() {
        assert_bounded_transport_failure(true, false);
    }

    #[test]
    fn request_deadline_bounds_a_dripping_streamed_body() {
        assert_bounded_transport_failure(true, true);
    }

    /// A server that answers with a head announcing a longer body and the
    /// first part of that body, signals once both are written, and then holds
    /// the connection open. The task returns whether the client closed it.
    fn partial_body_server() -> (
        LocalModelClient,
        mpsc::Receiver<()>,
        thread::JoinHandle<bool>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("private fixture");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("address");
        let (sent, partial_sent) = mpsc::channel();
        let task = thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(2);
            let mut stream = accept_fixture_request(&listener, end);
            let partial = br#"{"choices":[{"message":{"role":"assistant","content":"partial"#;
            let head = format!(
                "HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: {}
Connection: close

",
                partial.len() * 4
            );
            stream.write_all(head.as_bytes()).expect("write head");
            stream.write_all(partial).expect("write first part");
            stream.flush().expect("flush first part");
            // The test may already have failed and stopped listening.
            let _ignored = sent.send(());
            stream
                .set_read_timeout(Some(Duration::from_millis(25)))
                .expect("poll deadline");
            let mut buffer = [0_u8; 64];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => return true,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        if Instant::now() >= end {
                            return false;
                        }
                    }
                    Err(_) => return true,
                }
            }
        });
        let client = LocalModelClient::new(
            LocalModelConfig {
                base_url: format!("http://{address}/v1"),
                model: "fixture".to_owned(),
                context_tokens: 131_072,
                max_output_tokens: 32,
            },
            None,
        )
        .expect("client");
        (client, partial_sent, task)
    }

    /// The body reader is reached only once the head has arrived, so the
    /// cancel below is ordered after the head without any fixed sleep
    /// (ADR-0051).
    #[test]
    fn cancelling_while_the_rest_of_a_body_is_awaited_discards_the_part_read() {
        // Given a response whose head has arrived and whose body is held open
        // after its first part, with the reader waiting for the rest.
        let (client, partial_sent, task) = partial_body_server();
        let cancel = CancelToken::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("request runtime");
        let (result, elapsed) = runtime.block_on(async {
            let http = client.http_client().expect("request client");
            let response = client
                .send(
                    &http,
                    b"{}".to_vec(),
                    false,
                    client.authorization.as_ref(),
                    &cancel,
                )
                .await
                .expect("the head arrives");
            partial_sent
                .recv_timeout(Duration::from_secs(2))
                .expect("the server wrote the first part of the body");
            let mut read = std::pin::pin!(read_whole_body(response, MAX_RESPONSE_BYTES, &cancel));
            assert!(
                tokio::time::timeout(Duration::from_millis(100), read.as_mut())
                    .await
                    .is_err(),
                "the reader is still waiting for the held body"
            );

            // When the token is set.
            let started = Instant::now();
            cancel.cancel();
            let result = read.await;
            (result, started.elapsed())
        });
        drop(runtime);

        // Then the read is cancelled promptly, nothing of the body is
        // returned, and the connection was closed.
        assert_eq!(result, Err(ProviderError::Cancelled));
        assert!(
            elapsed < Duration::from_secs(1),
            "cancellation took {elapsed:?}"
        );
        let end = Instant::now() + Duration::from_secs(3);
        while !task.is_finished() {
            assert!(Instant::now() < end, "fixture cleanup deadline");
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            task.join().expect("fixture thread"),
            "the connection was closed"
        );
    }

    /// Non-streaming reply shapes captured from Ollama 0.40.16 (ADR-0064).
    /// Only the assistant text is shortened. `prompt_tokens_details` is an
    /// extra usage member the parser ignores.
    const OLLAMA_TEXT_REPLY: &str = r#"{"id":"chatcmpl-289","object":"chat.completion","created":1795102716,"model":"lfm2.5:latest","system_fingerprint":"fp_ollama","choices":[{"index":0,"message":{"role":"assistant","content":"Hello there"},"finish_reason":"length"}],"usage":{"prompt_tokens":26,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens":64,"total_tokens":90}}"#;

    const OLLAMA_TOOL_REPLY: &str = r#"{"id":"chatcmpl-484","object":"chat.completion","created":1795102716,"model":"lfm2.5:latest","system_fingerprint":"fp_ollama","choices":[{"index":0,"message":{"role":"assistant","content":"Listing.","tool_calls":[{"id":"call_yk0rwzuq","index":0,"type":"function","function":{"name":"workspace_list_directory","arguments":"{\"path\":\"src\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":89,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens":176,"total_tokens":265}}"#;

    #[test]
    fn an_ollama_text_reply_is_accepted() {
        let reply = parse_response(OLLAMA_TEXT_REPLY.as_bytes()).expect("Ollama text reply");
        assert_eq!(reply.content, "Hello there");
        assert_eq!(reply.finish_reason, "length");
        assert_eq!(
            reply.usage,
            Some(Usage {
                prompt_tokens: Some(26),
                completion_tokens: Some(64),
                total_tokens: Some(90),
            })
        );
    }

    #[test]
    fn an_ollama_tool_reply_parses_as_a_tool_call_and_a_text_request_refuses_it() {
        let reply = parse_tool_response(OLLAMA_TOOL_REPLY.as_bytes()).expect("Ollama tool reply");
        assert_eq!(reply.content, "Listing.");
        assert_eq!(reply.finish_reason, "tool_calls");
        assert_eq!(reply.tool_calls.len(), 1);
        // The captured call carries `index`; the runtime's strict parser
        // accepts what the adapter hands on.
        let call = crate::turn::ToolCall::parse(&reply.tool_calls[0]).expect("runtime parse");
        assert_eq!(call.call_id, "call_yk0rwzuq");
        assert_eq!(
            reply.tool_calls[0]["function"]["name"],
            "workspace_list_directory"
        );
        assert_eq!(
            parse_response(OLLAMA_TOOL_REPLY.as_bytes()),
            Err(ProviderError::UnsupportedToolCall)
        );
    }

    /// Only a numeric `index` is dropped: any other extra member, or an
    /// `index` of another type, reaches the strict runtime parser, which
    /// refuses it.
    #[test]
    fn only_a_numeric_position_index_is_dropped_from_a_buffered_call() {
        let reply_with = |extra: &str| {
            format!(
                r#"{{"choices":[{{"message":{{"role":"assistant","content":null,"tool_calls":[{{"id":"c1",{extra}"type":"function","function":{{"name":"workspace_read_file","arguments":"{{\"path\":\"a.txt\"}}"}}}}]}},"finish_reason":"tool_calls"}}]}}"#
            )
        };
        for (extra, accepted) in [
            ("", true),
            (r#""index":3,"#, true),
            (r#""index":"0","#, false),
            (r#""index":-1,"#, false),
            (r#""extra":0,"#, false),
        ] {
            let reply = parse_tool_response(reply_with(extra).as_bytes()).expect("tool reply");
            assert_eq!(
                crate::turn::ToolCall::parse(&reply.tool_calls[0]).is_ok(),
                accepted,
                "{extra}"
            );
        }
    }

    /// Ollama's documented default address is a valid local endpoint.
    #[test]
    fn the_default_ollama_endpoint_is_a_valid_local_endpoint() {
        assert!(validate_base_url("http://127.0.0.1:11434/v1").is_ok());
        assert!(validate_base_url("http://localhost:11434/v1").is_err());
    }
}

#[cfg(test)]
mod web_fetch_tool_tests {
    use super::*;
    use crate::turn::{
        command_tool_definition, create_directory_tool_definition, create_tool_definition,
        delete_tool_definition, edit_tool_definition, multi_edit_tool_definition,
        rename_tool_definition, tool_definitions, web_fetch_tool_definition,
    };

    /// ADR-0098: the Chat Completions request builder accepts the ten tools
    /// the runtime can advertise, with `web_fetch` last, and still refuses an
    /// eleventh.
    #[test]
    fn the_chat_completions_builder_accepts_ten_tool_definitions_and_refuses_eleven() {
        let mut tools = tool_definitions();
        tools.push(edit_tool_definition());
        tools.push(multi_edit_tool_definition());
        tools.push(create_tool_definition());
        tools.push(rename_tool_definition());
        tools.push(delete_tool_definition());
        tools.push(create_directory_tool_definition());
        tools.push(command_tool_definition());
        tools.push(web_fetch_tool_definition());
        assert_eq!(tools.len(), 10);
        let client = LocalModelClient::new(
            LocalModelConfig {
                base_url: "http://127.0.0.1:1234/v1".to_owned(),
                model: "m".to_owned(),
                context_tokens: 131_072,
                max_output_tokens: 64,
            },
            None,
        )
        .expect("client");
        let messages = [
            ModelMessage {
                role: ModelRole::System,
                content: crate::prompt::BASE_PROMPT.to_owned(),
            },
            ModelMessage {
                role: ModelRole::User,
                content: "fetch the page".to_owned(),
            },
        ];
        let body = client.tool_body(&messages, &tools, false).expect("body");
        let value: Value = serde_json::from_slice(&body).expect("json");
        let wire = value["tools"].as_array().expect("tools");
        assert_eq!(wire.len(), 10);
        assert_eq!(wire[9]["function"]["name"], "web_fetch");

        tools.push(web_fetch_tool_definition());
        assert_eq!(
            client.tool_body(&messages, &tools, false).err(),
            Some(ProviderError::InvalidConfiguration)
        );
    }
}
