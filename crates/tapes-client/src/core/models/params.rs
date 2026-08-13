//! Typed parameters for the sealed operations that take them.
//!
//! # Why these are types and not `Vec<(&str, String)>`
//!
//! The untyped form is still there — [`crate::core::CoreClient::call`] takes
//! wire-named pairs, and always will, because that is what makes an operation
//! this crate never named still reachable. But a pair list is checked against
//! the contract at *runtime*: a misspelled `payolad` is refused when the call
//! is made, which is late for a name that was wrong the moment it was typed.
//! These structs move the spelling to compile time and leave the runtime check
//! exactly where it was, as the backstop for the untyped route.
//!
//! # Why a request enum is closed and a response string is not
//!
//! [`super`]'s models keep `status` and `kind` as `String` because an added
//! variant must never fail a decode. A *request* parameter is the opposite
//! situation: the value is the client's to choose, the contract declares the
//! closed set the server accepts, and a value outside it is a 400 the client
//! could have prevented. So where the document declares an `enum`, this module
//! declares one too — and [`super::coverage`] holds the two together.

use serde::{Deserialize, Serialize};

/// One operation's parameters, in the shape the contract declares them.
pub trait ContractParams {
    /// The `operationId` these parameters belong to.
    const OPERATION: &'static str;

    /// The wire pairs to send: set parameters only, under the contract's own
    /// names.
    ///
    /// An unset optional parameter is omitted rather than sent as a default,
    /// so the server's default applies and this client never has to be
    /// updated when one of them changes.
    fn values(&self) -> Vec<(&'static str, String)>;
}

/// A parameter whose accepted values the contract closes with an `enum`.
pub trait ContractEnum: Sized + Copy {
    /// Where the contract declares this set: `(operationId, parameter)`, once
    /// per operation that takes it.
    const DECLARED_BY: &'static [(&'static str, &'static str)];

    /// Every value, in the document's own spelling.
    const VALUES: &'static [&'static str];

    /// This value, as it travels.
    fn as_str(self) -> &'static str;
}

/// How much of a span's payload a trace read should carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadDetail {
    /// Whole payloads.
    Full,
    /// Truncated payloads, with a marker on the spans that were cut.
    Preview,
}

impl ContractEnum for PayloadDetail {
    const DECLARED_BY: &'static [(&'static str, &'static str)] =
        &[("getSessionTraces", "payload"), ("getTrace", "payload")];
    const VALUES: &'static [&'static str] = &["full", "preview"];

    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Preview => "preview",
        }
    }
}

/// The grain an export is written at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportDetail {
    /// One record per span.
    Spans,
    /// One record per trace.
    Traces,
}

impl ContractEnum for ExportDetail {
    const DECLARED_BY: &'static [(&'static str, &'static str)] =
        &[("exportSession", "detail"), ("exportSessions", "detail")];
    const VALUES: &'static [&'static str] = &["spans", "traces"];

    fn as_str(self) -> &'static str {
        match self {
            Self::Spans => "spans",
            Self::Traces => "traces",
        }
    }
}

/// Which way a listing is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    /// Oldest, or lowest, first.
    Asc,
    /// Newest, or highest, first.
    Desc,
}

impl ContractEnum for SortDirection {
    const DECLARED_BY: &'static [(&'static str, &'static str)] = &[("listSessions", "direction")];
    const VALUES: &'static [&'static str] = &["asc", "desc"];

    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// Whose skills a listing covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillScope {
    /// Everything the caller may see.
    All,
    /// Only the caller's own.
    Mine,
    /// Everyone else's.
    Team,
}

impl ContractEnum for SkillScope {
    const DECLARED_BY: &'static [(&'static str, &'static str)] = &[("listSkills", "scope")];
    const VALUES: &'static [&'static str] = &["all", "mine", "team"];

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mine => "mine",
            Self::Team => "team",
        }
    }
}

/// How a skills listing is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSort {
    /// Most downloaded first.
    Downloads,
}

impl ContractEnum for SkillSort {
    const DECLARED_BY: &'static [(&'static str, &'static str)] = &[("listSkills", "sort")];
    const VALUES: &'static [&'static str] = &["downloads"];

    fn as_str(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
        }
    }
}

/// `GET /v1/sessions` — the sessions listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionListParams {
    /// How many sessions to return.
    pub limit: Option<u32>,
    /// The cursor from a previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Which column to order by.
    pub sort: Option<String>,
    /// Which way to order it.
    pub direction: Option<SortDirection>,
    /// Lower bound on activity, as an RFC 3339 timestamp.
    pub since: Option<String>,
    /// Upper bound on activity, as an RFC 3339 timestamp.
    pub until: Option<String>,
    /// Only sessions captured from this harness.
    pub harness_id: Option<String>,
    /// Only the session with this harness-side id.
    pub harness_session_id: Option<String>,
    /// Only sessions captured for this authenticated subject.
    pub auth_subject: Option<String>,
}

impl ContractParams for SessionListParams {
    const OPERATION: &'static str = "listSessions";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        push_num(&mut values, "limit", self.limit);
        push(&mut values, "cursor", self.cursor.as_deref());
        push(&mut values, "sort", self.sort.as_deref());
        push_enum(&mut values, "direction", self.direction);
        push(&mut values, "since", self.since.as_deref());
        push(&mut values, "until", self.until.as_deref());
        push(&mut values, "harness_id", self.harness_id.as_deref());
        push(
            &mut values,
            "harness_session_id",
            self.harness_session_id.as_deref(),
        );
        push(&mut values, "auth_subject", self.auth_subject.as_deref());
        values
    }
}

/// `GET /v1/sessions/{id}/traces` — the derived span read model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTracesParams {
    /// How much of each span's payload to carry.
    pub payload: Option<PayloadDetail>,
}

impl ContractParams for SessionTracesParams {
    const OPERATION: &'static str = "getSessionTraces";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        push_enum(&mut values, "payload", self.payload);
        values
    }
}

/// `GET /v1/traces/{trace_id}` — one trace with its spans.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceParams {
    /// How much of each span's payload to carry.
    pub payload: Option<PayloadDetail>,
}

impl ContractParams for TraceParams {
    const OPERATION: &'static str = "getTrace";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        push_enum(&mut values, "payload", self.payload);
        values
    }
}

/// `GET /v1/traces` — the trace summaries for one session.
///
/// `session_id` is required: the contract scopes this listing with it, and a
/// call without one is refused before it is sent rather than answering a
/// different question than the caller asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceListParams {
    /// The session whose traces to list.
    pub session_id: String,
}

impl ContractParams for TraceListParams {
    const OPERATION: &'static str = "listTraces";

    fn values(&self) -> Vec<(&'static str, String)> {
        vec![("session_id", self.session_id.clone())]
    }
}

/// `GET /v1/search/spans` — semantic search over span embeddings.
///
/// `query` is required, for the same reason [`TraceListParams::session_id`] is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchSpansParams {
    /// The search text.
    pub query: String,
    /// How many hits to return.
    pub top_k: Option<u32>,
}

impl ContractParams for SearchSpansParams {
    const OPERATION: &'static str = "searchSpans";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = vec![("query", self.query.clone())];
        push_num(&mut values, "top_k", self.top_k);
        values
    }
}

/// `GET /v1/sessions/{id}/export` — one session's export stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportSessionParams {
    /// The grain to write.
    pub detail: Option<ExportDetail>,
}

impl ContractParams for ExportSessionParams {
    const OPERATION: &'static str = "exportSession";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        push_enum(&mut values, "detail", self.detail);
        values
    }
}

/// `GET /v1/sessions/export` — every session in a window, streamed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportSessionsParams {
    /// Lower bound, as an RFC 3339 timestamp.
    pub since: Option<String>,
    /// Upper bound, as an RFC 3339 timestamp.
    pub until: Option<String>,
    /// The grain to write.
    pub detail: Option<ExportDetail>,
}

impl ContractParams for ExportSessionsParams {
    const OPERATION: &'static str = "exportSessions";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        push(&mut values, "since", self.since.as_deref());
        push(&mut values, "until", self.until.as_deref());
        push_enum(&mut values, "detail", self.detail);
        values
    }
}

/// `GET /v1/skills` — the skills listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillsListParams {
    /// How many skills to return.
    pub limit: Option<u32>,
    /// The cursor from a previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Free-text search.
    pub q: Option<String>,
    /// Whose skills to list.
    pub scope: Option<SkillScope>,
    /// How to order them.
    pub sort: Option<SkillSort>,
}

impl ContractParams for SkillsListParams {
    const OPERATION: &'static str = "listSkills";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        push_num(&mut values, "limit", self.limit);
        push(&mut values, "cursor", self.cursor.as_deref());
        push(&mut values, "q", self.q.as_deref());
        push_enum(&mut values, "scope", self.scope);
        push_enum(&mut values, "sort", self.sort);
        values
    }
}

/// `GET /v1/stats` — the aggregate rollups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsParams {
    /// Lower bound, as an RFC 3339 timestamp.
    pub since: Option<String>,
    /// Upper bound, as an RFC 3339 timestamp.
    pub until: Option<String>,
}

impl ContractParams for StatsParams {
    const OPERATION: &'static str = "getStats";

    fn values(&self) -> Vec<(&'static str, String)> {
        let mut values = Vec::new();
        push(&mut values, "since", self.since.as_deref());
        push(&mut values, "until", self.until.as_deref());
        values
    }
}

fn push(values: &mut Vec<(&'static str, String)>, wire: &'static str, value: Option<&str>) {
    if let Some(value) = value {
        values.push((wire, value.to_owned()));
    }
}

fn push_num(values: &mut Vec<(&'static str, String)>, wire: &'static str, value: Option<u32>) {
    if let Some(value) = value {
        values.push((wire, value.to_string()));
    }
}

fn push_enum<E: ContractEnum>(
    values: &mut Vec<(&'static str, String)>,
    wire: &'static str,
    value: Option<E>,
) {
    if let Some(value) = value {
        values.push((wire, value.as_str().to_owned()));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_optional_parameter_is_omitted_rather_than_defaulted() {
        // The omit-when-unset rule, at the typed layer: sending `limit=50`
        // because the caller said nothing would pin this client to today's
        // server default forever.
        assert!(SessionListParams::default().values().is_empty());
    }

    #[test]
    fn a_set_parameter_travels_under_the_contracts_own_name() {
        let params = SessionListParams {
            limit: Some(25),
            direction: Some(SortDirection::Desc),
            ..Default::default()
        };
        assert_eq!(
            params.values(),
            vec![("limit", "25".to_owned()), ("direction", "desc".to_owned())],
        );
    }

    #[test]
    fn a_required_parameter_is_always_sent_even_when_it_is_empty() {
        // Empty is a value the server can reject in its own words; omitting it
        // is a differently-shaped request the contract layer would refuse.
        assert_eq!(
            SearchSpansParams::default().values(),
            vec![("query", String::new())],
        );
    }
}
