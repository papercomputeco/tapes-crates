//! The search cassette's span search, typed once for every consumer.
//!
//! Search is a cassette a deployment serves, not an operation of the sealed
//! core contract — so its route and shapes are not vendored, and the generic
//! cassette surface reaches it like any other. But both command-line clients
//! render hits, and each used to decode the response with a private model that
//! named only the fields its own renderer read. A field the server added
//! reached neither, and the two disagreed about which fields existed at all.
//!
//! So the hit model lives here, with every field the search cassette
//! publishes and a flattened `extra` for the ones it adds later: a hit that
//! passes through [`SpanSearchHit`] re-encodes with nothing dropped.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::decode;
use crate::envelope::ListEnvelope;
use crate::error::Result;
use crate::transport::{Call, TapesTransport};

/// The search cassette's span route.
pub const SEARCH_SPANS_ROUTE: &str = "/v1/cassettes/search/spans";

/// One span search, as the caller asks it.
///
/// Every filter is optional and omitted when unset, so the cassette's own
/// default applies. `top_k` is likewise omitted when unset; a CLI whose flag
/// carries a default passes it through as `Some`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// The natural-language query.
    pub query: String,
    /// How many hits to return.
    pub top_k: Option<u32>,
    /// Only hits from sessions with exactly this auth subject.
    pub auth_subject: Option<String>,
    /// Only hits from sessions whose subject matches this value — the same
    /// match rule as the sessions listing's `subject`. Resolve `me` first.
    pub subject: Option<String>,
    /// Only hits from sessions whose working directory starts with this.
    pub cwd: Option<String>,
    /// Only hits from sessions captured from this harness id.
    pub harness: Option<String>,
    /// Only hits from turns started at or after this RFC 3339 timestamp.
    pub since: Option<String>,
    /// Only hits from turns started before this RFC 3339 timestamp.
    pub until: Option<String>,
    /// Only hits from sessions that are (or are not) live right now.
    pub live: Option<bool>,
    /// Further query pairs appended verbatim, in order, after the ones above
    /// — filter params another cassette claims on this route (a deployment's
    /// `label`, say). The names are data: nothing here validates them.
    pub extra: Vec<(String, String)>,
}

impl SearchQuery {
    /// A query with no filters.
    #[must_use]
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Self::default()
        }
    }

    /// The query pairs this search sends, in a stable order.
    #[must_use]
    pub fn pairs(&self) -> Vec<(String, String)> {
        let mut pairs = vec![("query".to_owned(), self.query.clone())];
        let mut push = |name: &str, value: Option<String>| {
            if let Some(value) = value {
                pairs.push((name.to_owned(), value));
            }
        };
        push("top_k", self.top_k.map(|n| n.to_string()));
        push("auth_subject", self.auth_subject.clone());
        push("subject", self.subject.clone());
        push("cwd", self.cwd.clone());
        push("harness", self.harness.clone());
        push("since", self.since.clone());
        push("until", self.until.clone());
        push("live", self.live.map(|live| live.to_string()));
        pairs.extend(self.extra.iter().cloned());
        pairs
    }
}

/// The span search response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SpanSearchResponse {
    /// The query the server ran, echoed back.
    pub query: String,
    /// The ranked hits, best first. An explicit `null` decodes as empty.
    #[serde(deserialize_with = "crate::core::models::null_default")]
    pub results: Vec<SpanSearchHit>,
    /// Everything else the server said, kept verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl SpanSearchResponse {
    /// The hits as the shared list-output shape:
    /// `{"items": [...], "query": "...", "top_k": n}`.
    ///
    /// `query` is the server's echo when it sent one, else `asked`; `top_k`
    /// is printed only when the caller set one.
    #[must_use]
    pub fn into_envelope(self, asked: &SearchQuery) -> ListEnvelope<SpanSearchHit> {
        let query = if self.query.is_empty() {
            asked.query.clone()
        } else {
            self.query
        };
        let envelope = ListEnvelope::new(self.results).with("query", query);
        match asked.top_k {
            Some(top_k) => envelope.with("top_k", top_k),
            None => envelope,
        }
    }

    /// Session ids of the hits, deduplicated, in rank order.
    #[must_use]
    pub fn session_ids(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = Vec::new();
        for hit in &self.results {
            let id = hit.session_id.as_str();
            if !id.is_empty() && !seen.contains(&id) {
                seen.push(id);
            }
        }
        seen
    }
}

/// One span hit, with its turn and its session's context.
///
/// Every published field is named; an absent one decodes to its default, so
/// "missing" is the empty string. Fields this build has never heard of land
/// in `extra` and re-encode untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SpanSearchHit {
    /// The hit's relevance score.
    pub score: f64,
    /// The session the span belongs to.
    pub session_id: String,
    /// The trace the span belongs to.
    pub trace_id: String,
    /// The span's id.
    pub span_id: String,
    /// The span's start, an RFC 3339 timestamp.
    pub started_at: String,
    /// The prompt of the turn the span belongs to; empty for a synthetic
    /// turn.
    pub user_prompt: String,
    /// Preview of the matched span's delta-only text.
    pub snippet: String,
    /// The model of the turn the span belongs to.
    pub model: String,
    /// The session's auth subject.
    pub auth_subject: String,
    /// The latest known name for the auth subject; empty when unknown.
    pub auth_subject_name: String,
    /// The latest known email for the auth subject; empty when unknown.
    pub auth_subject_email: String,
    /// The session's working directory.
    pub cwd: String,
    /// The harness the session was captured from.
    pub harness_id: String,
    /// The session's display title: its rename, else its derived title.
    pub display_title: String,
    /// When the session was last seen, an RFC 3339 timestamp.
    pub last_seen_at: String,
    /// Whether the session is live: no recorded end and seen recently.
    pub live: bool,
    /// Everything else the server said about the hit, kept verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /v1/cassettes/search/spans` — run one span search.
///
/// # Errors
///
/// Any transport, status, or decode failure; see [`crate::Error`]. A
/// deployment without the search cassette answers with a status error.
pub async fn search_spans<T: TapesTransport>(
    transport: &T,
    query: &SearchQuery,
) -> Result<SpanSearchResponse> {
    let call = Call {
        method: "GET",
        path: SEARCH_SPANS_ROUTE,
        query: query.pairs(),
        ..Call::default()
    };
    let value = crate::cassettes::invoke(transport, &call).await?;
    decode::typed(value)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::path::{PathMode, call_url};
    use crate::transport::{TransportError, WireRequest, WireResponse};
    use serde_json::json;
    use std::cell::RefCell;
    use url::Url;

    struct Canned {
        body: Value,
        seen: RefCell<Vec<String>>,
    }

    impl TapesTransport for Canned {
        async fn send(
            &self,
            request: &WireRequest<'_>,
        ) -> std::result::Result<WireResponse, TransportError> {
            let base = Url::parse("http://127.0.0.1:8081").unwrap();
            let url = call_url(&base, request, PathMode::UnderBase)
                .map_err(|error| TransportError::new(error.to_string()))?;
            self.seen.borrow_mut().push(url.to_string());
            Ok(WireResponse::new(
                200,
                url.to_string(),
                Vec::new(),
                self.body.to_string().into_bytes(),
            ))
        }
    }

    fn hit() -> Value {
        json!({
            "trace_id": "t-1", "span_id": "sp-1", "session_id": "s-1",
            "score": 0.75, "user_prompt": "fix auth", "snippet": "redirect",
            "started_at": "2026-10-01T00:00:00Z", "model": "claude",
            "auth_subject": "user_01", "auth_subject_name": "Ada Lovelace",
            "auth_subject_email": "ada@example.com", "cwd": "/src/app",
            "harness_id": "claude", "display_title": "Auth fix",
            "last_seen_at": "2026-10-01T00:05:00Z", "live": true,
            "a_field_from_the_future": {"k": 1}
        })
    }

    #[tokio::test]
    async fn every_filter_travels_under_its_wire_name() {
        let transport = Canned {
            body: json!({"query": "auth", "count": 0, "results": []}),
            seen: RefCell::new(Vec::new()),
        };
        let query = SearchQuery {
            query: "auth".to_owned(),
            top_k: Some(5),
            auth_subject: Some("user_01".to_owned()),
            subject: Some("ada".to_owned()),
            cwd: Some("/src".to_owned()),
            harness: Some("claude".to_owned()),
            since: Some("2026-10-01T00:00:00Z".to_owned()),
            until: Some("2026-10-02T00:00:00Z".to_owned()),
            live: Some(true),
            extra: vec![("label".to_owned(), "bug".to_owned())],
        };
        let response = search_spans(&transport, &query).await.unwrap();
        assert_eq!(
            transport.seen.borrow()[0],
            "http://127.0.0.1:8081/v1/cassettes/search/spans?query=auth&top_k=5\
             &auth_subject=user_01&subject=ada&cwd=%2Fsrc&harness=claude\
             &since=2026-10-01T00%3A00%3A00Z&until=2026-10-02T00%3A00%3A00Z&live=true&label=bug",
        );
        assert_eq!(response.extra["count"], 0);
    }

    #[tokio::test]
    async fn an_unset_filter_is_not_sent() {
        let transport = Canned {
            body: json!({"results": null}),
            seen: RefCell::new(Vec::new()),
        };
        let response = search_spans(&transport, &SearchQuery::new("x"))
            .await
            .unwrap();
        assert_eq!(
            transport.seen.borrow()[0],
            "http://127.0.0.1:8081/v1/cassettes/search/spans?query=x",
        );
        assert!(response.results.is_empty());
    }

    #[test]
    fn a_hit_carries_every_published_field_and_keeps_unknown_ones() {
        let parsed: SpanSearchHit = serde_json::from_value(hit()).unwrap();
        assert_eq!(parsed.auth_subject_name, "Ada Lovelace");
        assert_eq!(parsed.display_title, "Auth fix");
        assert!(parsed.live);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), hit());
    }

    #[test]
    fn the_envelope_is_items_query_and_top_k() {
        let response: SpanSearchResponse =
            serde_json::from_value(json!({"query": "", "results": [hit()]})).unwrap();
        let mut asked = SearchQuery::new("auth");
        asked.top_k = Some(3);
        let envelope = serde_json::to_value(response.into_envelope(&asked)).unwrap();
        assert_eq!(envelope["query"], "auth");
        assert_eq!(envelope["top_k"], 3);
        assert_eq!(envelope["items"][0]["session_id"], "s-1");
        assert!(envelope.get("next_cursor").is_none());
    }

    #[test]
    fn session_ids_are_deduplicated_in_rank_order() {
        let response: SpanSearchResponse = serde_json::from_value(json!({"results": [
            {"session_id": "a"}, {"session_id": "b"}, {"session_id": "a"},
            {"session_id": ""}, {"session_id": "c"}
        ]}))
        .unwrap();
        assert_eq!(response.session_ids(), vec!["a", "b", "c"]);
    }
}
