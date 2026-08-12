//! Session shapes: the capture identity and the deriver's projection.
//!
//! The split the wire draws is kept here rather than flattened: identity is
//! ingest-written and lives at the top of [`SessionItem`], while everything
//! folded from the span layer at derive time lives under `rollup`. Flattening
//! them would blur which layer owns a field, and "why is this empty?" has two
//! very different answers depending on the side it fell on.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContractModel;
use super::span::SpanLinkItem;
use super::trace::TraceDetail;

/// The per-session shape: capture identity at the top level, the deriver-
/// owned projection nested under `rollup`.
///
/// Models the contract's `SessionItem` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionItem {
    /// The gateway-stamped JWT subject (WorkOS user id) captured at ingest;
    /// empty for rows captured before the edge began stamping it.
    pub auth_subject: String,

    /// The contract's `cwd`.
    pub cwd: String,

    /// The user's Console rename (sessions.display_name), empty unless a user
    /// set one.
    pub display_name: String,

    /// The server-resolved label clients should render: DisplayName ->
    /// rollup.title (generated) -> preview -> Name -> id slice.
    pub display_title: String,

    /// The contract's `ended_at`, an RFC 3339 timestamp.
    pub ended_at: String,

    /// The contract's `harness_id`.
    pub harness_id: String,

    /// The contract's `harness_metadata`.
    #[serde(deserialize_with = "super::null_default")]
    pub harness_metadata: BTreeMap<String, Value>,

    /// The contract's `harness_session_id`.
    pub harness_session_id: String,

    /// The contract's `harness_version`.
    pub harness_version: String,

    /// Identity — capture-side facts, ingest-written.
    pub id: String,

    /// The contract's `last_seen_at`, an RFC 3339 timestamp.
    pub last_seen_at: String,

    /// A runtime presence signal, not a projection fact: true when the
    /// session has no recorded end and was seen within the liveness window.
    pub live: bool,

    /// The harness identity-row label — the harness-supplied session name (a
    /// plan slug), or the folded title (rollup.title) as a fallback when no
    /// name was captured.
    pub name: String,

    /// The contract's `parent_session_id`.
    pub parent_session_id: String,

    /// The contract's `rollup`.
    #[serde(deserialize_with = "super::null_default")]
    pub rollup: SessionRollup,

    /// The contract's `started_at`, an RFC 3339 timestamp.
    pub started_at: String,
}

impl ContractModel for SessionItem {
    const SCHEMA: &'static str = "SessionItem";
}

/// The deriver-owned session projection — status, title, counts, and spend,
/// all folded from the span layer at derive time.
///
/// Models the contract's `SessionRollup` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionRollup {
    /// KindCounts (spans per call_kind) and Tasks (TaskCreate/TaskUpdate
    /// folds) are pinned so the rollup shape is uniform across sessions.
    #[serde(deserialize_with = "super::null_default")]
    pub kind_counts: BTreeMap<String, i32>,

    /// The dominant conversation-spine model; ModelUsage is the per- model
    /// spend breakdown across every thread (subagent models included), cost-
    /// ordered so the UI can show "dominant model + share" without a cheap-
    /// subagent fan-out skewing it.
    pub model: String,

    /// The contract's `model_usage`.
    #[serde(deserialize_with = "super::null_default")]
    pub model_usage: Vec<ModelUsage>,

    /// The contract's `preview`.
    pub preview: String,

    /// The contract's `status`.
    pub status: String,

    /// The contract's `tasks`.
    #[serde(deserialize_with = "super::null_default")]
    pub tasks: Vec<TreeTask>,

    /// The deriver's folded session title (derived_title), generated from the
    /// conversation.
    pub title: String,

    /// The contract's `turn_count`.
    pub turn_count: i32,

    /// The contract's `usage`.
    #[serde(deserialize_with = "super::null_default")]
    pub usage: SessionUsage,
}

impl ContractModel for SessionRollup {
    const SCHEMA: &'static str = "SessionRollup";
}

/// The session's total token/cost spend, folded from the span layer.
///
/// Models the contract's `SessionUsage` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionUsage {
    /// The contract's `cost_usd`.
    pub cost_usd: f64,

    /// The contract's `input_tokens`.
    pub input_tokens: i64,

    /// The contract's `output_tokens`.
    pub output_tokens: i64,
}

impl ContractModel for SessionUsage {
    const SCHEMA: &'static str = "SessionUsage";
}

/// One model's contribution to a session in the API: how many llm calls ran
/// on it and what they spent.
///
/// Models the contract's `ModelUsage` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ModelUsage {
    /// The contract's `calls`.
    pub calls: i64,

    /// The contract's `cost_usd`.
    pub cost_usd: f64,

    /// The contract's `input_tokens`.
    pub input_tokens: i64,

    /// The contract's `model`.
    pub model: String,

    /// The contract's `output_tokens`.
    pub output_tokens: i64,
}

impl ContractModel for ModelUsage {
    const SCHEMA: &'static str = "ModelUsage";
}

/// One task folded from the session's TaskCreate/TaskUpdate calls.
///
/// Models the contract's `TreeTask` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct TreeTask {
    /// The contract's `description`.
    pub description: String,

    /// The contract's `id`.
    pub id: String,

    /// The contract's `status`.
    pub status: String,

    /// The contract's `subject`.
    pub subject: String,

    /// The contract's `updates`.
    pub updates: i32,
}

impl ContractModel for TreeTask {
    const SCHEMA: &'static str = "TreeTask";
}

/// The response envelope for GET /v1/sessions.
///
/// Models the contract's `SessionListResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionListResponse {
    /// The contract's `items`.
    #[serde(deserialize_with = "super::null_default")]
    pub items: Vec<SessionItem>,

    /// The contract's `next_cursor`.
    pub next_cursor: String,
}

impl ContractModel for SessionListResponse {
    const SCHEMA: &'static str = "SessionListResponse";
}

/// The response for GET /v1/sessions/:id: the session record alone.
///
/// Models the contract's `SessionDetailResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionDetailResponse {
    /// The contract's `session`.
    #[serde(deserialize_with = "super::null_default")]
    pub session: SessionItem,
}

impl ContractModel for SessionDetailResponse {
    const SCHEMA: &'static str = "SessionDetailResponse";
}

/// The composite session view on the span model.
///
/// Models the contract's `SessionTracesResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionTracesResponse {
    /// The contract's `links`.
    #[serde(deserialize_with = "super::null_default")]
    pub links: Vec<SpanLinkItem>,

    /// The contract's `schema`.
    pub schema: String,

    /// The contract's `session`.
    #[serde(deserialize_with = "super::null_default")]
    pub session: SessionItem,

    /// The contract's `traces`.
    #[serde(deserialize_with = "super::null_default")]
    pub traces: Vec<TraceDetail>,
}

impl ContractModel for SessionTracesResponse {
    const SCHEMA: &'static str = "SessionTracesResponse";
}

/// The `PATCH /v1/sessions/{id}` body.
///
/// `display_name` distinguishes three states the server acts on differently:
/// absent is nothing to update (a 400), while an explicit null or an empty
/// string clears the rename back to the auto-derived title.
///
/// Models the contract's `sessionUpdateRequest` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionUpdateRequest {
    /// The contract's `display_name`.
    pub display_name: String,
}

impl ContractModel for SessionUpdateRequest {
    const SCHEMA: &'static str = "sessionUpdateRequest";
}

impl SessionListResponse {
    /// This listing as one page of the crate's pagination convention.
    ///
    /// The envelope is `items` plus `next_cursor`, which is exactly
    /// [`crate::page::Page`] — so a caller walking sessions reaches the same
    /// loop, the same three spellings of "no more pages", and the same guard
    /// against a server that repeats a cursor as every other listing.
    #[must_use]
    pub fn into_page(self) -> crate::page::Page<SessionItem> {
        crate::page::Page {
            items: self.items,
            next_cursor: Some(self.next_cursor),
        }
    }
}
