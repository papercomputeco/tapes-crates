//! Assembling one OpenAPI-described call into a URL.
//!
//! Extracted from tapesctl's `api::client`, where these helpers backed both
//! the generated cassette surface and the vendored core contract. The server
//! owns the shape on both sides; this module only routes values into the
//! places the document declared for them.

use snafu::ResultExt;
use url::Url;

use crate::error::{Result, error};

/// One call against an OpenAPI-described route — a runtime-discovered cassette
/// operation, or a core operation from a vendored contract.
#[derive(Debug, Default, Clone)]
pub struct Call<'a> {
    /// The HTTP verb, uppercased.
    pub method: &'a str,
    /// The public path template, `{name}` placeholders included.
    pub path: &'a str,
    /// Values for those placeholders, by placeholder name.
    pub path_params: Vec<(String, String)>,
    /// Query parameters, under their wire names.
    pub query: Vec<(String, String)>,
    /// Header parameters, under their wire names.
    pub headers: Vec<(String, String)>,
    /// A JSON request body, when the operation takes one.
    pub body: Option<String>,
}

/// Replace the `{name}` placeholders in one path segment.
///
/// The result is pushed through `path_segments_mut`, which percent-encodes the
/// whole segment — so a value containing a slash stays one segment rather than
/// addressing a different route.
fn substitute(segment: &str, path_params: &[(String, String)]) -> String {
    let mut rendered = segment.to_owned();
    for (name, value) in path_params {
        rendered = rendered.replace(&format!("{{{name}}}"), value);
    }
    rendered
}

/// Remove the empty query `query_pairs_mut` leaves behind when no pair was
/// appended.
///
/// `url.query_pairs_mut()` sets the query to `Some("")` the moment it is
/// called, so a request with every parameter unset would go out as
/// `/v1/sessions?`. Servers ignore it, but it means the same request has two
/// spellings — which shows up in logs, in cached URLs, and in any test that
/// compares them.
fn drop_empty_query(url: &mut Url) {
    if url.query() == Some("") {
        url.set_query(None);
    }
}

/// Build the URL for one described call against a base.
///
/// Path parameters are substituted into their segment and the segment is then
/// pushed through `path_segments_mut`, which percent-encodes it whole. A value
/// containing a slash therefore stays one segment instead of addressing a
/// different route.
pub fn call_url(base: &Url, call: &Call<'_>) -> Result<Url> {
    let mut url = base.join("/").context(error::UrlSnafu)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| error::NotABaseSnafu.build())?;
        segments.clear();
        for segment in call.path.split('/').filter(|s| !s.is_empty()) {
            segments.push(&substitute(segment, &call.path_params));
        }
    }
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in &call.query {
            query.append_pair(name, value);
        }
    }
    drop_empty_query(&mut url);
    Ok(url)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn base(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    #[test]
    fn a_path_value_is_encoded_as_one_path_segment() {
        // A raw join would let `../` in a value address a different route.
        let call = Call {
            method: "GET",
            path: "/v1/sessions/{id}/traces",
            path_params: vec![("id".to_owned(), "../admin/seed/demo".to_owned())],
            ..Default::default()
        };
        let url = call_url(&base("http://127.0.0.1:8081"), &call).unwrap();
        assert!(
            url.as_str()
                .starts_with("http://127.0.0.1:8081/v1/sessions/"),
            "got: {url}",
        );
        assert!(!url.path().contains("/admin/"), "got: {url}");
    }

    #[test]
    fn an_empty_query_set_leaves_no_bare_question_mark() {
        let call = Call {
            method: "GET",
            path: "/v1/sessions",
            ..Default::default()
        };
        let url = call_url(&base("http://127.0.0.1:8081"), &call).unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:8081/v1/sessions");
    }

    #[test]
    fn query_values_are_percent_encoded_under_their_wire_names() {
        let call = Call {
            method: "GET",
            path: "/v1/sessions",
            query: vec![
                ("limit".to_owned(), "25".to_owned()),
                ("since".to_owned(), "2026-07-01T00:00:00Z".to_owned()),
            ],
            ..Default::default()
        };
        let url = call_url(&base("http://127.0.0.1:8081"), &call).unwrap();
        let query = url.query().unwrap();
        assert!(query.contains("limit=25"), "got: {query}");
        assert!(
            query.contains("since=2026-07-01T00%3A00%3A00Z"),
            "the timestamp must be percent-encoded: {query}",
        );
    }

    #[test]
    fn a_base_url_with_a_path_prefix_still_resolves_to_the_api_route() {
        let call = Call {
            method: "GET",
            path: "/v1/sessions",
            ..Default::default()
        };
        let url = call_url(&base("http://127.0.0.1:8081/base/"), &call).unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:8081/v1/sessions");
    }
}
