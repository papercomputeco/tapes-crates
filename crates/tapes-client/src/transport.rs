//! The seam — one trait both surfaces call through.
//!
//! # Why a seam rather than a client
//!
//! The consumers of the tapes read API do not share a way of sending a request
//! and should not be made to. One speaks to a tapes server directly with no
//! credential at all; one mints a fresh token per request, sends it under a
//! header chosen so `Authorization` stays free for upstream provider
//! credentials, retries once on a 401, and pins its TLS; one carries opaque
//! frames over a local control socket and never speaks HTTP in its own process
//! at all. None of that is contract knowledge. [`TapesTransport`] is the line
//! between them: this crate decides *which request*, the implementation decides
//! *how it is sent*.
//!
//! Base resolution, authentication, retry policy, and TLS all live **inside**
//! an implementation. That is what makes the line hold: a [`WireRequest`]
//! carries a contract-relative path, not a URL, so there is no place for a
//! caller to smuggle a host into one.
//!
//! # Why the transport cannot grow verbs
//!
//! [`WireResponse`] is a status, headers, and bytes. A transport can cache,
//! multiplex, or log; it structurally cannot answer "list the sessions",
//! because the frame has no vocabulary for it. Every semantic verb therefore
//! stays in [`crate::core`] and [`crate::cassettes`], where the operation
//! tables are — which is the property that keeps one implementation of the read
//! surface rather than one per transport.

use std::fmt;

use serde_json::Value;

use crate::error::Result;

/// One call against an OpenAPI-described route — a runtime-discovered cassette
/// operation, or a core operation from a sealed contract.
///
/// The path is the document's own template, `{name}` placeholders included, and
/// is *contract-relative*: joining it onto a base is [`crate::path`]'s job, and
/// which base is the transport's. A caller therefore cannot address a host by
/// constructing one of these.
#[derive(Debug, Default, Clone)]
pub struct WireRequest<'a> {
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

/// The name this type has carried since it described only cassette calls.
///
/// Kept as an alias rather than a second struct: the sealed surface and the
/// discovered surface describe a call identically, and the two names existing
/// at all was a side effect of the two surfaces having lived in two crates.
pub type Call<'a> = WireRequest<'a>;

/// What a transport hands back: what the server said, and the bytes it said it
/// with.
///
/// Deliberately not a decoded document. Status and headers are load-bearing
/// above this layer — a 304 is the whole point of a conditional spec fetch, and
/// an error body is what names the offending parameter — and a transport that
/// decoded eagerly would have to throw one of them away.
#[derive(Debug, Clone)]
pub struct WireResponse {
    /// The HTTP status, or the transport's equivalent.
    pub status: u16,
    /// What actually answered, for diagnostics.
    ///
    /// The transport resolves the base, so it is the only layer that knows the
    /// full address a contract-relative path became — and an error message
    /// naming `/v1/sessions` when three deployments were in play is the message
    /// that costs an afternoon.
    pub endpoint: String,
    /// Response headers, in the order received.
    pub headers: Vec<(String, String)>,
    /// The response body, verbatim.
    pub body: Vec<u8>,
}

impl WireResponse {
    /// Build a response from its parts.
    #[must_use]
    pub fn new(
        status: u16,
        endpoint: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            endpoint,
            headers,
            body,
        }
    }

    /// Whether the status is in the 2xx range.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Whether the status is a redirect.
    #[must_use]
    pub fn is_redirection(&self) -> bool {
        (300..400).contains(&self.status)
    }

    /// The first value of a header, matched case-insensitively.
    ///
    /// Case-insensitive because the seam admits transports that never had an
    /// HTTP library to normalise them: a hand-built frame may spell `ETag`
    /// however its author did.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A transport-level failure, with its cause kept opaque.
///
/// Opaque on purpose: naming a transport's error type here would put that
/// transport's dependency in every build of this crate, including the ones
/// whose transport is a local socket or a test double. An implementation
/// attaches its own error as a source, and a consumer that wants the detail
/// walks [`std::error::Error::source`].
#[derive(Debug)]
pub struct TransportError {
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl TransportError {
    /// A failure with no underlying error to attach.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// A failure carrying the implementation's own error as its cause.
    #[must_use]
    pub fn with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|boxed| boxed.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// Sending one described request and getting back what the server said.
///
/// The single required method is the whole seam. Everything a surface does —
/// resolving an operation, routing values, decoding, paging — happens above it.
pub trait TapesTransport {
    /// Send one described request.
    fn send(
        &self,
        request: &WireRequest<'_>,
    ) -> impl Future<Output = std::result::Result<WireResponse, TransportError>>;
}

/// Sending a request whose response must not be buffered.
///
/// A separate trait, and generic in the body it yields, so that
/// [`TapesTransport`] itself never names a streaming type: an export can be far
/// larger than a session's working set, but a transport carrying frames over a
/// socket has a different notion of "a stream" than an HTTP client does, and
/// forcing one of them into the other's type is how the seam would acquire a
/// dependency it exists to avoid.
///
/// An implementation must surface a non-success status as an error rather than
/// as a readable body, so a caller streaming to a file can never write an error
/// page into it.
pub trait StreamingTransport: TapesTransport {
    /// The streaming body this transport yields.
    type Body;

    /// Send one described request and hand back the live response.
    fn send_stream(&self, request: &WireRequest<'_>) -> impl Future<Output = Result<Self::Body>>;
}

/// The outcome of a conditional fetch of a cassette's OpenAPI document.
#[derive(Debug, Clone)]
pub enum SpecFetch {
    /// The server matched our `If-None-Match` and sent no body; the cached copy
    /// is still current.
    Unchanged,
    /// A document, and the validator to revalidate it with next time.
    Fetched {
        /// The OpenAPI document, verbatim.
        document: Value,
        /// The response `ETag`, when the server sent one.
        etag: Option<String>,
    },
}

/// The narrow seam the cassette surface cache fetches through.
///
/// Retained alongside [`TapesTransport`] for consumers that implement fetching
/// on their own client rather than handing this crate a transport — the shape
/// the cassette machinery shipped with. [`Wire`] adapts any [`TapesTransport`]
/// onto it, so there is one implementation of the cache's revalidation ladder
/// regardless of which seam a consumer plugs in.
///
/// The associated error is only ever displayed: every failure here is
/// survivable for the surface cache, which logs it and degrades. A consumer's
/// own error type therefore plugs in without conversion.
pub trait SpecTransport {
    /// The transport's own error type.
    type Error: fmt::Display;

    /// Fetch the `/v1/cassettes` discovery document, raw.
    fn fetch_discovery(&self) -> impl Future<Output = std::result::Result<Value, Self::Error>>;

    /// Conditionally fetch one cassette's OpenAPI document.
    ///
    /// `path` is the server-relative `openapi_path` discovery published;
    /// `etag` is the validator from a previous fetch.
    fn fetch_spec(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> impl Future<Output = std::result::Result<SpecFetch, Self::Error>>;

    /// Execute one described call and decode the JSON response.
    fn execute(
        &self,
        call: &Call<'_>,
    ) -> impl Future<Output = std::result::Result<Value, Self::Error>>;
}

/// Adapts any [`TapesTransport`] onto the [`SpecTransport`] the cassette cache
/// fetches through.
///
/// This is where the two seams meet, and it is four request descriptions long
/// — which is the point. The cache's freshness rules, its revalidation ladder,
/// and its degradation behaviour are written once against [`SpecTransport`];
/// what changes between a consumer's own client and a transport this crate
/// drives is only how the bytes are fetched.
#[derive(Debug, Clone, Copy)]
pub struct Wire<T>(pub T);

impl<T> Wire<T> {
    /// Wrap a transport.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self(transport)
    }
}

impl<T: TapesTransport> TapesTransport for Wire<T> {
    fn send(
        &self,
        request: &WireRequest<'_>,
    ) -> impl Future<Output = std::result::Result<WireResponse, TransportError>> {
        self.0.send(request)
    }
}

impl<T: TapesTransport> SpecTransport for Wire<T> {
    type Error = crate::error::Error;

    async fn fetch_discovery(&self) -> Result<Value> {
        crate::cassettes::fetch_discovery(&self.0).await
    }

    async fn fetch_spec(&self, path: &str, etag: Option<&str>) -> Result<SpecFetch> {
        crate::cassettes::fetch_spec(&self.0, path, etag).await
    }

    async fn execute(&self, call: &Call<'_>) -> Result<Value> {
        crate::cassettes::invoke(&self.0, call).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A transport that answers everything with one canned document.
    struct Canned;

    impl TapesTransport for Canned {
        async fn send(
            &self,
            request: &WireRequest<'_>,
        ) -> std::result::Result<WireResponse, TransportError> {
            Ok(WireResponse::new(
                200,
                format!("http://tapes.test{}", request.path),
                vec![("etag".to_owned(), "\"sha256:abc\"".to_owned())],
                br#"{"cassettes":[]}"#.to_vec(),
            ))
        }
    }

    #[tokio::test]
    async fn a_transport_drives_the_cassette_seam_through_the_bridge() {
        // The claim this type exists to make good on: the surface cache's
        // revalidation ladder is written once, against `SpecTransport`, and a
        // consumer that plugs in a `TapesTransport` instead reaches the same
        // implementation rather than a parallel one.
        let bridged = Wire::new(Canned);

        let discovery = SpecTransport::fetch_discovery(&bridged).await.unwrap();
        assert_eq!(discovery["cassettes"], serde_json::json!([]));

        let fetched = SpecTransport::fetch_spec(&bridged, "/v1/cassettes/x/openapi.json", None)
            .await
            .unwrap();
        match fetched {
            SpecFetch::Fetched { etag, .. } => {
                assert_eq!(etag.as_deref(), Some("\"sha256:abc\""));
            }
            SpecFetch::Unchanged => panic!("expected a document"),
        }
    }

    #[test]
    fn a_header_is_found_however_the_transport_spelled_it() {
        // A transport carrying hand-built frames has no HTTP library to
        // normalise header names, and a conditional fetch that missed an
        // `ETag` would silently re-download every spec forever.
        let response = WireResponse::new(
            200,
            "http://tapes.test/v1/cassettes".to_owned(),
            vec![("ETag".to_owned(), "\"x\"".to_owned())],
            Vec::new(),
        );
        assert_eq!(response.header("etag"), Some("\"x\""));
        assert_eq!(response.header("ETAG"), Some("\"x\""));
        assert_eq!(response.header("if-none-match"), None);
    }
}
