//! Safe construction of FTS5 queries and parsing of marked snippets.
//!
//! Caller text is never passed to FTS5 as query syntax. This module quotes each
//! term itself, and it reserves two private-use code points as snippet markers
//! so transcript text cannot forge a match highlight.

use super::IndexError;
use super::query::SnippetPart;

/// Marks the start of a matched run in an FTS5 snippet.
pub(super) const MARK_START: char = '\u{e000}';
/// Marks the end of a matched run in an FTS5 snippet.
pub(super) const MARK_END: char = '\u{e001}';

/// The most characters a search text may have.
pub(super) const MAX_QUERY_CHARS: usize = 256;
/// The most whitespace-separated terms a search text may have.
pub(super) const MAX_QUERY_TERMS: usize = 16;

/// Replace the marker code points so indexed text can never contain them.
pub(super) fn sanitize_text(text: &str) -> String {
    text.replace([MARK_START, MARK_END], "\u{fffd}")
}

/// Build the FTS5 `MATCH` expression for the caller's text.
///
/// Each whitespace-separated term becomes a double-quoted FTS5 string with
/// embedded quotes doubled, so operators, column filters, and `NEAR` are plain
/// text. Terms join with spaces (implicit AND). The last term gets a trailing
/// `*` for prefix matching while typing.
///
/// # Errors
///
/// `InvalidQuery` for text over 256 characters, with no terms, with more than
/// 16 terms, or containing NUL.
pub(super) fn match_expression(text: &str) -> Result<String, IndexError> {
    if text.chars().count() > MAX_QUERY_CHARS {
        return Err(IndexError::InvalidQuery("search text is too long"));
    }
    if text.contains('\0') {
        return Err(IndexError::InvalidQuery("search text contains NUL"));
    }
    let terms: Vec<&str> = text.split_whitespace().collect();
    if terms.is_empty() {
        return Err(IndexError::InvalidQuery("search text has no terms"));
    }
    if terms.len() > MAX_QUERY_TERMS {
        return Err(IndexError::InvalidQuery("search text has too many terms"));
    }
    let mut expression = String::new();
    for (index, term) in terms.iter().enumerate() {
        if index > 0 {
            expression.push(' ');
        }
        expression.push('"');
        expression.push_str(&term.replace('"', "\"\""));
        expression.push('"');
        if index + 1 == terms.len() {
            expression.push('*');
        }
    }
    Ok(expression)
}

/// Split an FTS5 snippet into plain and matched parts. Marker code points never
/// appear in the output, and empty parts are omitted.
pub(super) fn parse_snippet(marked: &str) -> Vec<SnippetPart> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut matched = false;
    for character in marked.chars() {
        let next = match character {
            MARK_START => Some(true),
            MARK_END => Some(false),
            _ => None,
        };
        match next {
            Some(state) => {
                flush(&mut parts, &mut current, matched);
                matched = state;
            }
            None => current.push(character),
        }
    }
    flush(&mut parts, &mut current, matched);
    parts
}

fn flush(parts: &mut Vec<SnippetPart>, current: &mut String, matched: bool) {
    if !current.is_empty() {
        parts.push(SnippetPart {
            text: std::mem::take(current),
            matched,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{match_expression, parse_snippet, sanitize_text};
    use crate::session_index::IndexError;

    #[test]
    fn terms_are_quoted_and_only_the_last_gets_a_prefix_star() {
        assert_eq!(
            match_expression("  alpha   be\"ta gam  ").expect("expression"),
            "\"alpha\" \"be\"\"ta\" \"gam\"*"
        );
        assert_eq!(
            match_expression("a OR b").expect("expression"),
            "\"a\" \"OR\" \"b\"*"
        );
        assert_eq!(match_expression("\"").expect("expression"), "\"\"\"\"*");
    }

    #[test]
    fn unusable_text_is_rejected_with_a_typed_error() {
        for text in ["", "   \t\n", &"x".repeat(257), "a\0b", &"a ".repeat(17)] {
            assert!(
                matches!(match_expression(text), Err(IndexError::InvalidQuery(_))),
                "{text:?}"
            );
        }
        assert!(match_expression(&"x".repeat(256)).is_ok());
        assert!(match_expression(&"a ".repeat(16)).is_ok());
    }

    #[test]
    fn snippet_parts_alternate_and_never_contain_markers() {
        let parts = parse_snippet("one \u{e000}two\u{e001} three \u{e000}four\u{e001}");
        let shown: Vec<(&str, bool)> = parts
            .iter()
            .map(|part| (part.text.as_str(), part.matched))
            .collect();
        assert_eq!(
            shown,
            [
                ("one ", false),
                ("two", true),
                (" three ", false),
                ("four", true)
            ]
        );
        assert!(parse_snippet("").is_empty());
        assert!(parse_snippet("\u{e000}\u{e001}").is_empty());
    }

    #[test]
    fn sanitizing_removes_both_markers() {
        assert_eq!(sanitize_text("a\u{e000}b\u{e001}c"), "a\u{fffd}b\u{fffd}c");
    }
}
