//! Span shapes: the observed units of work, their edges, and search hits.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContractModel;

/// One observed unit of work. Every field is a deriver output, formatting-
/// only: the harness-taxonomy fields (call_kind, model, stop_reason,
/// thread_id, verdict) are typed rather than bagged in a metadata map, and
/// input/output are uniform content-block arrays for ALL kinds — the console
/// owns per-kind rendering.
///
/// Models the contract's `SpanItem` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SpanItem {
    /// Deriver-written taxonomy, promoted from the old metadata grab-bag.
    pub call_kind: String,

    /// The contract's `duration_ns`.
    pub duration_ns: i64,

    /// Input/Output are content-block arrays (llm.ContentBlock), uniform for
    /// every kind (tool spans included — no unwrapping).
    #[serde(deserialize_with = "super::null_default")]
    pub input: Vec<Value>,

    /// The contract's `kind`.
    pub kind: String,

    /// The contract's `model`.
    pub model: String,

    /// The contract's `name`.
    pub name: String,

    /// The contract's `output`.
    #[serde(deserialize_with = "super::null_default")]
    pub output: Vec<Value>,

    /// The contract's `parent_span_id`.
    pub parent_span_id: String,

    /// Payload marks a preview-truncated span so the console drills in for
    /// the full payload; absent in full mode.
    pub payload: String,

    /// The contract's `raw_turn_id`.
    pub raw_turn_id: i64,

    /// The span's presentation ordinal within its trace; spans arrive sorted
    /// by it (started_at ties inside one llm call — parallel tool batches
    /// share an instant).
    pub seq: i64,

    /// The contract's `span_id`.
    pub span_id: String,

    /// The contract's `started_at`, an RFC 3339 timestamp.
    pub started_at: String,

    /// The contract's `status`.
    pub status: String,

    /// The contract's `stop_reason`.
    pub stop_reason: String,

    /// The contract's `thread_id`.
    pub thread_id: String,

    /// The contract's `trace_id`.
    pub trace_id: String,

    /// Usage (was `metrics`) is an llm.Usage object on the wire — {}-pinned
    /// for usage-less spans.
    pub usage: Value,

    /// The typed security-monitor disposition (null off permission-check
    /// spans), deriver-written.
    pub verdict: Option<Value>,
}

impl ContractModel for SpanItem {
    const SCHEMA: &'static str = "SpanItem";
}

/// A dataflow edge. kind is a typed top-level field (rejoin / verdict /
/// compaction-seam / emits / feeds); from/to trace ids differ on cross-trace
/// causality.
///
/// Models the contract's `SpanLinkItem` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SpanLinkItem {
    /// The contract's `from_io`.
    pub from_io: String,

    /// The contract's `from_span_id`.
    pub from_span_id: String,

    /// The contract's `from_trace_id`.
    pub from_trace_id: String,

    /// The contract's `kind`.
    pub kind: String,

    /// The contract's `to_io`.
    pub to_io: String,

    /// The contract's `to_span_id`.
    pub to_span_id: String,

    /// The contract's `to_trace_id`.
    pub to_trace_id: String,
}

impl ContractModel for SpanLinkItem {
    const SCHEMA: &'static str = "SpanLinkItem";
}

/// The span search response.
///
/// Models the contract's `SpanSearchOutput` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SpanSearchOutput {
    /// The contract's `count`.
    pub count: i32,

    /// The contract's `query`.
    pub query: String,

    /// The contract's `results`.
    #[serde(deserialize_with = "super::null_default")]
    pub results: Vec<SpanSearchResult>,
}

impl ContractModel for SpanSearchOutput {
    const SCHEMA: &'static str = "SpanSearchOutput";
}

/// One span hit with its trace/turn context.
///
/// Models the contract's `SpanSearchResult` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SpanSearchResult {
    /// The contract's `model`.
    pub model: String,

    /// The contract's `score`.
    pub score: f32,

    /// The contract's `session_id`.
    pub session_id: String,

    /// Snippet previews the matched span's delta-only text.
    pub snippet: String,

    /// The contract's `span_id`.
    pub span_id: String,

    /// The contract's `started_at`, an RFC 3339 timestamp.
    pub started_at: String,

    /// The contract's `trace_id`.
    pub trace_id: String,

    /// The prompt of the turn (trace) the span belongs to. Served explicitly
    /// (not omitempty) so a synthetic turn's empty prompt reaches consumers
    /// as "" rather than a dropped key — see TraceItem.
    pub user_prompt: String,
}

impl ContractModel for SpanSearchResult {
    const SCHEMA: &'static str = "SpanSearchResult";
}
