//! Raw-turn shapes: the wire log behind a derivation, and its repairs.
//!
//! The raw layer is immutable. What can be corrected is the *attribution*
//! projected over it, which is why the repair shapes describe a replacement
//! projection rather than an edit.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContractModel;

/// One wire-log row: what crossed the wire (or arrived as a transcript push),
/// without the payload blobs.
///
/// Models the contract's `RawTurnHeaderItem` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RawTurnHeaderItem {
    /// The contract's `agent_name`.
    pub agent_name: String,

    /// The contract's `id`.
    pub id: i64,

    /// The contract's `meta`.
    pub meta: Value,

    /// The contract's `provider`.
    pub provider: String,

    /// How many verbatim upstream response bytes the raw layer retained for
    /// this turn, as they arrived on the wire and before any reduction; 0
    /// when none were kept.
    pub raw_response_bytes: i64,

    /// True when the turn's verbatim response existed but was not retained —
    /// it exceeded the 8 MiB ingest cap or the producer withheld it — so
    /// `raw_response_bytes == 0` with this set is a fidelity gap, not a turn
    /// that never had verbatim bytes.
    pub raw_response_dropped: bool,

    /// The contract's `received_at`, an RFC 3339 timestamp.
    pub received_at: String,

    /// The request size the capture adapter recorded at capture time
    /// (`meta.request_bytes`), not a measurement of the stored payload; 0
    /// when the producer did not report one.
    pub request_bytes: i64,

    /// The contract's `request_id`.
    pub request_id: String,

    /// The response size the capture adapter recorded at capture time
    /// (`meta.response_bytes`), not a measurement of the stored payload; 0
    /// when the producer did not report one.
    pub response_bytes: i64,

    /// The contract's `source`.
    pub source: String,
}

impl ContractModel for RawTurnHeaderItem {
    const SCHEMA: &'static str = "RawTurnHeaderItem";
}

/// One page of a session's wire log, in raw turn id order.
///
/// Models the contract's `RawTurnListResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RawTurnListResponse {
    /// The contract's `items`.
    #[serde(deserialize_with = "super::null_default")]
    pub items: Vec<RawTurnHeaderItem>,

    /// Continues the walk from the last raw turn of this page (pass it as
    /// `cursor`). Empty once the page reached the session's last raw turn.
    #[serde(deserialize_with = "super::null_default")]
    pub next_cursor: String,
}

impl ContractModel for RawTurnListResponse {
    const SCHEMA: &'static str = "RawTurnListResponse";
}

impl RawTurnListResponse {
    /// This listing as one page of the crate's pagination convention.
    ///
    /// The envelope is `items` plus `next_cursor`, which is exactly
    /// [`crate::page::Page`] — so a caller walking a wire log reaches the
    /// same loop, the same three spellings of "no more pages", and the same
    /// guard against a server that repeats a cursor as every other listing.
    #[must_use]
    pub fn into_page(self) -> crate::page::Page<RawTurnHeaderItem> {
        crate::page::Page {
            items: self.items,
            next_cursor: Some(self.next_cursor),
        }
    }
}

/// The effective, repairable attribution projected over an immutable raw
/// turn.
///
/// Models the contract's `RawTurnAttribution` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RawTurnAttribution {
    /// The contract's `harness_id`.
    pub harness_id: String,

    /// The contract's `harness_session_id`.
    pub harness_session_id: String,

    /// The contract's `parent_harness_session_id`.
    pub parent_harness_session_id: String,

    /// The contract's `raw_turn_id`.
    pub raw_turn_id: i64,

    /// The contract's `thread_id`.
    pub thread_id: String,
}

impl ContractModel for RawTurnAttribution {
    const SCHEMA: &'static str = "RawTurnAttribution";
}

/// Selects exactly one raw row and supplies a complete replacement
/// attribution.
///
/// Models the contract's `RawTurnAttributionRepairRequest` schema.
/// default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RawTurnAttributionRepairRequest {
    /// The contract's `harness_id`.
    pub harness_id: String,

    /// The contract's `harness_session_id`.
    pub harness_session_id: String,

    /// The contract's `paper_proxy_request_id`.
    pub paper_proxy_request_id: String,

    /// The contract's `parent_harness_session_id`.
    pub parent_harness_session_id: String,

    /// The contract's `raw_turn_id`.
    pub raw_turn_id: i64,

    /// The contract's `reason`.
    pub reason: String,

    /// The contract's `thread_id`.
    pub thread_id: String,
}

impl ContractModel for RawTurnAttributionRepairRequest {
    const SCHEMA: &'static str = "RawTurnAttributionRepairRequest";
}

/// What an attribution repair changed, and what it left to converge.
///
/// Models the contract's `RawTurnAttributionRepairResult` schema.
/// default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RawTurnAttributionRepairResult {
    /// The contract's `effective`.
    #[serde(deserialize_with = "super::null_default")]
    pub effective: RawTurnAttribution,

    /// The contract's `previous`.
    #[serde(deserialize_with = "super::null_default")]
    pub previous: RawTurnAttribution,

    /// ProjectionsPending lists the sessions whose synchronous rebuild failed
    /// after the correction committed.
    #[serde(deserialize_with = "super::null_default")]
    pub projections_pending: Vec<RepairPendingSession>,

    /// The contract's `recorded`.
    pub recorded: bool,

    /// SourceCleanupPending reports that the best-effort removal of the
    /// emptied previous-session row failed after the correction and both
    /// projection rebuilds applied.
    pub source_cleanup_pending: bool,
}

impl ContractModel for RawTurnAttributionRepairResult {
    const SCHEMA: &'static str = "RawTurnAttributionRepairResult";
}

/// One harness session whose projection rebuild did not complete
/// synchronously during a repair.
///
/// Models the contract's `RepairPendingSession` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RepairPendingSession {
    /// The contract's `harness_id`.
    pub harness_id: String,

    /// The contract's `harness_session_id`.
    pub harness_session_id: String,
}

impl ContractModel for RepairPendingSession {
    const SCHEMA: &'static str = "RepairPendingSession";
}
