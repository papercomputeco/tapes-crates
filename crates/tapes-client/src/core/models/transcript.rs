//! Transcript shapes: a session read as the conversation it was.
//!
//! The span read model answers "what did the harness do?"; a transcript
//! answers "what was said?". Each row is one side of one main-thread llm
//! turn — the user's genuine text with harness-injected tags already
//! stripped server-side, or the assistant's reply with the tool calls it made.
//! Tool results ride in their own field rather than in `text`, so a search
//! over what a person typed is not a search over tool output.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContractModel;

/// One page of a session's transcript.
///
/// Models the contract's `SessionTranscriptResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionTranscriptResponse {
    /// This page's rows, in conversation order.
    #[serde(deserialize_with = "super::null_default")]
    pub items: Vec<TranscriptRow>,

    /// Continues the walk (pass it as `cursor`); empty on the last page.
    pub next_cursor: String,

    /// The session the rows belong to.
    pub session_id: String,
}

impl ContractModel for SessionTranscriptResponse {
    const SCHEMA: &'static str = "SessionTranscriptResponse";
}

impl SessionTranscriptResponse {
    /// This page as one page of the crate's pagination convention.
    #[must_use]
    pub fn into_page(self) -> crate::page::Page<TranscriptRow> {
        crate::page::Page {
            items: self.items,
            next_cursor: Some(self.next_cursor),
        }
    }
}

/// One side of one main-thread llm turn.
///
/// Models the contract's `TranscriptRow` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TranscriptRow {
    /// `user` or `assistant`. A `String` rather than an enum so a role a
    /// newer server adds decodes instead of failing the page.
    pub role: String,

    /// The row's position in the session.
    pub seq: i64,

    /// The llm span the row was read from.
    pub span_id: String,

    /// When the turn started, an RFC 3339 timestamp.
    pub started_at: String,

    /// The row's text: genuine user text, or the assistant's reply.
    pub text: String,

    /// Empty for the root thread; the subagent thread otherwise.
    pub thread_id: String,

    /// The tool calls an assistant row made.
    #[serde(
        deserialize_with = "super::null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tool_calls: Vec<TranscriptToolCall>,

    /// The tool results a user row carried back.
    #[serde(
        deserialize_with = "super::null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tool_results: Vec<TranscriptToolResult>,

    /// The trace the span belongs to.
    pub trace_id: String,
}

impl ContractModel for TranscriptRow {
    const SCHEMA: &'static str = "TranscriptRow";
}

/// One tool call an assistant row made.
///
/// Models the contract's `TranscriptToolCall` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TranscriptToolCall {
    /// The call's id, which a later result's `tool_use_id` names.
    pub id: String,

    /// The tool's input, verbatim — its shape is the tool's, not tapes'.
    pub input: Value,

    /// The tool's name.
    pub name: String,
}

impl ContractModel for TranscriptToolCall {
    const SCHEMA: &'static str = "TranscriptToolCall";
}

/// One tool result a user row carried back.
///
/// Models the contract's `TranscriptToolResult` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TranscriptToolResult {
    /// Whether the tool reported failure.
    pub is_error: bool,

    /// The result's text.
    pub text: String,

    /// The id of the call this answers.
    pub tool_use_id: String,
}

impl ContractModel for TranscriptToolResult {
    const SCHEMA: &'static str = "TranscriptToolResult";
}
