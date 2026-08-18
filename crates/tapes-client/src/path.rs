//! Turning one contract-described call into a URL against a base.
//!
//! # Two bases, two meanings
//!
//! A standalone client points at a tapes server's root: `http://host:8081`,
//! and `/v1/sessions` is the whole path. A platform client points at a gateway
//! that mounts tapes under a prefix — `https://<slug>.<host>/<gateway>/tapes/`
//! — and `/v1/sessions` has to land *under* that prefix or the request leaves
//! for the cloud edge root, most likely a 404 and conceivably a wrong-gateway
//! route, neither of which looks like a URL bug at the call site.
//!
//! Those are not the same operation, and a builder that silently picks one is
//! wrong for the other client. [`PathMode`] makes the caller say which, and
//! both are pinned by their own test below.
//!
//! The percent-encoding rules are shared by both modes and are the reason this
//! belongs in one place: a path value is substituted into its segment and the
//! segment is pushed whole through `path_segments_mut`, so a value containing
//! `../` stays one segment instead of addressing a different route.
//!
//! Both surfaces join here. A cassette route and a sealed-contract route are
//! the same kind of string against the same kind of base, and when the two had
//! a builder each only one of them had learned about gateway prefixes.

use crate::transport::Call;
use snafu::ResultExt;
use url::Url;

use crate::error::{Result, error};

/// How a contract path template is joined onto a base URL.
///
/// The default is [`PathMode::Direct`], which is the behaviour every
/// existing caller of the shared builder already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathMode {
    /// The path template is root-absolute: any path prefix on the base is
    /// dropped, and `/v1/sessions` addresses the server's own root.
    #[default]
    Direct,
    /// The path template is joined *under* the base's path, preserving a
    /// gateway prefix such as `/<gateway>/tapes/`.
    UnderBase,
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

/// Build the URL for one described call against a base, in the given mode.
pub fn call_url(base: &Url, call: &Call<'_>, mode: PathMode) -> Result<Url> {
    let mut url = match mode {
        // `join("/")` resets to the origin root and drops any query or
        // fragment the base carried.
        PathMode::Direct => base.join("/").context(error::UrlSnafu)?,
        PathMode::UnderBase => {
            let mut url = base.clone();
            // A base is a mount point, not a request: anything after the path
            // is not ours to inherit, and leaving a query on it would merge
            // with the call's own parameters below.
            url.set_query(None);
            url.set_fragment(None);
            url
        }
    };

    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| error::NotABaseSnafu.build())?;
        match mode {
            PathMode::Direct => {
                segments.clear();
            }
            PathMode::UnderBase => {
                // A base is conventionally written with a trailing slash,
                // which `url` models as a final empty segment. Left in place
                // it would produce `/gateway/tapes//v1/sessions`, so the two
                // spellings of the same base are normalised to one here.
                segments.pop_if_empty();
            }
        }
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

    fn sessions() -> Call<'static> {
        Call {
            method: "GET",
            path: "/v1/sessions",
            ..Default::default()
        }
    }

    #[test]
    fn direct_is_the_default_mode() {
        // The mode every existing caller of the shared builder already has.
        assert_eq!(PathMode::default(), PathMode::Direct);
    }

    #[test]
    fn a_direct_join_drops_a_base_path_prefix() {
        // A standalone client's `/v1/sessions` addresses the server root, and
        // this is the behaviour the cassette transport has always had.
        let url = call_url(
            &base("http://127.0.0.1:8081/base/"),
            &sessions(),
            PathMode::Direct,
        )
        .unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:8081/v1/sessions");
    }

    #[test]
    fn an_under_base_join_preserves_a_gateway_path_prefix() {
        // The exact inverse of the root-absolute case, and the reason
        // `PathMode` exists: a platform client's base mounts tapes under a
        // gateway prefix, and a builder that reset to root would retarget
        // every request at the cloud edge.
        let url = call_url(
            &base("https://acme.cloud.example/primary/tapes/"),
            &sessions(),
            PathMode::UnderBase,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://acme.cloud.example/primary/tapes/v1/sessions",
        );
    }

    #[test]
    fn an_under_base_join_reads_both_spellings_of_the_same_base() {
        // A base written without its trailing slash is the same mount point;
        // `url` models the slash as a final empty segment, which left in
        // place would double the separator.
        for raw in [
            "https://acme.cloud.example/primary/tapes/",
            "https://acme.cloud.example/primary/tapes",
        ] {
            let url = call_url(&base(raw), &sessions(), PathMode::UnderBase).unwrap();
            assert_eq!(
                url.as_str(),
                "https://acme.cloud.example/primary/tapes/v1/sessions",
                "base {raw:?}",
            );
        }
    }

    #[test]
    fn an_under_base_join_against_a_bare_origin_matches_the_direct_result() {
        // With no prefix to preserve the two modes must agree, or a
        // standalone deployment would depend on which mode its client picked.
        let bare = base("http://127.0.0.1:8081");
        let rooted = base("http://127.0.0.1:8081/");
        let expected = "http://127.0.0.1:8081/v1/sessions";
        for candidate in [&bare, &rooted] {
            assert_eq!(
                call_url(candidate, &sessions(), PathMode::UnderBase)
                    .unwrap()
                    .as_str(),
                expected,
            );
            assert_eq!(
                call_url(candidate, &sessions(), PathMode::Direct)
                    .unwrap()
                    .as_str(),
                expected,
            );
        }
    }

    #[test]
    fn a_path_value_is_encoded_as_one_path_segment_in_both_modes() {
        // A raw join would let `../` in a value climb out of the route — and
        // under a prefix, out of the gateway mount entirely.
        let call = Call {
            method: "GET",
            path: "/v1/sessions/{id}/traces",
            path_params: vec![("id".to_owned(), "../admin/seed/demo".to_owned())],
            ..Default::default()
        };
        for (mode, prefix) in [
            (PathMode::Direct, "http://127.0.0.1:8081/v1/sessions/"),
            (
                PathMode::UnderBase,
                "http://127.0.0.1:8081/primary/tapes/v1/sessions/",
            ),
        ] {
            let url = call_url(&base("http://127.0.0.1:8081/primary/tapes/"), &call, mode).unwrap();
            assert!(url.as_str().starts_with(prefix), "{mode:?} got: {url}");
            assert!(!url.path().contains("/admin/"), "{mode:?} got: {url}");
        }
    }

    #[test]
    fn an_empty_query_set_leaves_no_bare_question_mark() {
        for mode in [PathMode::Direct, PathMode::UnderBase] {
            let url = call_url(&base("http://127.0.0.1:8081"), &sessions(), mode).unwrap();
            assert_eq!(url.as_str(), "http://127.0.0.1:8081/v1/sessions");
        }
    }

    #[test]
    fn query_values_are_form_encoded_under_their_wire_names() {
        // Form encoding is the spelling both clients converge on: a space is
        // `+`, and a timestamp's colons are percent-encoded.
        let call = Call {
            method: "GET",
            path: "/v1/cassettes/search/spans",
            query: vec![
                ("query".to_owned(), "gum glow charm".to_owned()),
                ("since".to_owned(), "2026-07-01T00:00:00Z".to_owned()),
            ],
            ..Default::default()
        };
        for mode in [PathMode::Direct, PathMode::UnderBase] {
            let url = call_url(&base("http://127.0.0.1:8081"), &call, mode).unwrap();
            let query = url.query().unwrap();
            assert!(query.contains("query=gum+glow+charm"), "got: {query}");
            assert!(
                query.contains("since=2026-07-01T00%3A00%3A00Z"),
                "the timestamp must be percent-encoded: {query}",
            );
        }
    }

    #[test]
    fn a_base_query_or_fragment_is_not_inherited_by_the_request() {
        // A base is a mount point, not a request. Inheriting its query would
        // merge with the call's own parameters and produce a request nobody
        // wrote.
        let call = Call {
            method: "GET",
            path: "/v1/sessions",
            query: vec![("limit".to_owned(), "25".to_owned())],
            ..Default::default()
        };
        let url = call_url(
            &base("https://acme.example/primary/tapes/?trace=1#top"),
            &call,
            PathMode::UnderBase,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://acme.example/primary/tapes/v1/sessions?limit=25",
        );
    }

    #[test]
    fn a_base_that_cannot_carry_a_path_is_refused_rather_than_guessed_at() {
        let call = sessions();
        let err =
            call_url(&base("mailto:ops@example.com"), &call, PathMode::UnderBase).unwrap_err();
        assert!(err.to_string().contains("base"), "got: {err}");
    }
}
