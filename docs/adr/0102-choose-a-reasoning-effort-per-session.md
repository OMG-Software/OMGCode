# 0102: Choose a reasoning effort per session

- Status: Accepted
- Date: 2026-11-26
- Amends: [ADR-0089](0089-choose-the-provider-and-model-in-the-side-panel-footer.md) decision 1, for the box's contents only;
  [ADR-0087](0087-show-a-per-part-context-estimate-for-the-open-session.md) decision 3, for what framing counts

## Context

The operator asked for a way to set the reasoning effort ("Medium, High,
Extra High etc.") in the provider and model settings, as a dedicated
dropdown. OMGCode sent no effort field, so every reasoning model ran at its
provider's default.

Facts established from each provider's official documentation on
2026-11-26, before the decision:

- OpenAI's Chat Completions reference documents `reasoning_effort` with the
  values `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, and `max`, and
  says the accepted values are model-dependent; some models answer a value
  they do not support with HTTP 400. The Responses API takes the same values
  as `reasoning.effort`. OpenAI's open-source Codex client, which uses the
  ChatGPT plan route, sends `reasoning: {"effort": …}` with the same names.
- Ollama's OpenAI compatibility page lists `reasoning_effort` for
  `/v1/chat/completions`, with model-defined values and aliases. Ollama
  Cloud serves the same API; its effort handling was not checked separately.
- LM Studio's 0.4.8 release notes add `reasoning_effort` to its
  OpenAI-compatible `/v1/chat/completions`. Older LM Studio versions are not
  documented to accept it.
- Google's Gemini OpenAI compatibility, xAI, OpenRouter, Z.ai, Moonshot, and
  MiniMax document a top-level `reasoning_effort`, each with a
  model-dependent subset of the same names; several say an unsupported
  value or `none` on a model that always reasons is answered with HTTP 400.
- Anthropic's OpenAI SDK compatibility layer lists `reasoning_effort` as
  ignored. DeepSeek documents `reasoning_effort`, but a `thinking` field
  takes precedence, and this client sends `thinking: {"type": "disabled"}`
  to DeepSeek (ADR-0092), so an effort would have no effect.
- Tencent Hunyuan, Meta's Llama API, and Thinking Machines' Tinker document
  no effort field. ADR-0092 records that Tinker ignores fields it does not
  list.
- An operator-typed `openai_compatible` server and a loopback server can be
  any software; their documentation cannot be checked in advance.

No provider's effort handling was exercised with a real request.

## Decision

1. **Core owns the values.** `omgcode_core::provider::ReasoningEffort` has
   the seven documented names: `none`, `minimal`, `low`, `medium`, `high`,
   `xhigh`, and `max`. It is a closed enum: no other text reaches a request,
   and serde refuses any other name on every interface that reads it. "No
   effort" is `Option::None`, which sends no field and leaves the provider's
   default in force; it is not the provider's `none` value.
2. **Core decides which endpoints take an effort.** `ModelEndpoint` reports
   whether it accepts one: every loopback endpoint and every operator-typed
   HTTPS endpoint; of the fixed endpoints, those whose provider documents the
   field (Ollama Cloud, OpenAI, Google, Moonshot, MiniMax, OpenRouter, Z.ai,
   and xAI). Anthropic, DeepSeek, Tencent, Meta, and Tinker do not. The list
   is an allowlist, so a provider added later takes no effort until its
   documentation is checked. The endpoint text decides, not the profile
   kind: `ModelEndpoint::parse` matches the fixed table first, so an
   `openai_compatible` address typed as exactly a fixed endpoint (one
   trailing `/` allowed) takes that endpoint's rule. One typed as the
   Anthropic or DeepSeek endpoint therefore takes no effort either.
3. **The client sends the effort only when one is chosen.**
   `LocalModelClient::with_reasoning_effort` sets it after construction and
   refuses an effort for an endpoint that does not take one with
   `InvalidConfiguration`, before anything is sent. A Chat Completions request
   then carries `"reasoning_effort": "<value>"`; a plan-route Responses
   request carries `"reasoning": {"effort": "<value>"}`. With no effort,
   both bodies are byte-for-byte as before. The field's bytes (a fixed upper
   bound, `effort_field_bytes`) count in the client's request size and
   context checks, in the turn's pre-turn compaction trigger, and in the
   desktop **Context** tab, where they are part of framing (amending ADR-0087
   decision 3), so the parts still add up to the trigger's estimate.
4. **The value is not adjusted to the model.** Accepted values depend on the
   model, which core cannot know. The server decides, and a refusal is the
   existing `provider` failure with its HTTP status. No value is rewritten,
   retried, or silently dropped.
5. **The request manifest records the effort.** `RequestManifest` gains an
   optional `reasoning_effort`, omitted when no effort was sent, so journals
   without one keep their bytes and session schema version 3 is unchanged. As
   with `skills`, a build older than this one refuses a journal that contains
   it.
6. **Only the chat requests use it.** The compaction summary client sends no
   effort. A summary is a bounded utility request that the manifest does not
   describe, and a high effort on the plan route can use most of its 120-second
   request deadline.
7. **The desktop records it per session.** `SessionRecord` in
   `desktop-v1/session-connections.json` gains an optional
   `reasoning_effort`, omitted when none is chosen, so files written before
   this change still load and version 1 is kept. An older build reads a file
   that holds an effort as unreadable, reports it, and treats it as empty,
   as ADR-0071 decision 6 already requires. The new `main` command
   `session_effort_select` records it without a dialog, like
   `session_model_select`: an effort grants nothing. An effort the
   session's connection does not take is refused with `invalid_request`;
   `null`, which records no effort ("Default"), is accepted on any
   connection, including one that takes none. It holds
   the one-change-at-a-time guard that a provider choice holds, so the
   connection it checks is the one the record keeps, and it is refused with
   `settings_change_running` while another settings change runs. Choosing
   another provider clears it with the model, because accepted values depend
   on the provider and model.
8. **Rust tells the window what to offer.** `connection_status` returns the
   session's recorded `reasoning_effort` and `reasoning_efforts`, the values
   its connection accepts: all seven, or none. `chat_send` takes the effort in
   its settings beside the model, as the composer already sends the model it
   reads from the box. Rust refuses an effort the turn's connection does not
   take before the turn starts.
9. **The model box has an Effort dropdown.** Under **Model**, a dedicated
   **Effort** dropdown offers "Default" and the accepted values, labelled
   None, Minimal, Low, Medium, High, Extra high, and Max. It is disabled with
   the other dropdowns (no session that accepts a message, a turn running, or
   a choice in flight) and when the connection takes no effort, where a note
   says so. A launch argument that fixes the connection does not disable it,
   as it does not disable the model.
10. **The trusted CLI matches.** `omgcode chat --reasoning-effort <value>`
    sends the same field through the same client; without the flag nothing is
    sent.

## Alternatives considered

- **Send `reasoning_effort` to every endpoint.** Rejected: Anthropic ignores
  it and DeepSeek overrides it, so the dropdown would claim an effect that
  does not happen. Refusing it there tells the operator the truth.
- **Per-provider value lists.** Rejected for now: every provider's list is
  model-dependent, so a per-provider list would still offer values a model
  refuses, and would go stale with each model release. The server's HTTP
  error names the problem.
- **Map unsupported values to the nearest supported one.** Rejected: a
  silently changed setting is worse than a refused one, and core does not
  know the model's list.
- **Read the effort from the session record in `chat_send`.** It would avoid
  a field in the send settings. Rejected for consistency: the model is sent
  in the settings, and ADR-0089 decision 3 makes the box the composer's
  source for each send. Rust still validates the value and the connection.
- **Keep the effort when the provider changes.** Rejected: the next
  provider may take none, or not that value, and the model is cleared anyway.
- **Use the effort for compaction.** Rejected for decision 6's reasons; it
  can be revisited if summaries prove too shallow.
- **Raise the session schema version.** Rejected: an omitted optional field
  keeps every existing journal valid, as `skills` did.

## Consequences

- The operator can choose an effort for any session whose connection
  documents one. "Default" keeps the previous behaviour exactly.
- A value the model does not support fails the turn with the provider's
  HTTP status, before any reply; the operator chooses another value.
- Ollama Cloud's and LM Studio's handling, and every hosted provider's, were
  not observed with a real request. LM Studio before 0.4.8 may ignore or
  refuse the field.
- An older build refuses a journal with a recorded effort and reads a
  session-connections file with an effort as unreadable.

## References

- [Desktop interface](../desktop-interface.md#right-hand-panel)
- [OpenAI: Create chat completion](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create/)
- [OpenAI: Reasoning models](https://developers.openai.com/api/docs/guides/reasoning)
- [openai/codex `ReasoningEffort`](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models.rs)
- [Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility)
- [LM Studio 0.4.8 changelog](https://lmstudio.ai/changelog/lmstudio/lmstudio-v0.4.8)
- [Gemini OpenAI compatibility](https://ai.google.dev/gemini-api/docs/openai)
- [Anthropic OpenAI SDK compatibility](https://docs.anthropic.com/en/api/openai-sdk)
- [xAI reasoning](https://docs.x.ai/developers/model-capabilities/text/reasoning)
- [OpenRouter reasoning tokens](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)
- [DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/)
- [Z.ai chat completion](https://docs.z.ai/api-reference/llm/chat-completion)
- [Moonshot quickstart](https://platform.moonshot.ai/docs/api/chat.en-US)
- [MiniMax chat completion](https://platform.minimax.io/docs/api-reference/text-chat-openai)
- [ADR-0071](0071-choose-the-provider-and-model-per-session-and-add-ollama-cloud.md),
  [ADR-0089](0089-choose-the-provider-and-model-in-the-side-panel-footer.md),
  [ADR-0092](0092-add-hosted-providers-and-an-openai-compatible-connection.md),
  [ADR-0093](0093-sign-in-with-chatgpt-and-use-the-plan-through-the-responses-api.md)
