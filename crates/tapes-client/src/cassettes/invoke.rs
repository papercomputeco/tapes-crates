//! Fetching cassette documents and executing described calls.
//!
//! Everything here is a [`crate::transport::TapesTransport`] send plus the
//! shared decode. That is the point: a discovered-cassette call and a
//! sealed-contract call are the same request through the same pipeline, and the
//! only difference is which operation table described it.
//!
//! # Discovery is data, not authority
//!
//! Discovery publishes each cassette's `openapi_path`, and this client fetches
//! what it published rather than a path it builds — so the server stays free to
//! move the route. That makes the published value untrusted input:
//! [`guard_spec_path`] refuses anything that is not a plain server-relative
//! path, because `Url::join` treats an absolute URL as a *replacement* and
//! `//host/path` is a protocol-relative reference that changes the authority
//! while surviving a naive leading-slash check.
//!
//! The syntactic guard lives here, where the path arrives. The origin backstop
//! — did the answer come from the server the user configured? — lives in the
//! transport, because the base URL is the transport's and this layer has never
//! seen it.

use serde_json::Value;
use snafu::ResultExt;

use crate::decode;
use crate::error::{Result, error};
use crate::transport::{Call, SpecFetch, TapesTransport};

/// The route that lists a deployment's cassettes.
pub const DISCOVERY_PATH: &str = "/v1/cassettes";

/// HTTP `304 Not Modified` — a matched validator, and the whole reason a
/// conditional spec fetch is cheap.
const NOT_MODIFIED: u16 = 304;

/// Execute one described call and decode the JSON response.
///
/// The verb, path, and parameter names all come from the document rather than
/// from anything hand-written here, which is the whole point of a generated
/// surface.
pub async fn invoke<T: TapesTransport>(transport: &T, call: &Call<'_>) -> Result<Value> {
    let response = transport.send(call).await.context(error::TransportSnafu)?;
    decode::json(&response)
}

/// `GET /v1/cassettes` — the cassette discovery document, raw.
///
/// Returned raw for the same reason a consumer's reads should be: discovery
/// grows fields, and a partial model would eat them.
pub async fn fetch_discovery<T: TapesTransport>(transport: &T) -> Result<Value> {
    invoke(
        transport,
        &Call {
            method: "GET",
            path: DISCOVERY_PATH,
            ..Default::default()
        },
    )
    .await
}

/// Conditionally fetch one cassette's OpenAPI document.
///
/// `path` is the `openapi_path` discovery published; `etag` is the validator
/// from a previous fetch. A matching validator is answered with 304 and an
/// empty body, which is what makes revalidating a cached surface cheap.
pub async fn fetch_spec<T: TapesTransport>(
    transport: &T,
    path: &str,
    etag: Option<&str>,
) -> Result<SpecFetch> {
    guard_spec_path(path)?;

    let mut call = Call {
        method: "GET",
        path,
        ..Default::default()
    };
    if let Some(etag) = etag {
        call.headers
            .push(("if-none-match".to_owned(), etag.to_owned()));
    }

    let response = transport.send(&call).await.context(error::TransportSnafu)?;

    // Checked before anything else looks at the status: a matched validator is
    // a successful answer that happens to be a 3xx, and reading it as a
    // redirect would make the cheap path — the one this whole conditional
    // fetch exists for — unreachable.
    if response.status == NOT_MODIFIED {
        return Ok(SpecFetch::Unchanged);
    }

    let etag = response.header("etag").map(ToOwned::to_owned);
    let document = decode::json(&response)?;
    Ok(SpecFetch::Fetched { document, etag })
}

/// Refuse an `openapi_path` that could move the request off the server.
///
/// A single leading slash keeps the request on the server that served
/// discovery. `//host/path` is a protocol-relative reference that `Url::join`
/// resolves onto a different host entirely, so it is rejected up front rather
/// than left to the origin backstop.
pub fn guard_spec_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path.starts_with("//") {
        return error::SpecPathSnafu {
            path: path.to_owned(),
        }
        .fail();
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::transport::{TransportError, WireRequest, WireResponse};
    use std::cell::RefCell;

    /// One recorded request: the path asked for, and the headers it carried.
    type Seen = Vec<(String, Vec<(String, String)>)>;

    /// A transport that answers with a canned response and records the
    /// requests it was handed.
    struct Canned {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        seen: RefCell<Seen>,
    }

    impl Canned {
        fn new(status: u16, body: &str) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
                seen: RefCell::new(Vec::new()),
            }
        }

        fn with_header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_owned(), value.to_owned()));
            self
        }
    }

    impl TapesTransport for Canned {
        async fn send(
            &self,
            request: &WireRequest<'_>,
        ) -> std::result::Result<WireResponse, TransportError> {
            self.seen
                .borrow_mut()
                .push((request.path.to_owned(), request.headers.clone()));
            Ok(WireResponse::new(
                self.status,
                format!("http://tapes.test{}", request.path),
                self.headers.clone(),
                self.body.clone(),
            ))
        }
    }

    #[tokio::test]
    async fn a_spec_path_may_not_change_the_request_authority() {
        // `//host/path` is protocol-relative: it survives a naive
        // leading-slash check while a join moves the request onto a different
        // host. Nothing may be sent for any of these.
        let transport = Canned::new(200, "{}");
        for path in ["//evil.example/spec.json", "relative/spec.json", ""] {
            let err = fetch_spec(&transport, path, None).await.unwrap_err();
            assert!(
                err.to_string().contains("non-relative OpenAPI path"),
                "{path:?} produced the wrong error: {err}",
            );
        }
        assert!(
            transport.seen.borrow().is_empty(),
            "a refused path must never reach the transport",
        );
    }

    #[tokio::test]
    async fn a_matched_validator_is_unchanged_rather_than_an_error() {
        // The two surfaces used to disagree here: the cassette transport
        // refused every 3xx before it looked for a 304, so a matched validator
        // errored and `Unchanged` was unreachable. The cache treated the error
        // as "keep the previous entry", which is why nothing observably broke
        // — but the two paths through this crate must not answer the same
        // server differently, so the conditional answer is read first.
        let transport = Canned::new(304, "");
        let got = fetch_spec(
            &transport,
            "/v1/cassettes/x/openapi.json",
            Some("\"sha256:abc\""),
        )
        .await
        .unwrap();
        assert!(matches!(got, SpecFetch::Unchanged), "got: {got:?}");

        let seen = transport.seen.borrow();
        assert_eq!(
            seen[0].1,
            vec![("if-none-match".to_owned(), "\"sha256:abc\"".to_owned())],
            "the validator must travel with the request",
        );
    }

    #[tokio::test]
    async fn a_fetched_spec_carries_the_validator_for_next_time() {
        let transport =
            Canned::new(200, r#"{"openapi":"3.1.0"}"#).with_header("ETag", "\"sha256:def\"");
        let got = fetch_spec(&transport, "/v1/cassettes/x/openapi.json", None)
            .await
            .unwrap();
        match got {
            SpecFetch::Fetched { document, etag } => {
                assert_eq!(document["openapi"], "3.1.0");
                // Read case-insensitively: a transport that never had an HTTP
                // library to normalise headers spells it however it likes.
                assert_eq!(etag.as_deref(), Some("\"sha256:def\""));
            }
            SpecFetch::Unchanged => panic!("expected a document"),
        }
    }

    #[tokio::test]
    async fn an_error_body_is_surfaced_with_the_status() {
        let transport = Canned::new(400, r#"{"error":"invalid cursor"}"#);
        let err = fetch_discovery(&transport).await.unwrap_err();
        let rendered = format!("{err}");
        assert!(rendered.contains("400"), "got: {rendered}");
        assert!(rendered.contains("invalid cursor"), "got: {rendered}");
    }

    #[tokio::test]
    async fn a_successful_empty_body_is_null_not_a_decode_failure() {
        let transport = Canned::new(204, "");
        assert_eq!(fetch_discovery(&transport).await.unwrap(), Value::Null);
    }

    #[tokio::test]
    async fn discovery_asks_for_the_documented_route() {
        let transport = Canned::new(200, "{}");
        let _ = fetch_discovery(&transport).await.unwrap();
        assert_eq!(transport.seen.borrow()[0].0, DISCOVERY_PATH);
    }
}
