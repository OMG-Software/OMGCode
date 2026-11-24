//! The hosted endpoints the model adapter may send to (ADR-0071, ADR-0092).
//!
//! Every hosted provider is reached through its OpenAI-compatible Chat
//! Completions API with a bearer key. Each named provider has one fixed base
//! URL, checked against its official documentation on 2026-11-24; an
//! `openai_compatible` profile records a base URL the operator typed, which
//! must pass [`validate_custom_url`]. A profile's endpoint never changes, so a
//! stored key goes only to the destination recorded with it (ADR-0048).
//!
//! The base URL is the part before `/chat/completions` and `/models`.

use std::net::IpAddr;

use super::ProviderError;
use reqwest::Url;

/// Ollama Cloud's OpenAI-compatible base URL (ADR-0071).
pub const OLLAMA_CLOUD_ENDPOINT: &str = "https://ollama.com/v1";
/// Anthropic's OpenAI SDK compatibility layer, which Anthropic describes as
/// not production-ready.
pub const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1";
/// OpenAI's API.
pub const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";
/// Google Gemini's OpenAI compatibility layer (beta).
pub const GOOGLE_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
/// Meta's Llama API compatibility endpoint (preview).
pub const META_ENDPOINT: &str = "https://api.llama.com/compat/v1";
/// Moonshot AI (Kimi), international platform.
pub const MOONSHOT_ENDPOINT: &str = "https://api.moonshot.ai/v1";
/// Tencent Hunyuan's OpenAI-compatible API.
pub const TENCENT_ENDPOINT: &str = "https://api.hunyuan.cloud.tencent.com/v1";
/// Thinking Machines Lab's Tinker OpenAI-compatible API (beta, for testing).
pub const THINKING_MACHINES_ENDPOINT: &str =
    "https://tinker.thinkingmachines.dev/services/tinker-prod/oai/api/v1";
/// MiniMax, international platform.
pub const MINIMAX_ENDPOINT: &str = "https://api.minimax.io/v1";
/// OpenRouter.
pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1";
/// Z.ai's general API (GLM models).
pub const Z_AI_ENDPOINT: &str = "https://api.z.ai/api/paas/v4";
/// DeepSeek. Its documented base has no `/v1`.
pub const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com";
/// xAI (Grok).
pub const XAI_ENDPOINT: &str = "https://api.x.ai/v1";

/// The request field that carries the output budget.
///
/// OpenAI rejects `max_tokens` for its reasoning models and documents
/// `max_completion_tokens`; Moonshot and MiniMax document `max_tokens` as
/// deprecated; Tinker documents only `max_completion_tokens` and ignores
/// fields it does not list, so `max_tokens` would leave its output unbounded. DeepSeek and Z.ai document only `max_tokens`, so the field is
/// chosen per endpoint rather than sending both.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputLimitField {
    /// `max_tokens`, which local servers and most providers accept.
    MaxTokens,
    /// `max_completion_tokens`.
    MaxCompletionTokens,
}

/// Every fixed hosted endpoint and the output field it takes.
const FIXED_ENDPOINTS: &[(&str, OutputLimitField)] = &[
    (OLLAMA_CLOUD_ENDPOINT, OutputLimitField::MaxTokens),
    (ANTHROPIC_ENDPOINT, OutputLimitField::MaxTokens),
    (OPENAI_ENDPOINT, OutputLimitField::MaxCompletionTokens),
    (GOOGLE_ENDPOINT, OutputLimitField::MaxTokens),
    (META_ENDPOINT, OutputLimitField::MaxTokens),
    (MOONSHOT_ENDPOINT, OutputLimitField::MaxCompletionTokens),
    (TENCENT_ENDPOINT, OutputLimitField::MaxTokens),
    (
        THINKING_MACHINES_ENDPOINT,
        OutputLimitField::MaxCompletionTokens,
    ),
    (MINIMAX_ENDPOINT, OutputLimitField::MaxCompletionTokens),
    (OPENROUTER_ENDPOINT, OutputLimitField::MaxTokens),
    (Z_AI_ENDPOINT, OutputLimitField::MaxTokens),
    (DEEPSEEK_ENDPOINT, OutputLimitField::MaxTokens),
    (XAI_ENDPOINT, OutputLimitField::MaxCompletionTokens),
];

/// Fixed endpoints whose provider reasons ("thinks") by default and then
/// answers HTTP 400 to a tool request that does not send every earlier
/// turn's `reasoning_content` back. This client keeps no reasoning text, so
/// requests to these endpoints send `"thinking": {"type": "disabled"}`
/// (ADR-0092). DeepSeek documents both the default and the 400.
const THINKING_OFF_ENDPOINTS: &[&str] = &[DEEPSEEK_ENDPOINT];

/// Whether requests to `text` must switch the provider's thinking mode off.
pub(crate) fn disables_thinking(text: &str) -> bool {
    THINKING_OFF_ENDPOINTS
        .iter()
        .any(|fixed| matches_fixed(text, fixed))
}

/// Longest endpoint text either hosted rule reads, checked before parsing.
const MAX_ENDPOINT_BYTES: usize = 256;

/// Whether `text` is `fixed` exactly, with or without one trailing `/`.
pub(crate) fn matches_fixed(text: &str, fixed: &str) -> bool {
    text == fixed || text.strip_suffix('/') == Some(fixed)
}

/// The fixed endpoint `text` names, with its output field, if any. The match
/// is exact (one trailing `/` allowed), so no host, port, path, user
/// information, query, fragment, scheme, or letter-case variant matches.
pub(crate) fn fixed_endpoint(text: &str) -> Option<(Url, OutputLimitField)> {
    let field = FIXED_ENDPOINTS
        .iter()
        .find(|(fixed, _)| matches_fixed(text, fixed))
        .map(|(_, field)| *field)?;
    Url::parse(text).ok().map(|url| (url, field))
}

/// The rule for an operator-typed hosted endpoint (`openai_compatible`).
///
/// The text must be at most 256 bytes of `https://` URL with a host, an
/// optional port, and an optional path, and nothing else: no user
/// information, query, fragment, percent-encoding, whitespace, control
/// character, or empty path segment. It must also be exactly what the URL
/// parser would write (one trailing `/` aside), so the text the operator
/// confirms is the URL each request uses: an upper-case host, a default port,
/// a `.` or `..` segment, or a non-ASCII host is refused rather than rewritten.
/// Port 0, which no request can reach, is refused as the loopback rule does.
/// A loopback or unspecified address, or a `localhost` name, is refused too:
/// a server on this computer uses a local kind, and a hosted connection's
/// confirmation says that prompts leave this computer.
/// Plain HTTP is refused: the key and every prompt would cross the network
/// in clear text. A loopback HTTP server uses a local kind instead.
///
/// # Errors
///
/// `InvalidConfiguration` for any other text.
pub(crate) fn validate_custom_url(text: &str) -> Result<Url, ProviderError> {
    if text.len() > MAX_ENDPOINT_BYTES
        || !text.starts_with("https://")
        || text
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || text.contains(['%', '@', '?', '#', '\\'])
    {
        return Err(ProviderError::InvalidConfiguration);
    }
    let url = Url::parse(text).map_err(|_| ProviderError::InvalidConfiguration)?;
    let written = url.as_str();
    if url.scheme() != "https"
        || url.host_str().is_none_or(str::is_empty)
        || url.cannot_be_a_base()
        || url.port() == Some(0)
        || url.host_str().is_some_and(is_this_computer)
        || !(written == text || written.strip_suffix('/') == Some(text))
        || url.path().contains("//")
    {
        return Err(ProviderError::InvalidConfiguration);
    }
    Ok(url)
}

/// Whether `host`, as the URL parser writes it, names this computer: a
/// loopback or unspecified address, including an IPv4-mapped one, or
/// `localhost` and its subdomains. An IPv6 host keeps its brackets. A name
/// with trailing root dots (`localhost.`) resolves the same, so they are
/// dropped before the name is compared.
fn is_this_computer(host: &str) -> bool {
    let name = host.trim_end_matches('.');
    if name == "localhost" || name.ends_with(".localhost") {
        return true;
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => address.is_loopback() || address.is_unspecified(),
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                || address.is_unspecified()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback() || mapped.is_unspecified())
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixed_endpoint_is_https_and_passes_the_custom_rule() {
        for (endpoint, _) in FIXED_ENDPOINTS {
            assert!(validate_custom_url(endpoint).is_ok(), "{endpoint}");
            let (url, _) = fixed_endpoint(endpoint).expect(endpoint);
            assert_eq!(url.scheme(), "https");
            assert!(
                fixed_endpoint(&format!("{endpoint}/")).is_some(),
                "{endpoint}"
            );
            assert!(
                fixed_endpoint(&format!("{endpoint}//")).is_none(),
                "{endpoint}"
            );
            assert!(
                fixed_endpoint(&format!("{endpoint}x")).is_none(),
                "{endpoint}"
            );
            assert!(
                fixed_endpoint(&endpoint.to_ascii_uppercase()).is_none(),
                "{endpoint}"
            );
        }
        assert_eq!(
            fixed_endpoint(OPENAI_ENDPOINT).map(|(_, field)| field),
            Some(OutputLimitField::MaxCompletionTokens)
        );
        assert_eq!(
            fixed_endpoint(DEEPSEEK_ENDPOINT).map(|(_, field)| field),
            Some(OutputLimitField::MaxTokens)
        );
    }

    #[test]
    fn the_custom_rule_accepts_plain_https_base_urls() {
        for text in [
            "https://provider-api.example.org/v1",
            "https://provider-api.example.org/v1/",
            "https://provider-api.example.org",
            "https://provider-api.example.org:8443/openai/v1",
            "https://192.0.2.10/v1",
            "https://[2001:db8::1]:8443/v1",
        ] {
            assert!(validate_custom_url(text).is_ok(), "{text}");
        }
    }

    #[test]
    fn the_custom_rule_refuses_anything_that_could_mislead_or_leak() {
        let long = format!("https://example.org/{}", "a".repeat(250));
        for text in [
            "http://provider-api.example.org/v1",
            "HTTPS://provider-api.example.org/v1",
            "https://Provider-API.example.org/v1",
            "https://provider-api.example.org:443/v1",
            "https://provider-api.example.org:0/v1",
            "https://127.0.0.1:8443/v1",
            "https://127.1.2.3/v1",
            "https://0.0.0.0:8443/v1",
            "https://[::1]:9443/v1",
            "https://[::]/v1",
            "https://[::ffff:7f00:1]/v1",
            "https://localhost:8443/v1",
            "https://llm.localhost/v1",
            "https://localhost.:8443/v1",
            "https://llm.localhost./v1",
            "https://user@provider-api.example.org/v1",
            "https://user:pass@provider-api.example.org/v1",
            "https://provider-api.example.org/v1?key=1",
            "https://provider-api.example.org/v1#f",
            "https://provider-api.example.org/v1/../v2",
            "https://provider-api.example.org/./v1",
            "https://provider-api.example.org//v1",
            "https://provider-api.example.org/v%31",
            "https://exämple.org/v1",
            " https://provider-api.example.org/v1",
            "https://provider-api.example.org/v1\n",
            "https://provider-api.example.org/v 1",
            "https:///v1",
            "https://",
            "ftp://provider-api.example.org/v1",
            "",
            long.as_str(),
        ] {
            assert_eq!(
                validate_custom_url(text),
                Err(ProviderError::InvalidConfiguration),
                "{text:?}"
            );
        }
    }
}
