//! Fetching cassette documents and executing described calls.
//!
//! [`SpecTransport`] is the seam a consumer plugs its own client into: the
//! surface cache asks it for discovery and for each cassette's OpenAPI
//! document, and a generated command's [`Call`] is executed through it.
//! `tapesctl` implements it on its contract-driven `ApiClient`; [`DirectHttp`]
//! is the crate's own no-auth implementation, extracted verbatim from that
//! client's transport half.
//!
//! # No auth
//!
//! The tapes read API carries no authentication of its own. Tenancy is settled
//! by the deployment before a request reaches the process. [`DirectHttp`]
//! sends no credential; a deployment's gateway adds its own on the way
//! through.

use serde_json::Value;
use snafu::ResultExt;
use url::Url;

use crate::error::{Error, Result, error};
use crate::invoke::{Call, call_url};

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

/// What the cassette machinery needs from a transport.
///
/// The associated error is only ever displayed: every failure on this seam is
/// survivable for the surface cache, which logs it and degrades. A consumer's
/// own error type therefore plugs in without conversion.
pub trait SpecTransport {
    /// The transport's own error type.
    type Error: std::fmt::Display;

    /// Fetch the `/v1/cassettes` discovery document, raw.
    fn fetch_discovery(&self) -> impl Future<Output = Result<Value, Self::Error>>;

    /// Conditionally fetch one cassette's OpenAPI document.
    ///
    /// `path` is the server-relative `openapi_path` discovery published;
    /// `etag` is the validator from a previous fetch.
    fn fetch_spec(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> impl Future<Output = Result<SpecFetch, Self::Error>>;

    /// Execute one described call and decode the JSON response.
    fn execute(&self, call: &Call<'_>) -> impl Future<Output = Result<Value, Self::Error>>;
}

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
        // Redirects are refused, not followed: this client speaks to exactly
        // the server the user configured, and both the discovery document and
        // a cassette's own spec are data that must not be able to steer a
        // request — least of all one carrying a user-provided body — onto
        // another host. The tapes API never redirects, so a 3xx here is
        // always either a misconfiguration or an attempt to move the client.
        // There is deliberately NO fallback client: if this build fails
        // (which a redirect policy alone cannot cause in practice), every
        // request errors instead of any default client quietly following
        // redirects.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        Self { http, base }
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
    /// the property visible per-response: any 3xx errors out, and the origin
    /// that answered must be the origin the user configured.
    fn refuse_moved(&self, response: &reqwest::Response) -> Result<()> {
        snafu::ensure!(
            !response.status().is_redirection(),
            error::ContractSnafu {
                detail: "the server answered with a redirect; this client does not follow them",
            }
        );
        snafu::ensure!(
            response.url().origin() == self.base.origin(),
            error::ContractSnafu {
                detail: "the response came from a different origin than the configured server",
            }
        );
        Ok(())
    }

    /// The base URL, for logging.
    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Join an absolute API path onto the base.
    fn url(&self, path: &str) -> Result<Url> {
        self.base.join(path).context(error::UrlSnafu)
    }

    /// `GET /v1/cassettes` — the cassette discovery document, raw.
    ///
    /// Returned raw for the same reason a consumer's reads should be:
    /// discovery grows fields and a partial model would eat them.
    pub async fn fetch_discovery(&self) -> Result<Value> {
        self.execute(&Call {
            method: "GET",
            path: "/v1/cassettes",
            ..Default::default()
        })
        .await
    }

    /// `GET /v1/cassettes/{name}/openapi.json` — one cassette's own document.
    ///
    /// `path` is the `openapi_path` discovery published rather than a path this
    /// client builds, so the server stays free to move the route. It is
    /// required to be server-relative: `Url::join` treats an absolute URL as a
    /// replacement, so a discovery document naming
    /// `http://elsewhere/openapi.json` would otherwise redirect this fetch off
    /// the server the user asked for.
    ///
    /// `etag` is the validator from a previous fetch. The route answers a
    /// matching `If-None-Match` with 304 and an empty body, which is what makes
    /// revalidating a cached surface cheap.
    pub async fn fetch_spec(&self, path: &str, etag: Option<&str>) -> Result<SpecFetch> {
        // Discovery is data, not authority. A single leading slash keeps the
        // request on the server that served discovery; `//host/path` is a
        // protocol-RELATIVE reference that Url::join resolves onto a
        // different host entirely, so it is rejected up front — and the
        // built URL's origin is checked against the base as the backstop for
        // any other authority-changing join.
        if !path.starts_with('/') || path.starts_with("//") {
            return error::SpecPathSnafu {
                path: path.to_owned(),
            }
            .fail();
        }
        let url = self.url(path)?;
        if url.origin() != self.base.origin() {
            return error::SpecPathSnafu {
                path: path.to_owned(),
            }
            .fail();
        }

        let mut request = self.http()?.get(url.clone());
        if let Some(etag) = etag {
            request = request.header(http::header::IF_NONE_MATCH, etag);
        }
        let response = request.send().await.context(error::SendSnafu)?;
        self.refuse_moved(&response)?;

        // The pre-flight guards validate the URL this client BUILT; a 30x
        // from the server can still walk the request elsewhere, and reqwest
        // follows redirects by default. The origin that ultimately answered
        // must be the origin the user configured — a spec served from
        // anywhere else is refused unread. (Nothing sensitive left with the
        // redirected request: this fetch carries no credentials.)
        if response.url().origin() != self.base.origin() {
            return error::SpecPathSnafu {
                path: path.to_owned(),
            }
            .fail();
        }

        if response.status() == http::StatusCode::NOT_MODIFIED {
            return Ok(SpecFetch::Unchanged);
        }
        let etag = response
            .headers()
            .get(http::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let document = decode_json(response, &url).await?;

        Ok(SpecFetch::Fetched { document, etag })
    }

    /// Make one call described by an OpenAPI document and decode the JSON.
    ///
    /// The verb, path and parameter names all come from the document rather
    /// than from anything hand-written here, which is the whole point of a
    /// generated surface.
    pub async fn execute(&self, call: &Call<'_>) -> Result<Value> {
        let (response, url) = self.send_call(call).await?;
        decode_json(response, &url).await
    }

    /// Make one described call and hand back the live response for streaming.
    ///
    /// A non-success status is read and surfaced as [`Error::Status`] here,
    /// so a caller streaming to a file can never write an error page into it.
    pub async fn execute_stream(&self, call: &Call<'_>) -> Result<reqwest::Response> {
        let (response, url) = self.send_call(call).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Status {
                status: status.as_u16(),
                endpoint: url.to_string(),
                body,
            });
        }
        Ok(response)
    }

    /// Send one described call, without reading the response body.
    async fn send_call(&self, call: &Call<'_>) -> Result<(reqwest::Response, Url)> {
        let url = call_url(&self.base, call)?;
        let method = reqwest::Method::from_bytes(call.method.as_bytes()).map_err(|_| {
            error::MethodSnafu {
                method: call.method,
            }
            .build()
        })?;

        let mut request = self.http()?.request(method, url.clone());
        for (name, value) in &call.headers {
            request = request.header(name, value);
        }
        if let Some(body) = &call.body {
            request = request
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
        }

        let response = request.send().await.context(error::SendSnafu)?;
        // Described calls can carry user-provided bodies and headers, so of
        // all requests this client makes these are the ones a redirect must
        // never be able to move.
        self.refuse_moved(&response)?;
        Ok((response, url))
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

/// Decode one response as JSON, surfacing an error body with its status.
pub async fn decode_json(response: reqwest::Response, url: &Url) -> Result<Value> {
    let status = response.status();
    let body = response.bytes().await.context(error::SendSnafu)?;
    if !status.is_success() {
        // Every tapes error body is `{"error": "..."}`; surfacing it beats
        // the bare status, which never names the offending parameter.
        return Err(Error::Status {
            status: status.as_u16(),
            endpoint: url.to_string(),
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    // A successful response with no body is a real answer, not a decode
    // failure: cassette routes are free to return 204.
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).context(error::DecodeSnafu)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn base(raw: &str) -> DirectHttp {
        DirectHttp::new(Url::parse(raw).unwrap())
    }

    #[tokio::test]
    async fn a_spec_path_may_not_change_the_request_authority() {
        // `//host/path` is protocol-relative: it survives a naive
        // leading-slash check while Url::join moves the request onto a
        // different host. Both the prefix guard and the origin backstop must
        // refuse before anything is sent.
        let client = base("http://tapes.local:8081");
        for path in ["//evil.example/spec.json", "relative/spec.json", ""] {
            let err = client.fetch_spec(path, None).await.unwrap_err();
            assert!(
                err.to_string().contains("non-relative OpenAPI path"),
                "{path:?} produced the wrong error: {err}"
            );
        }
    }

    #[tokio::test]
    async fn a_redirected_spec_fetch_may_not_leave_the_configured_origin() {
        // The URL guards validate what this client builds; a 30x can still
        // walk the request onto another host, and reqwest follows it by
        // default. The answering origin is checked after the fact, so the
        // foreign document is refused unread.
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
        // Nothing left the configured origin: the redirect was refused, not
        // followed and then rejected.
        assert!(
            elsewhere
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "the foreign host must never see a request"
        );
    }

    #[tokio::test]
    async fn a_304_surfaces_as_an_error_which_the_cache_treats_as_keep_the_previous_entry() {
        // Pinned pre-extraction behavior, moved verbatim: `refuse_moved` runs
        // before the NOT_MODIFIED check and 304 is a 3xx, so a matched
        // validator errors here rather than reaching `SpecFetch::Unchanged`.
        // The cache's revalidation answers any fetch error by keeping the
        // previous cached document — observably identical to `Unchanged` —
        // which the consumer's end-to-end suite asserts. Reordering the check
        // is a deliberate follow-up, not something this extraction may do.
        use wiremock::matchers::header;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/cassettes/x/openapi.json"))
            .and(header("if-none-match", "\"sha256:abc\""))
            .respond_with(ResponseTemplate::new(304))
            .mount(&server)
            .await;

        let err = base(&server.uri())
            .fetch_spec("/v1/cassettes/x/openapi.json", Some("\"sha256:abc\""))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("redirect"), "got: {err}");
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
}
