//! The result of one completed chat turn.

use omgcode_core::provider::ModelReply;
use serde::ser::Error as _;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value, to_value};

use crate::context::ContextMeter;

/// Assistant text from a tool-enabled turn. It remains untrusted model output.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolTurnReply {
    pub(crate) content: String,
    /// `stop`, or `length` when the token limit cut the reply short.
    pub(crate) finish_reason: String,
}

/// Bounded counters for a tool-enabled turn.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolTurnSummary {
    pub(crate) mode: &'static str,
    pub(crate) workspace_id: String,
    pub(crate) rounds: usize,
    pub(crate) tool_calls: usize,
    pub(crate) tool_bytes: u64,
    pub(crate) withheld_tool_results: usize,
}

/// Summary of one completed tool-enabled turn.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolTurnOutput {
    pub(crate) reply: ToolTurnReply,
    pub(crate) model_tools: ToolTurnSummary,
    pub(crate) context: ContextMeter,
}

/// Summary of one completed text-only turn.
#[derive(Clone, Debug)]
pub(crate) struct TextTurnOutput {
    pub(crate) reply: ModelReply,
    pub(crate) reason: &'static str,
    pub(crate) withheld_tool_results: usize,
    pub(crate) context: ContextMeter,
}

impl Serialize for TextTurnOutput {
    /// Build the same key-ordered object the CLI printed before the extraction.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut model_tools = Map::new();
        model_tools.insert("mode".to_owned(), Value::from("disabled"));
        model_tools.insert("reason".to_owned(), Value::from(self.reason));
        model_tools.insert(
            "withheld_tool_results".to_owned(),
            Value::from(self.withheld_tool_results),
        );
        let mut object = Map::new();
        object.insert(
            "reply".to_owned(),
            to_value(&self.reply).map_err(S::Error::custom)?,
        );
        object.insert("model_tools".to_owned(), Value::Object(model_tools));
        object.insert(
            "context".to_owned(),
            to_value(&self.context).map_err(S::Error::custom)?,
        );
        Value::Object(object).serialize(serializer)
    }
}

/// The two turn shapes behind [`ChatTurnOutput`].
#[derive(Clone, Debug)]
enum Shape {
    Tools(ToolTurnOutput),
    Text(TextTurnOutput),
}

/// One completed chat turn: the reply, the tool outcome, and the context meter.
///
/// It serializes to the object the trusted CLI prints after the session
/// locator. Typed accessors let another front end avoid parsing that JSON. The
/// reply is untrusted model output.
#[derive(Clone, Debug)]
pub struct ChatTurnOutput {
    shape: Shape,
}

impl ChatTurnOutput {
    pub(crate) fn tools(output: ToolTurnOutput) -> Self {
        Self {
            shape: Shape::Tools(output),
        }
    }

    pub(crate) fn text(output: TextTurnOutput) -> Self {
        Self {
            shape: Shape::Text(output),
        }
    }

    /// The assistant reply text. It is untrusted model output.
    pub fn reply(&self) -> &str {
        match &self.shape {
            Shape::Tools(output) => &output.reply.content,
            Shape::Text(output) => &output.reply.content,
        }
    }

    /// The provider finish reason: `stop`, or `length` when the limit cut the reply.
    pub fn finish_reason(&self) -> &str {
        match &self.shape {
            Shape::Tools(output) => &output.reply.finish_reason,
            Shape::Text(output) => &output.reply.finish_reason,
        }
    }

    /// Whether this turn offered the workspace tools.
    pub fn tools_enabled(&self) -> bool {
        matches!(self.shape, Shape::Tools(_))
    }

    /// Why a text-only turn did not offer tools; `None` when tools were offered.
    pub fn disabled_reason(&self) -> Option<&'static str> {
        match &self.shape {
            Shape::Tools(_) => None,
            Shape::Text(output) => Some(output.reason),
        }
    }

    /// The registered workspace whose tools were offered, as text.
    pub fn workspace_id(&self) -> Option<&str> {
        match &self.shape {
            Shape::Tools(output) => Some(&output.model_tools.workspace_id),
            Shape::Text(_) => None,
        }
    }

    /// Model requests made; `None` for a text-only turn, which does not count them.
    pub fn rounds(&self) -> Option<usize> {
        match &self.shape {
            Shape::Tools(output) => Some(output.model_tools.rounds),
            Shape::Text(_) => None,
        }
    }

    /// Tool calls dispatched; `None` for a text-only turn.
    pub fn tool_calls(&self) -> Option<usize> {
        match &self.shape {
            Shape::Tools(output) => Some(output.model_tools.tool_calls),
            Shape::Text(_) => None,
        }
    }

    /// Aggregate tool-message bytes; `None` for a text-only turn.
    pub fn tool_bytes(&self) -> Option<u64> {
        match &self.shape {
            Shape::Tools(output) => Some(output.model_tools.tool_bytes),
            Shape::Text(_) => None,
        }
    }

    /// Saved tool results replaced by a withheld notice when the turn started.
    pub fn withheld_tool_results(&self) -> usize {
        match &self.shape {
            Shape::Tools(output) => output.model_tools.withheld_tool_results,
            Shape::Text(output) => output.withheld_tool_results,
        }
    }

    /// The conservative pre-turn context accounting.
    pub fn context(&self) -> &ContextMeter {
        match &self.shape {
            Shape::Tools(output) => &output.context,
            Shape::Text(output) => &output.context,
        }
    }
}

impl Serialize for ChatTurnOutput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.shape {
            Shape::Tools(output) => output.serialize(serializer),
            Shape::Text(output) => output.serialize(serializer),
        }
    }
}
