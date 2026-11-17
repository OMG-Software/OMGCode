mod stream;

use crate::prompt::{BASE_PROMPT, ModelMessage, ModelRole};
use crate::storage::parse_strict_json;
use crate::turn::{
    CancelToken, ModelError, ToolDefinition, ToolModel, ToolModelReply, ToolModelRequest,
};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;

const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a request looks at its cancel token while it waits. It is also
/// the longest a cancellation waits for the next wake-up.
const CANCEL_POLL: Duration = Duration::from_millis(25);
const MESSAGE_OVERHEAD_ESTIMATE: u64 = 256;
const PROMPT_HEADROOM_ESTIMATE: u64 = 512;
/// Upper bound on advertised tool definitions in one request.
const MAX_TOOL_DEFINITIONS: usize = 8;
/// Upper bound on raw tool calls accepted from one response before validation.
/// The turn runtime applies its own, smaller per-round limit.
const MAX_RESPONSE_TOOL_CALLS: usize = 64;

/// Explicit settings for one local LM Studio Chat Completions connection.
///
/// `context_tokens` is a required conservative budget supplied by the caller.
/// This adapter estimates input tokens from UTF-8 bytes and fixed overhead; it
/// does not claim to reproduce the selected model's tokenizer.
pub struct LocalModelConfig {
    /// A literal loopback HTTP URL with an explicit port and `/v1` path.
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
    model: String,
    context_tokens: u64,
    max_output_tokens: u32,
    authorization: Option<HeaderValue>,
    request_timeout: Duration,
}

impl fmt::Debug for LocalModelClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalModelClient")
            .field("model", &self.model)
            .field("context_tokens", &self.context_tokens)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("has_token", &self.authorization.is_some())
            .finish()
    }
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
    #[error("local model request exceeds 256 KiB")]
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
    /// A streamed response ended before its `[DONE]` marker.
    #[error("local model stream ended before completion")]
    IncompleteStream,
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
    max_tokens: u32,
}

/// The text request plus the advertised function tools.
#[derive(Serialize)]
struct ToolChatRequest<'a> {
    model: &'a str,
    messages: &'a [ModelMessage],
    stream: bool,
    max_tokens: u32,
    tools: &'a [WireTool<'a>],
    tool_choice: &'static str,
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
        let mut endpoint = validate_base_url(&config.base_url)?;
        if config.model.is_empty()
            || config.model.len() > 256
            || config.model.chars().any(char::is_control)
            || config.context_tokens == 0
            || config.max_output_tokens == 0
        {
            return Err(ProviderError::InvalidConfiguration);
        }
        endpoint.set_path("/v1/chat/completions");

        let authorization = token
            .map(|token| {
                if token.is_empty() || token.chars().any(char::is_control) {
                    return Err(ProviderError::InvalidToken);
                }
                let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| ProviderError::InvalidToken)?;
                header.set_sensitive(true);
                Ok(header)
            })
            .transpose()?;

        Ok(Self {
            endpoint,
            model: config.model,
            context_tokens: config.context_tokens,
            max_output_tokens: config.max_output_tokens,
            authorization,
            request_timeout,
        })
    }

    /// The selected model identifier. This is descriptive, not an entitlement.
    pub fn model(&self) -> &str {
        &self.model
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

    /// Encode the text request with the chosen `stream` setting.
    fn text_body(
        &self,
        messages: &[ModelMessage],
        streaming: bool,
    ) -> Result<Vec<u8>, ProviderError> {
        self.check_messages(messages, 0)?;
        serde_json::to_vec(&ChatRequest {
            model: &self.model,
            messages,
            stream: streaming,
            max_tokens: self.max_output_tokens,
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
            u64::try_from(tool_bytes.len()).map_err(|_| ProviderError::RequestTooLarge)?,
        )?;
        serde_json::to_vec(&ToolChatRequest {
            model: &self.model,
            messages,
            stream: streaming,
            max_tokens: self.max_output_tokens,
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
        cancel: &CancelToken,
    ) -> Result<Response, ProviderError> {
        let mut request = client
            .post(self.endpoint.clone())
            .timeout(self.request_timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if streaming {
            request = request.header(reqwest::header::ACCEPT, "text/event-stream");
        }
        if let Some(authorization) = &self.authorization {
            request = request.header(AUTHORIZATION, authorization.clone());
        }
        until_cancelled(request.body(body).send(), cancel)
            .await?
            .map_err(|_| ProviderError::Transport)
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
        let response = self.send(&client, body, false, cancel).await?;
        read_whole_body(response, cancel).await
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
        let mut response = self.send(&client, body, true, cancel).await?;
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

/// Read the whole body of a non-streaming response within 1 MiB.
///
/// A non-success status is refused before the body is read. The token is
/// checked before each piece of the body as well as while each is awaited; a
/// cancelled read discards what it read. Dropping the response on any return
/// closes the connection.
async fn read_whole_body(
    mut response: Response,
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
        if total > MAX_RESPONSE_BYTES {
            return Err(ProviderError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
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
        | ProviderError::IncompleteStream => ModelError::Transport,
        ProviderError::Cancelled => ModelError::Cancelled,
        ProviderError::InvalidConfiguration
        | ProviderError::InvalidToken
        | ProviderError::ResponseTooLarge
        | ProviderError::MalformedResponse
        | ProviderError::UnsupportedToolCall => ModelError::InvalidResponse,
    }
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
    let is_stream = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"));
    if is_stream {
        Ok(())
    } else {
        Err(ProviderError::MalformedResponse)
    }
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
        Some(Value::Array(calls)) if calls.len() <= MAX_RESPONSE_TOOL_CALLS => calls.clone(),
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

/// Destinations the endpoint rule refuses. The provider profile tests use the
/// same list, so the rule is proved to be shared.
#[cfg(test)]
pub(crate) const REFUSED_ENDPOINTS: &[&str] = &[
    "http://localhost:1234/v1",
    "http://192.0.2.1:1234/v1",
    "https://127.0.0.1:1234/v1",
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
                bytes.len() <= 256 * 1024 && Instant::now() < end,
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
                assert!(length <= 256 * 1024, "bounded declared request");
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
                .send(&http, b"{}".to_vec(), false, &cancel)
                .await
                .expect("the head arrives");
            partial_sent
                .recv_timeout(Duration::from_secs(2))
                .expect("the server wrote the first part of the body");
            let mut read = std::pin::pin!(read_whole_body(response, &cancel));
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
}
