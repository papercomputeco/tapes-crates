//! A direct, unauthenticated HTTP transport.
//!
//! The in-crate [`TapesTransport`] implementation, behind the `direct-http`
//! feature: one tapes server, addressed by URL, with no credential.
//!
//! # No auth
//!
//! The tapes read API carries no authentication of its own. Tenancy is settled
//! by the deployment before a request reaches the process. [`DirectHttp`] sends
//! no credential; a deployment's gateway adds its own on the way through. A
//! consumer that *does* hold a credential implements the seam itself rather
//! than configuring this one — which is why authentication, retry, and TLS
//! policy are absent here instead of switched off.
//!
//! # Redirects are refused, not followed
//!
//! This client speaks to exactly the server the user configured. Both the
//! discovery document and a cassette's own spec are *data*, and data must not
//! be able to steer a request — least of all one carrying a user-provided body
//! — onto another host. The tapes API never redirects, so a 3xx is always
//! either a misconfiguration or an attempt to move the client.
//!
//! The defence is two-layered on purpose: the client is built with
//! `Policy::none`, and every response is checked to have come from the
//! configured origin. The second layer is what catches a redirect that some
//! future client-builder change quietly re-enables.

use serde_json::Value;
use snafu::ResultExt;
use url::Url;

use crate::cassettes;
use crate::error::{Error, Result, error};
use crate::path::{PathMode, call_url};
use crate::transport::{
    Call, SpecFetch, SpecTransport, StreamingTransport, TapesTransport, TransportError,
    WireRequest, WireResponse,
};

/// A direct, unauthenticated HTTP transport for one tapes server.
///
/// `http` is `None` only if the no-redirect client could not be built at all —
/// in which case every request errors, rather than any fallback silently
/// following redirects.
#[derive(Debug, Clone)]
pub struct DirectHttp {
    http: Option<reqwest::Client>,
    base: Url,
}

impl DirectHttp {
    /// Build a transport against `base`.
    #[must_use]
    pub fn new(base: Url) -> Self {
        // There is deliberately NO fallback client: if this build fails (which
        // a redirect policy alone cannot cause in practice), every request
        // errors instead of any default client quietly following redirects.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        Self { http, base }
    }

    /// The base URL, for logging.
    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// The one HTTP client, or a hard error — never a redirect-following one.
    fn http(&self) -> Result<&reqwest::Client> {
        self.http
            .as_ref()
            .ok_or_else(|| error::ClientInitSnafu.build())
    }

    /// Refuse a response that is a redirect or that a redirect produced.
    ///
    /// The primary defence is the client's `Policy::none`; this backstop makes
    /// the property visible per-response: any 3xx is refused, and the origin
    /// that answered must be the origin the user configured. Nothing sensitive
    /// leaves with a redirected request — this client carries no credentials —
    /// but a foreign document is refused unread regardless.
    ///
    /// `304 Not Modified` is exempt. It is numerically a 3xx and semantically
    /// the opposite of one: it carries no `Location`, moves nothing, and is the
    /// successful answer to the conditional spec fetch the surface cache is
    /// built around. Refusing it — which this client used to, because the
    /// status check came first — made `SpecFetch::Unchanged` unreachable and
    /// left the two surfaces answering the same server differently.
    fn refuse_moved(
        &self,
        response: &reqwest::Response,
    ) -> std::result::Result<(), TransportError> {
        if response.status().is_redirection()
            && response.status() != reqwest::StatusCode::NOT_MODIFIED
        {
            return Err(TransportError::new(
                "the server answered with a redirect; this client does not follow them",
            ));
        }
        if response.url().origin() != self.base.origin() {
            return Err(TransportError::new(
                "the response came from a different origin than the configured server",
            ));
        }
        Ok(())
    }

    /// Send one described call, without reading the response body.
    async fn send_call(
        &self,
        call: &WireRequest<'_>,
    ) -> std::result::Result<(reqwest::Response, Url), TransportError> {
        let url = call_url(&self.base, call, PathMode::Direct)
            .map_err(|error| TransportError::new(error.to_string()))?;
        let method = reqwest::Method::from_bytes(call.method.as_bytes())
            .map_err(|_| TransportError::new(format!("unusable HTTP method {:?}", call.method)))?;

        let client = self
            .http()
            .map_err(|error| TransportError::new(error.to_string()))?;
        let mut request = client.request(method, url.clone());
        for (name, value) in &call.headers {
            request = request.header(name, value);
        }
        if let Some(body) = &call.body {
            request = request
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
        }

        let response = request.send().await.map_err(|source| {
            TransportError::with_source("could not reach the tapes API", source)
        })?;
        // Described calls can carry user-provided bodies and headers, so of
        // all requests this client makes these are the ones a redirect must
        // never be able to move.
        self.refuse_moved(&response)?;
        Ok((response, url))
    }

    /// `GET /v1/cassettes` — the cassette discovery document, raw.
    pub async fn fetch_discovery(&self) -> Result<Value> {
        cassettes::fetch_discovery(self).await
    }

    /// `GET /v1/cassettes/{name}/openapi.json` — one cassette's own document.
    ///
    /// `path` is the `openapi_path` discovery published rather than a path this
    /// client builds, so the server stays free to move the route; it is
    /// therefore untrusted, and refused unless it is plainly server-relative.
    pub async fn fetch_spec(&self, path: &str, etag: Option<&str>) -> Result<SpecFetch> {
        cassettes::fetch_spec(self, path, etag).await
    }

    /// Make one call described by an OpenAPI document and decode the JSON.
    pub async fn execute(&self, call: &Call<'_>) -> Result<Value> {
        cassettes::invoke(self, call).await
    }

    /// Make one described call and hand back the live response for streaming.
    ///
    /// A non-success status is read and surfaced as [`Error::ApiStatus`] here,
    /// so a caller streaming to a file can never write an error page into it.
    pub async fn execute_stream(&self, call: &Call<'_>) -> Result<reqwest::Response> {
        let (response, url) = self.send_call(call).await.context(error::TransportSnafu)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::ApiStatus {
                status: status.as_u16(),
                endpoint: url.to_string(),
                body,
            });
        }
        Ok(response)
    }
}

impl TapesTransport for DirectHttp {
    async fn send(
        &self,
        request: &WireRequest<'_>,
    ) -> std::result::Result<WireResponse, TransportError> {
        let (response, url) = self.send_call(request).await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let body = response
            .bytes()
            .await
            .map_err(|source| TransportError::with_source("could not read the response", source))?
            .to_vec();
        Ok(WireResponse::new(status, url.to_string(), headers, body))
    }
}

impl StreamingTransport for DirectHttp {
    type Body = reqwest::Response;

    async fn send_stream(&self, request: &WireRequest<'_>) -> Result<Self::Body> {
        self.execute_stream(request).await
    }
}

impl SpecTransport for DirectHttp {
    type Error = Error;

    async fn fetch_discovery(&self) -> Result<Value> {
        Self::fetch_discovery(self).await
    }

    async fn fetch_spec(&self, path: &str, etag: Option<&str>) -> Result<SpecFetch> {
        Self::fetch_spec(self, path, etag).await
    }

    async fn execute(&self, call: &Call<'_>) -> Result<Value> {
        Self::execute(self, call).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn base(raw: &str) -> DirectHttp {
        DirectHttp::new(Url::parse(raw).unwrap())
    }

    #[tokio::test]
    async fn a_spec_path_may_not_change_the_request_authority() {
        // `//host/path` is protocol-relative: it survives a naive
        // leading-slash check while a join moves the request onto a different
        // host. Refused before anything is sent.
        let client = base("http://tapes.local:8081");
        for path in ["//evil.example/spec.json", "relative/spec.json", ""] {
            let err = client.fetch_spec(path, None).await.unwrap_err();
            assert!(
                err.to_string().contains("non-relative OpenAPI path"),
                "{path:?} produced the wrong error: {err}",
            );
        }
    }

    #[tokio::test]
    async fn a_redirected_spec_fetch_may_not_leave_the_configured_origin() {
        // The URL guards validate what this client builds; a 30x can still try
        // to walk the request onto another host. The redirect is refused
        // outright, so the foreign host never sees a request at all.
        let elsewhere = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/spec.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "openapi": "3.1.0"
            })))
            .mount(&elsewhere)
            .await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/cassettes/x/openapi.json"))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "location",
                format!("{}/spec.json", elsewhere.uri()).as_str(),
            ))
            .mount(&server)
            .await;

        let client = base(&server.uri());
        let err = client
            .fetch_spec("/v1/cassettes/x/openapi.json", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("redirect"), "wrong error: {err}");
        assert!(
            elsewhere
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "the foreign host must never see a request",
        );
    }

    #[tokio::test]
    async fn a_matched_validator_reads_as_unchanged_over_real_http() {
        // The behaviour this crate unified: a 304 is a successful conditional
        // answer, not a redirect to refuse. Pinned here as well as at the
        // surface-agnostic layer, because the redirect backstop lives in this
        // transport and is the thing that used to swallow it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/cassettes/x/openapi.json"))
            .and(header("if-none-match", "\"sha256:abc\""))
            .respond_with(ResponseTemplate::new(304))
            .mount(&server)
            .await;

        let got = base(&server.uri())
            .fetch_spec("/v1/cassettes/x/openapi.json", Some("\"sha256:abc\""))
            .await
            .unwrap();
        assert!(matches!(got, SpecFetch::Unchanged), "got: {got:?}");
    }

    #[tokio::test]
    async fn an_error_body_is_surfaced_with_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/cassettes"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid cursor"}"#),
            )
            .mount(&server)
            .await;

        let err = base(&server.uri()).fetch_discovery().await.unwrap_err();
        let rendered = format!("{err}");
        assert!(rendered.contains("400"), "got: {rendered}");
        assert!(rendered.contains("invalid cursor"), "got: {rendered}");
    }

    #[tokio::test]
    async fn a_successful_empty_body_is_null_not_a_decode_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/cassettes"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let got = base(&server.uri()).fetch_discovery().await.unwrap();
        assert_eq!(got, Value::Null);
    }

    #[tokio::test]
    async fn the_sealed_surface_rides_the_same_transport() {
        // The property the whole crate exists for: a sealed-contract call and
        // a cassette call are the same request through the same pipeline.
        use crate::core::CoreClient;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"items":[{"id":"s1"}]}"#))
            .mount(&server)
            .await;

        let client = CoreClient::new(base(&server.uri()));
        let got: Value = client.list_sessions(Vec::new()).await.unwrap();
        assert_eq!(got["items"][0]["id"], "s1");
    }
}
