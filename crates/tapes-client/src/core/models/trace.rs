//! Trace shapes: one user-visible turn, its header, and its spend.

use serde::{Deserialize, Serialize};

use super::ContractModel;
use super::span::{SpanItem, SpanLinkItem};

/// One user-visible turn's header. session_id / harness ids are not
/// duplicated here — they belong to the session.
///
/// Models the contract's `TraceItem` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TraceItem {
    /// The contract's `duration_ns`.
    pub duration_ns: i64,

    /// The contract's `ended_at`, an RFC 3339 timestamp.
    pub ended_at: String,

    /// The contract's `main_usage`.
    #[serde(deserialize_with = "super::null_default")]
    pub main_usage: MainUsage,

    /// The derive-time fold of the closing conversation- spine llm call's
    /// text output — the answer line for collapsed turn cards, so summary
    /// consumers never need spans.
    pub response_preview: String,

    /// The capture origin of the turn's rows ("wire" | "transcript"),
    /// promoted from raw_turns.source.
    pub source: String,

    /// The contract's `span_count`.
    pub span_count: i32,

    /// The contract's `started_at`, an RFC 3339 timestamp.
    pub started_at: String,

    /// The contract's `status`.
    pub status: String,

    /// A typed deriver signal ("post-compaction" for a compaction
    /// continuation, "shadow-opener" for a shadow-only opener), promoted out
    /// of the old metadata grab-bag.
    pub synthetic: String,

    /// The contract's `trace_id`.
    pub trace_id: String,

    /// The contract's `usage`.
    #[serde(deserialize_with = "super::null_default")]
    pub usage: TraceUsage,

    /// Served explicitly (not omitempty): a synthetic opener has an empty
    /// prompt, and dropping the key turns the empty string into `undefined`
    /// on the wire, which breaks consumers that expect a string (e.g.
    pub user_prompt: String,
}

impl ContractModel for TraceItem {
    const SCHEMA: &'static str = "TraceItem";
}

/// A trace's total token/cost rollup. Fields are pinned (no omitempty) so the
/// object shape is uniform across traces.
///
/// Models the contract's `TraceUsage` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TraceUsage {
    /// The contract's `cache_creation_tokens`.
    pub cache_creation_tokens: i64,

    /// The contract's `cache_read_tokens`.
    pub cache_read_tokens: i64,

    /// The contract's `cost_usd`.
    pub cost_usd: f64,

    /// The contract's `input_tokens`.
    pub input_tokens: i64,

    /// The contract's `output_tokens`.
    pub output_tokens: i64,
}

impl ContractModel for TraceUsage {
    const SCHEMA: &'static str = "TraceUsage";
}

/// The task token slice of a trace: the main agent and its subagents
/// (call_kind=main across every thread), no cache split or cost (those live
/// on the total Usage).
///
/// Models the contract's `MainUsage` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct MainUsage {
    /// The contract's `input_tokens`.
    pub input_tokens: i64,

    /// The contract's `output_tokens`.
    pub output_tokens: i64,
}

impl ContractModel for MainUsage {
    const SCHEMA: &'static str = "MainUsage";
}

/// One trace with its spans. In the composite session response links are
/// session-scoped (top level); the single-trace endpoint sets Links to the
/// edges touching that trace.
///
/// Models the contract's `TraceDetail` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TraceDetail {
    /// The contract's `links`.
    #[serde(deserialize_with = "super::null_default")]
    pub links: Vec<SpanLinkItem>,

    /// The contract's `schema`.
    pub schema: String,

    /// The contract's `spans`.
    #[serde(deserialize_with = "super::null_default")]
    pub spans: Vec<SpanItem>,

    /// The contract's `trace`.
    #[serde(deserialize_with = "super::null_default")]
    pub trace: TraceItem,
}

impl ContractModel for TraceDetail {
    const SCHEMA: &'static str = "TraceDetail";
}

/// The summaries list for one session. `schema` stamps the projection
/// generation the rows were derived against — the same stamp the composite
/// carries — so every trace-grain response is self-describing, not just the
/// composite.
///
/// Models the contract's `TraceListResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TraceListResponse {
    /// The contract's `items`.
    #[serde(deserialize_with = "super::null_default")]
    pub items: Vec<TraceItem>,

    /// The contract's `schema`.
    pub schema: String,
}

impl ContractModel for TraceListResponse {
    const SCHEMA: &'static str = "TraceListResponse";
}
