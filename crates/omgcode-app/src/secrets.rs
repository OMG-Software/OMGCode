//! Handling of the optional local server token.
//!
//! The token is a credential. It is never logged, serialized, or placed in an
//! error message; the only treatment here is removing it from untrusted text.

use std::env;

use crate::error::{AppError, ErrorKind};

/// The environment variable that carries the optional local server token.
const TOKEN_VARIABLE: &str = "OMGCODE_LM_TOKEN";

/// Read the trusted operator's credential input from the environment.
///
/// This performs the process-environment read; the caller decides when. An
/// unset or empty variable gives `None`. A non-Unicode value is a
/// `configuration` error that does not include the value.
pub fn read_lm_token() -> Result<Option<String>, AppError> {
    match env::var(TOKEN_VARIABLE) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(AppError::new(
            ErrorKind::Configuration,
            "Server token must be Unicode",
        )),
    }
}

/// Remove the configured credential from untrusted text before retention or egress.
///
/// If replacement would itself contain the credential, the text is discarded
/// and an empty string is returned.
pub fn redact_known_secret(text: &str, token: Option<&str>) -> String {
    match token.filter(|token| !token.is_empty()) {
        Some(token) => {
            let result = text.replace(token, "[REDACTED]");
            if result.contains(token) {
                String::new()
            } else {
                result
            }
        }
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_token_is_replaced_everywhere() {
        assert_eq!(
            redact_known_secret("a secret-1 b secret-1", Some("secret-1")),
            "a [REDACTED] b [REDACTED]"
        );
    }

    #[test]
    fn text_is_unchanged_without_a_token() {
        assert_eq!(redact_known_secret("plain", None), "plain");
        assert_eq!(redact_known_secret("plain", Some("")), "plain");
    }

    #[test]
    fn text_is_discarded_when_replacement_recreates_the_token() {
        assert_eq!(redact_known_secret("xx[REDACTED]", Some("x[RED")), "");
    }
}
