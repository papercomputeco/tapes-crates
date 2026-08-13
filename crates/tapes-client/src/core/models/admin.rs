//! Administrative and aggregate shapes: seeding, derive runs, and stats.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ContractModel;

/// A summary of one seeding run.
///
/// Models the contract's `SeedResult` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SeedResult {
    /// The total number of corpus rows replayed.
    pub raw_turns: i32,

    /// The contract's `raw_turns_deduped`.
    pub raw_turns_deduped: i64,

    /// RawTurnsInserted counts rows that landed as new raw turns;
    /// RawTurnsDeduped counts replays the raw layer's dedup absorbed (a re-
    /// seed reports everything deduped).
    pub raw_turns_inserted: i64,

    /// The number of demo sessions the corpora replay into.
    pub sessions: i32,
}

impl ContractModel for SeedResult {
    const SCHEMA: &'static str = "SeedResult";
}

/// The `POST /v1/admin/seed/demo` body.
///
/// Models the contract's `seedDemoRequest` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeedDemoRequest {
    /// The contract's `overwrite`.
    pub overwrite: bool,
}

impl ContractModel for SeedDemoRequest {
    const SCHEMA: &'static str = "seedDemoRequest";
}

/// The derive-run result, keyed by org.
///
/// Models the contract's `deriveRunResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct DeriveRunResponse {
    /// The contract's `orgs`.
    #[serde(deserialize_with = "super::null_default")]
    pub orgs: BTreeMap<String, RederiveReport>,
}

impl ContractModel for DeriveRunResponse {
    const SCHEMA: &'static str = "deriveRunResponse";
}

/// A summary of one derive pass.
///
/// Models the contract's `RederiveReport` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RederiveReport {
    /// The contract's `attached_verdicts`.
    pub attached_verdicts: i32,

    /// The contract's `call_kinds`.
    #[serde(deserialize_with = "super::null_default")]
    pub call_kinds: BTreeMap<String, i32>,

    /// Verdict attach: judged actions grouped across stages, and how many
    /// attached one-to-one to a captured tool_use.
    pub judged_actions: i32,

    /// The contract's `node_kinds`.
    #[serde(deserialize_with = "super::null_default")]
    pub node_kinds: BTreeMap<String, i32>,

    /// The contract's `nodes`.
    pub nodes: i32,

    /// The contract's `parse_failures`.
    #[serde(deserialize_with = "super::null_default")]
    pub parse_failures: Vec<String>,

    /// The contract's `parsed_turns`.
    pub parsed_turns: i32,

    /// PlansAttached counts plan-name-gen calls linked to the ExitPlanMode
    /// tool_use that accepted the plan.
    pub plans_attached: i32,

    /// The contract's `raw_only_turns`.
    pub raw_only_turns: i32,

    /// The contract's `raw_turns`.
    pub raw_turns: i32,

    /// The contract's `reconcile`.
    #[serde(deserialize_with = "super::null_default")]
    pub reconcile: ReconcileStats,

    /// UnattachedActions samples judged actions that found no matching
    /// tool_use (capped) — expected for non-tool events like subagent
    /// handbacks; anything else is matcher signal worth reading.
    #[serde(deserialize_with = "super::null_default")]
    pub unattached_actions: Vec<String>,

    /// WebSummaryAttached counts web-summary calls linked back to their
    /// WebFetch/WebSearch tool_use.
    pub web_summary_attached: i32,
}

impl ContractModel for RederiveReport {
    const SCHEMA: &'static str = "RederiveReport";
}

/// The transcript-to-wire fusion for one org.
///
/// Models the contract's `ReconcileStats` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ReconcileStats {
    /// Counts anchor rows carrying a non-started kind — interacted re-entries
    /// a capture client banked for future rendering.
    pub codex_interacted_rows: i32,

    /// Codex thread-spawn anchoring (see codex.go). Unanchored threads
    /// degrade to trace-root placement — a non-zero count is the visible
    /// signal that spawn-anchor rows are missing or ambiguous.
    pub codex_threads_anchored: i32,

    /// The contract's `codex_threads_unanchored`.
    pub codex_threads_unanchored: i32,

    /// `ConversationJoined` and `ConversationTotal` measure how many
    /// conversation- spine nodes' content appears in a transcript — the Go-
    /// native version of the prototype's join-rate oracle.
    pub conversation_joined: i32,

    /// The contract's `conversation_total`.
    pub conversation_total: i32,

    /// The contract's `forked_chains`.
    pub forked_chains: i32,

    /// The contract's `main_chains_joined`.
    pub main_chains_joined: i32,

    /// The contract's `subagent_forks`.
    pub subagent_forks: i32,

    /// The contract's `transcript_files`.
    pub transcript_files: i32,
}

impl ContractModel for ReconcileStats {
    const SCHEMA: &'static str = "ReconcileStats";
}

/// The response for `GET /v1/stats`.
///
/// The figures are sums over the trace-grain rollups, so they agree with the
/// session detail and trace views rather than being a second count. Duration
/// is served in milliseconds, not the nanoseconds the spans carry: a summed
/// nanosecond figure over a wide window leaves a JSON consumer's safe-integer
/// range.
///
/// Models the contract's `StatsResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct StatsResponse {
    /// The contract's `completed_count`.
    pub completed_count: i32,

    /// The contract's `input_tokens`.
    pub input_tokens: i64,

    /// The contract's `output_tokens`.
    pub output_tokens: i64,

    /// The contract's `session_count`.
    pub session_count: i32,

    /// The contract's `tool_calls`.
    pub tool_calls: i32,

    /// The contract's `total_cost`.
    pub total_cost: f64,

    /// The contract's `total_duration_ms`.
    pub total_duration_ms: i64,

    /// The contract's `turn_count`.
    pub turn_count: i32,
}

impl ContractModel for StatsResponse {
    const SCHEMA: &'static str = "StatsResponse";
}
