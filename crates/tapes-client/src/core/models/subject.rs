//! Subject shapes: who a session's `auth_subject` is.
//!
//! A subject is the opaque identity ingest stamps on a session. tapes knows
//! only the id: turning one into a person is the deployment's concern, joined
//! at read time by whatever client knows its own directory.

use serde::{Deserialize, Serialize};

use super::ContractModel;

/// One distinct auth subject.
///
/// Models the contract's `SubjectItem` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SubjectItem {
    /// When a session for this subject was last seen, an RFC 3339 timestamp.
    pub last_seen_at: String,

    /// How many sessions carry this subject.
    pub session_count: i64,

    /// The opaque subject itself, as stamped on sessions.
    pub subject: String,
}

impl ContractModel for SubjectItem {
    const SCHEMA: &'static str = "SubjectItem";
}

/// The response for `GET /v1/subjects`.
///
/// Models the contract's `SubjectListResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SubjectListResponse {
    /// The matching subjects, most recently seen first.
    #[serde(deserialize_with = "super::null_default")]
    pub items: Vec<SubjectItem>,
}

impl ContractModel for SubjectListResponse {
    const SCHEMA: &'static str = "SubjectListResponse";
}
