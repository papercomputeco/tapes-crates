//! Subject shapes: who a session's `auth_subject` is.
//!
//! A subject is the opaque identity ingest stamps on a session. The listing
//! joins whatever name and email the deployment's identity hook has recorded
//! for it — and still lists a subject nothing is known about, because an
//! unnamed subject is a fact about the data, not a gap in the response.

use serde::{Deserialize, Serialize};

use super::ContractModel;

/// One distinct auth subject and what is known about it.
///
/// Models the contract's `SubjectItem` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SubjectItem {
    /// The latest known email; empty when unknown.
    pub email: String,

    /// When a session for this subject was last seen, an RFC 3339 timestamp.
    pub last_seen_at: String,

    /// The latest known display name; empty when unknown.
    pub name: String,

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
