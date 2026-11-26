//! The reasoning effort a request may ask for (ADR-0102).
//!
//! The names are the ones OpenAI documents for Chat Completions
//! `reasoning_effort` and Responses `reasoning.effort`; the other providers
//! that take an effort document subsets of the same names. Which names a model
//! accepts is the server's decision, so no value is adjusted here.

use serde::{Deserialize, Serialize};

/// One documented reasoning effort. "No effort" is `Option::None`, which
/// sends no field at all; [`ReasoningEffort::None`] is the provider's own
/// `none` value, which asks a model to skip reasoning where it can.
///
/// The enum is closed: serde accepts exactly the seven wire names and refuses
/// any other text, so no other value reaches a request or a record.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// `none`: skip reasoning where the model allows it.
    None,
    /// `minimal`.
    Minimal,
    /// `low`.
    Low,
    /// `medium`.
    Medium,
    /// `high`.
    High,
    /// `xhigh`.
    #[serde(rename = "xhigh")]
    XHigh,
    /// `max`.
    Max,
}

impl ReasoningEffort {
    /// Every value, from the least reasoning to the most.
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    /// The wire name, as sent and recorded.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// The value with this wire name, if any.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|effort| effort.as_str() == text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_value_round_trips_through_its_wire_name() {
        for effort in ReasoningEffort::ALL {
            let json = serde_json::to_string(&effort).expect("encode");
            assert_eq!(json, format!("\"{}\"", effort.as_str()));
            assert_eq!(
                serde_json::from_str::<ReasoningEffort>(&json).expect("decode"),
                effort
            );
            assert_eq!(ReasoningEffort::parse(effort.as_str()), Some(effort));
        }
        assert_eq!(ReasoningEffort::XHigh.as_str(), "xhigh");
    }

    #[test]
    fn other_text_is_refused() {
        for text in ["", "x_high", "XHIGH", "High", "ultra", "medium ", "default"] {
            assert_eq!(ReasoningEffort::parse(text), None, "{text:?}");
            assert!(
                serde_json::from_str::<ReasoningEffort>(&format!("\"{text}\"")).is_err(),
                "{text:?}"
            );
        }
    }
}
