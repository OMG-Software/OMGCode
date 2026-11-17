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

/// Text that replaces each complete occurrence of the token.
const REDACTION: &str = "[REDACTED]";

/// Remove the known token from text that arrives in pieces.
///
/// A token can be split across two pieces, so the redactor withholds the
/// longest tail of its unemitted text that equals a proper prefix of the token
/// until more text arrives. Every complete occurrence is replaced with
/// `[REDACTED]` left to right, as [`redact_known_secret`] does, so for any
/// split of a text the concatenation of every [`push`](Self::push) result and
/// [`finish`](Self::finish) equals `redact_known_secret` of the whole text.
///
/// One case differs. `redact_known_secret` discards the whole text when the
/// redacted result still contains the token (for example when `[REDACTED]`
/// contains it, or the replacement joins its neighbours into it). Already
/// emitted text cannot be discarded, so the redactor remembers the last
/// `token.len() - 1` bytes it emitted; if that tail plus the next output would
/// contain the token, it emits nothing for that output and nothing further,
/// including from `finish`. Its output therefore never contains the token.
///
/// With no token, or an empty one, text passes through. The redactor borrows
/// the token, holds at most `token.len() - 1` bytes of withheld text, and has
/// no `Debug` output.
pub struct StreamRedactor<'a> {
    token: Option<&'a str>,
    /// Unemitted raw text: a proper prefix of the token, or empty.
    withheld: String,
    /// The last `token.len() - 1` bytes already emitted.
    emitted_tail: Vec<u8>,
    poisoned: bool,
}

impl<'a> StreamRedactor<'a> {
    /// Start a redactor for one model request.
    pub fn new(token: Option<&'a str>) -> Self {
        Self {
            token: token.filter(|token| !token.is_empty()),
            withheld: String::new(),
            emitted_tail: Vec::new(),
            poisoned: false,
        }
    }

    /// Accept one piece and return the text that is safe to emit now.
    pub fn push(&mut self, text: &str) -> String {
        if self.poisoned {
            return String::new();
        }
        let Some(token) = self.token else {
            return text.to_owned();
        };
        self.withheld.push_str(text);
        let mut output = String::new();
        let mut cursor = 0;
        while let Some(found) = self
            .withheld
            .get(cursor..)
            .and_then(|rest| rest.find(token))
        {
            let start = cursor.saturating_add(found);
            output.push_str(self.withheld.get(cursor..start).unwrap_or_default());
            output.push_str(REDACTION);
            cursor = start.saturating_add(token.len());
        }
        // No complete occurrence remains in `rest`, so only a proper prefix of
        // the token at its end can still become one.
        let rest = self.withheld.get(cursor..).unwrap_or_default();
        let split = rest.len().saturating_sub(prefix_suffix_len(rest, token));
        output.push_str(rest.get(..split).unwrap_or_default());
        let held = rest.get(split..).unwrap_or_default().to_owned();
        self.withheld = held;
        self.release(output)
    }

    /// End the text and return the withheld tail, which cannot hold a whole token.
    pub fn finish(mut self) -> String {
        if self.poisoned {
            return String::new();
        }
        let tail = std::mem::take(&mut self.withheld);
        self.release(tail)
    }

    /// Emit `output` unless it would complete the token with the emitted tail.
    fn release(&mut self, output: String) -> String {
        let Some(token) = self.token else {
            return output;
        };
        let mut window = std::mem::take(&mut self.emitted_tail);
        window.extend_from_slice(output.as_bytes());
        if window
            .windows(token.len())
            .any(|candidate| candidate == token.as_bytes())
        {
            self.poisoned = true;
            self.withheld.clear();
            return String::new();
        }
        let keep = token.len().saturating_sub(1);
        let start = window.len().saturating_sub(keep);
        self.emitted_tail = window.split_off(start);
        output
    }
}

/// The length of the longest suffix of `text` that is a proper prefix of `token`.
///
/// The comparison is on bytes. A suffix that would start inside a character of
/// `text` is skipped, so the caller can cut `text` at the result.
fn prefix_suffix_len(text: &str, token: &str) -> usize {
    let longest = token.len().saturating_sub(1).min(text.len());
    (1..=longest)
        .rev()
        .find(|length| {
            let start = text.len().saturating_sub(*length);
            matches!(
                (text.get(start..), token.as_bytes().get(..*length)),
                (Some(suffix), Some(prefix)) if suffix.as_bytes() == prefix
            )
        })
        .unwrap_or(0)
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

    /// Feed `pieces` through one redactor and join every result with `finish`.
    fn streamed(token: &str, pieces: &[&str]) -> String {
        let mut redactor = StreamRedactor::new(Some(token));
        let mut output = String::new();
        for piece in pieces {
            output.push_str(&redactor.push(piece));
        }
        output.push_str(&redactor.finish());
        output
    }

    /// Token and text pairs: split tokens, repeats, overlaps, multi-byte text,
    /// tokens inside the replacement, and text that forms the token only
    /// together with the replacement.
    const CASES: &[(&str, &str)] = &[
        ("secret-1", "a secret-1 b secret-1 c"),
        ("secret-1", "secret-1"),
        ("secret-1", "secret-1secret-1"),
        ("secret-1", "no match secret- and secret-2 end sec"),
        ("aab", "aaab"),
        ("aab", "aaaab aab aa"),
        ("aba", "ababa"),
        ("aba", "aba"),
        ("abab", "abababab"),
        ("a", "banana"),
        (
            "t\u{f6}k\u{e9}n-\u{e9}",
            "a t\u{f6}k\u{e9}n-\u{e9} b t\u{f6}k t\u{f6}k\u{e9}n",
        ),
        (
            "\u{65e5}\u{672c}\u{8a9e}",
            "\u{65e5}\u{672c}\u{65e5}\u{672c}\u{8a9e}\u{65e5}\u{672c}\u{8a9e}",
        ),
        ("\u{1f511}k", "x\u{1f511}k\u{1f511} \u{1f511}k"),
        ("RED", "a RED b"),
        ("[RED", "x[RED"),
        ("ED]", "ED]"),
        ("x[RED", "xx[REDACTED]"),
        ("]a", "]aa"),
        ("]a", "x]a b"),
    ];

    /// All two-piece and three-piece splits of `text` at character boundaries.
    fn splits(text: &str) -> Vec<Vec<&str>> {
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(index, _)| index)
            .chain([text.len()])
            .collect();
        let mut all = vec![vec![text]];
        for &first in &boundaries {
            all.push(vec![&text[..first], &text[first..]]);
            for &second in boundaries.iter().filter(|cut| **cut >= first) {
                all.push(vec![&text[..first], &text[first..second], &text[second..]]);
            }
        }
        all
    }

    #[test]
    fn every_split_never_emits_the_token_and_matches_whole_text_redaction() {
        for (token, whole) in CASES {
            let expected = redact_known_secret(whole, Some(token));
            // The empty result is the discard case: the redacted text still
            // held the token, so already-emitted text cannot match it.
            let discards = whole.replace(token, REDACTION).contains(token);
            assert_eq!(expected.is_empty() && !whole.is_empty(), discards);
            for pieces in splits(whole) {
                let output = streamed(token, &pieces);
                assert!(
                    !output.contains(token),
                    "{token:?} leaked from {pieces:?}: {output:?}"
                );
                if !discards {
                    assert_eq!(output, expected, "{token:?} over {pieces:?}");
                }
            }
        }
    }

    #[test]
    fn a_split_token_is_withheld_until_it_is_complete() {
        let mut redactor = StreamRedactor::new(Some("secret-1"));
        assert_eq!(redactor.push("see sec"), "see ");
        assert_eq!(redactor.push("ret-"), "");
        assert_eq!(redactor.push("1 now"), "[REDACTED] now");
        assert_eq!(redactor.finish(), "");

        // A withheld tail that never completes is emitted at the end.
        let mut redactor = StreamRedactor::new(Some("secret-1"));
        assert_eq!(redactor.push("end sec"), "end ");
        assert_eq!(redactor.finish(), "sec");
    }

    #[test]
    fn no_token_or_an_empty_token_passes_text_through() {
        for token in [None, Some("")] {
            let mut redactor = StreamRedactor::new(token);
            assert_eq!(redactor.push("a sec"), "a sec");
            assert_eq!(redactor.push("ret"), "ret");
            assert_eq!(redactor.finish(), "");
        }
    }

    #[test]
    fn output_that_would_recreate_the_token_poisons_the_redactor() {
        // `[REDACTED]` contains the token, so no output is ever safe.
        let mut redactor = StreamRedactor::new(Some("RED"));
        assert_eq!(redactor.push("a RED b"), "");
        assert_eq!(redactor.push("more text"), "");
        assert_eq!(redactor.finish(), "");

        // Text emitted before the join stays emitted; nothing follows it.
        let mut redactor = StreamRedactor::new(Some("]a"));
        assert_eq!(redactor.push("hello "), "hello ");
        assert_eq!(redactor.push("]a"), "[REDACTED]");
        assert_eq!(redactor.push("a"), "");
        assert_eq!(redactor.finish(), "");
    }
}
