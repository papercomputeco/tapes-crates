//! The HTTP engine, and the credential seam consumers plug into it.
//!
//! # What a consumer used to have to write
//!
//! [`crate::transport::TapesTransport`] is a small trait, and that made it look
//! cheap to implement. It was not. Every implementation had to parse the verb,
//! join the contract-relative path onto a base in the right mode, copy the
//! headers, attach a content type when there was a body, decide what a redirect
//! means, split the response into status, headers, and bytes, map three
//! different failures into the crate's taxonomy, and then write the whole thing
//! again for the streaming variant with a different rule about non-success
//! statuses. Every consumer got that right in its own way, and the ways
//! differed: one refused redirects, one did not; one surfaced a stream's 500 as
//! an error, one had not been asked to yet.
//!
//! None of that is a consumer's decision. The only genuine difference between
//! the implementations was **what makes a request authorised**.
//!
//! # The shape this module settles on
//!
//! [`HttpEngine`] owns the HTTP: request building, the redirect refusal, the
//! streaming variant, the error mapping, and the retry loop. [`HttpAuth`] is
//! what a consumer writes instead of a transport, and it is three things:
//!
//! 1. **The client.** Injected at construction with
//!    [`HttpEngine::with_client`], because TLS policy is a property of the
//!    client and not of the credential: a deployment that pins a root but sends
//!    no credential should not have to implement an auth trait to say so, and
//!    one that mints a token but has no TLS opinion should not have to build a
//!    client. Omitted, the engine builds its own no-redirect client.
//! 2. [`HttpAuth::authorize`] — the headers this attempt carries. Called once
//!    per attempt, so a consumer that mints a fresh credential per request
//!    keeps doing exactly that, including on the retry.
//! 3. [`HttpAuth::on_unauthorized`] — what a rejected credential means. The
//!    hook returns a *decision*; the engine owns the loop. That is the
//!    difference between a retry policy that is data and one that is a
//!    reimplemented `while` loop in every consumer, each with its own answer to
//!    "how many times?".
//!
//! [`DirectHttp`] is the trivial instance: [`NoAuth`], no headers, no retry.
//! The name and the `direct-http` feature are unchanged, because a consumer
//! that only wanted an unauthenticated client should not have to learn that a
//! seam appeared underneath it.
//!
//! # No auth is still the default
//!
//! The tapes read API carries no authentication of its own; tenancy is settled
//! by the deployment before a request reaches the process. A consumer that
//! holds a credential does so because *its* edge demands one, which is why the
//! credential is a hook rather than a configuration field on the engine.
//!
//! # Redirects are refused, not followed
//!
//! This engine speaks to exactly the server the caller configured. Both the
//! discovery document and a cassette's own spec are *data*, and data must not
//! be able to steer a request — least of all one carrying a user-provided body
//! or a credential — onto another host. The tapes API never redirects, so a 3xx
//! is always either a misconfiguration or an attempt to move the client.
//!
//! The defence is two-layered on purpose: the engine's own client is built with
//! `Policy::none`, and every response is checked to have come from the
//! configured origin. The second layer is what holds when the client was
//! injected by a consumer whose redirect policy is its own.

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

/// How many attempts one request may cost, however eager a hook is to retry.
///
/// A hook that always answers [`Unauthorized::Retry`] is a bug, but an
/// unbounded loop in a client reads as a hang — and a hang is the hardest
/// failure to attribute to its cause. Same reasoning as the repeated-cursor
/// guard in [`crate::page::walk`].
const MAX_ATTEMPTS: u32 = 4;

/// The status that puts the credential in question.
///
/// Only 401. A 403 is an authorization decision about a credential the server
/// understood, so minting the same credential again answers nothing and turns
/// a refusal into a retry storm.
const UNAUTHORIZED: u16 = 401;

/// One rejected attempt, as the hook sees it.
///
/// Deliberately not the response itself: a streaming request's body is not read
/// before this decision is made, and handing over a half-consumed response
/// would make the decision depend on which of the two paths asked for it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Rejected<'a> {
    /// The status the server answered with — always `401` today. It is carried
    /// rather than assumed so that a future engine which routes another status
    /// through this hook does not have to change the hook's shape.
    pub status: u16,
    /// The URL that was refused, for diagnostics.
    pub endpoint: &'a str,
    /// Which attempt this was, counting from one.
    pub attempt: u32,
}

/// What to do about a rejected credential.
#[derive(Debug)]
#[non_exhaustive]
pub enum Unauthorized {
    /// Send it again. [`HttpAuth::authorize`] runs first, so a hook that mints
    /// per request gets a fresh credential without holding one itself.
    Retry,
    /// Hand the 401 back to the caller as the answer it is.
    Surface,
    /// Fail the call with this error instead of the response.
    ///
    /// For a consumer whose own error type says something the status cannot —
    /// "the refresh token is spent, run the login command" is a different fact
    /// from "401", and the one a user can act on.
    Fail(TransportError),
}

/// What makes a request authorised, for consumers that need it to be.
///
/// The whole trait is two methods, one of them defaulted, because everything
/// else a transport used to do is [`HttpEngine`]'s.
pub trait HttpAuth {
    /// Headers to attach to this attempt.
    ///
    /// Returned rather than applied to the request so the engine stays the only
    /// thing that builds one: a hook that could edit the path or the query
    /// would be a second route to the wire, and this crate has spent its whole
    /// existence removing those.
    fn authorize(
        &self,
        request: &WireRequest<'_>,
        attempt: u32,
    ) -> impl Future<Output = std::result::Result<Vec<(String, String)>, TransportError>>;

    /// What a rejected credential means.
    ///
    /// Defaults to [`Unauthorized::Surface`]: a client with no credential to
    /// refresh has nothing to gain from sending the same request again, and the
    /// 401 is the server's answer.
    fn on_unauthorized(&self, rejected: Rejected<'_>) -> impl Future<Output = Unauthorized> {
        let _ = rejected;
        async { Unauthorized::Surface }
    }
}

/// The credential-free instance: no headers, no retry.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAuth;

impl HttpAuth for NoAuth {
    async fn authorize(
        &self,
        _request: &WireRequest<'_>,
        _attempt: u32,
    ) -> std::result::Result<Vec<(String, String)>, TransportError> {
        Ok(Vec::new())
    }
}

/// An HTTP transport for one tapes deployment, with the credential half left
/// to an [`HttpAuth`].
///
/// `http` is `None` only if the no-redirect client could not be built at all —
/// in which case every request errors, rather than any fallback silently
/// following redirects.
#[derive(Clone)]
pub struct HttpEngine<A> {
    http: Option<reqwest::Client>,
    base: Url,
    mode: PathMode,
    auth: A,
}

impl<A> std::fmt::Debug for HttpEngine<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The hook is a closure over a live credential source in every real
        // implementation and has no useful Debug; the base and the join mode
        // are the fields worth seeing in a trace.
        f.debug_struct("HttpEngine")
            .field("base", &self.base)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// A direct, unauthenticated HTTP transport for one tapes server.
///
/// The engine with [`NoAuth`] in it. Kept as a name of its own because it is
/// the shape most consumers want and the one this crate has always shipped.
pub type DirectHttp = HttpEngine<NoAuth>;

impl HttpEngine<NoAuth> {
    /// Build an unauthenticated transport against `base`.
    #[must_use]
    pub fn new(base: Url) -> Self {
        Self::with_auth(base, NoAuth)
    }
}

impl<A> HttpEngine<A> {
    /// Build a transport against `base`, authorised by `auth`.
    ///
    /// The join mode is [`PathMode::Direct`]; a deployment that mounts tapes
    /// under a gateway prefix adds [`HttpEngine::under_base`].
    #[must_use]
    pub fn with_auth(base: Url, auth: A) -> Self {
        // There is deliberately NO fallback client: if this build fails (which
        // a redirect policy alone cannot cause in practice), every request
        // errors instead of any default client quietly following redirects.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        Self {
            http,
            base,
            mode: PathMode::Direct,
            auth,
        }
    }

    /// Join contract paths *under* the base's own prefix rather than at its
    /// root.
    ///
    /// What a deployment that mounts tapes behind a gateway needs: a
    /// root-absolute join would send `/v1/sessions` to the edge root — a 404 at
    /// best, a wrong-gateway route at worst, and neither looks like a URL bug
    /// at the call site. The base must end in a slash.
    #[must_use]
    pub fn under_base(mut self) -> Self {
        self.mode = PathMode::UnderBase;
        self
    }

    /// Send on the caller's own client rather than the engine's.
    ///
    /// The injection point for a TLS policy, a proxy configuration, or a
    /// connection pool a consumer already holds. The per-response origin check
    /// still applies, so an injected client that follows redirects cannot walk
    /// a request onto another host unnoticed.
    #[must_use]
    pub fn with_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    /// The base URL, for logging.
    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// The credential hook this engine authorises with.
    #[must_use]
    pub fn auth(&self) -> &A {
        &self.auth
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
    /// that answered must be the origin the caller configured. It is the only
    /// defence when the client was injected, and the one that matters most
    /// there, because an authorised request carries a credential.
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
}

impl<A: HttpAuth> HttpEngine<A> {
    /// Build and send one attempt, without reading the response body.
    async fn attempt(
        &self,
        call: &WireRequest<'_>,
        url: &Url,
        attempt: u32,
    ) -> std::result::Result<reqwest::Response, TransportError> {
        // A verb the contract declared that reqwest will not parse is refused,
        // not defaulted. Falling back to GET would turn a mutating operation
        // into a read that quietly succeeds against the wrong route.
        let method = reqwest::Method::from_bytes(call.method.as_bytes())
            .map_err(|_| TransportError::new(format!("unusable HTTP method {:?}", call.method)))?;

        let client = self
            .http()
            .map_err(|error| TransportError::new(error.to_string()))?;
        let mut request = client.request(method, url.clone());
        for (name, value) in &call.headers {
            request = request.header(name, value);
        }
        for (name, value) in self.auth.authorize(call, attempt).await? {
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
        // Described calls can carry user-provided bodies, headers, and a
        // credential, so of all requests this client makes these are the ones a
        // redirect must never be able to move.
        self.refuse_moved(&response)?;
        Ok(response)
    }

    /// Send one described call, running the credential hook's retry policy.
    ///
    /// The loop is here, once, rather than in every consumer that needs one.
    async fn send_call(
        &self,
        call: &WireRequest<'_>,
    ) -> std::result::Result<(reqwest::Response, Url), TransportError> {
        let url = call_url(&self.base, call, self.mode)
            .map_err(|error| TransportError::new(error.to_string()))?;

        let endpoint = url.to_string();
        for attempt in 1..=MAX_ATTEMPTS {
            let response = self.attempt(call, &url, attempt).await?;
            if response.status().as_u16() != UNAUTHORIZED {
                return Ok((response, url));
            }
            // The response is held, unread, while the hook decides: a surfaced
            // 401 must reach the caller with the body that explains it, and a
            // bare status is a fact without a reason.
            let decision = self
                .auth
                .on_unauthorized(Rejected {
                    status: UNAUTHORIZED,
                    endpoint: &endpoint,
                    attempt,
                })
                .await;
            match decision {
                Unauthorized::Retry if attempt < MAX_ATTEMPTS => {
                    // Dropped here so the refused body and its connection are
                    // released before the next attempt goes out.
                    drop(response);
                }
                Unauthorized::Retry => {
                    return Err(TransportError::new(format!(
                        "the credential was refused {MAX_ATTEMPTS} times running",
                    )));
                }
                Unauthorized::Surface => return Ok((response, url)),
                Unauthorized::Fail(error) => return Err(error),
            }
        }
        Err(TransportError::new("no attempt was made"))
    }

    /// `GET /v1/cassettes` — the cassette discovery document, raw.
    ///
    /// # Errors
    ///
    /// Any transport, status, or decode failure; see [`crate::Error`].
    pub async fn fetch_discovery(&self) -> Result<Value> {
        cassettes::fetch_discovery(self).await
    }

    /// `GET /v1/cassettes/{name}/openapi.json` — one cassette's own document.
    ///
    /// `path` is the `openapi_path` discovery published rather than a path this
    /// client builds, so the server stays free to move the route; it is
    /// therefore untrusted, and refused unless it is plainly server-relative.
    ///
    /// # Errors
    ///
    /// Any transport, status, or decode failure; see [`crate::Error`].
    pub async fn fetch_spec(&self, path: &str, etag: Option<&str>) -> Result<SpecFetch> {
        cassettes::fetch_spec(self, path, etag).await
    }

    /// Make one call described by an OpenAPI document and decode the JSON.
    ///
    /// # Errors
    ///
    /// Any transport, status, or decode failure; see [`crate::Error`].
    pub async fn execute(&self, call: &Call<'_>) -> Result<Value> {
        cassettes::invoke(self, call).await
    }

    /// Make one described call and hand back the live response for streaming.
    ///
    /// A non-success status is read and surfaced as [`Error::ApiStatus`] here,
    /// so a caller streaming to a file can never write an error page into it.
    ///
    /// # Errors
    ///
    /// [`Error::ApiStatus`] for any non-success answer, or the transport's own
    /// failure.
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

impl<A: HttpAuth> TapesTransport for HttpEngine<A> {
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
        // The status travels rather than being judged here: a non-success body
        // is what names the offending parameter, and `decode` above this layer
        // is the one place that turns it into an error.
        Ok(WireResponse::new(status, url.to_string(), headers, body))
    }
}

impl<A: HttpAuth> StreamingTransport for HttpEngine<A> {
    type Body = reqwest::Response;

    async fn send_stream(&self, request: &WireRequest<'_>) -> Result<Self::Body> {
        self.execute_stream(request).await
    }
}

impl<A: HttpAuth> SpecTransport for HttpEngine<A> {
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn base(raw: &str) -> DirectHttp {
        DirectHttp::new(Url::parse(raw).unwrap())
    }

    /// A hook shaped like the one a credentialled consumer writes: mint a token
    /// per attempt, retry a 401 once, then say something the status cannot.
    struct Minting {
        mints: Arc<AtomicU32>,
        retries: u32,
    }

    impl HttpAuth for Minting {
        async fn authorize(
            &self,
            _request: &WireRequest<'_>,
            attempt: u32,
        ) -> std::result::Result<Vec<(String, String)>, TransportError> {
            self.mints.fetch_add(1, Ordering::SeqCst);
            Ok(vec![(
                "x-tapes-auth".to_owned(),
                format!("Bearer token-{attempt}"),
            )])
        }

        async fn on_unauthorized(&self, rejected: Rejected<'_>) -> Unauthorized {
            if rejected.attempt <= self.retries {
                Unauthorized::Retry
            } else {
                Unauthorized::Fail(TransportError::new(
                    "not authenticated; run the login command",
                ))
            }
        }
    }

    /// A hook that never accepts a refusal, so the engine's own cap is the
    /// only thing that ends the call.
    struct Forever;

    impl HttpAuth for Forever {
        async fn authorize(
            &self,
            _request: &WireRequest<'_>,
            _attempt: u32,
        ) -> std::result::Result<Vec<(String, String)>, TransportError> {
            Ok(Vec::new())
        }

        async fn on_unauthorized(&self, _rejected: Rejected<'_>) -> Unauthorized {
            Unauthorized::Retry
        }
    }

    fn minting(server: &MockServer, retries: u32) -> (HttpEngine<Minting>, Arc<AtomicU32>) {
        let mints = Arc::new(AtomicU32::new(0));
        let engine = HttpEngine::with_auth(
            Url::parse(&server.uri()).unwrap(),
            Minting {
                mints: Arc::clone(&mints),
                retries,
            },
        );
        (engine, mints)
    }

    #[tokio::test]
    async fn a_hooks_headers_ride_the_request_the_engine_built() {
        // The division of labour, in one assertion: the engine resolved the
        // path and sent it, the hook only said what makes it authorised.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions"))
            .and(header("x-tapes-auth", "Bearer token-1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let (engine, mints) = minting(&server, 1);
        let response = engine
            .send(&WireRequest {
                method: "GET",
                path: "/v1/sessions",
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(mints.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_401_is_retried_once_with_a_freshly_authorised_request() {
        // The policy is data — the hook said "retry" — and the loop that acts
        // on it is the engine's, so no consumer writes one again.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let (engine, mints) = minting(&server, 1);
        let response = engine
            .send(&WireRequest {
                method: "GET",
                path: "/v1/sessions",
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            mints.load(Ordering::SeqCst),
            2,
            "the retry was not authorised again"
        );
    }

    #[tokio::test]
    async fn an_exhausted_retry_fails_with_the_hooks_own_words() {
        // "run the login command" is a fact a user can act on; "401" is not.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let (engine, _) = minting(&server, 1);
        let err = engine
            .send(&WireRequest {
                method: "GET",
                path: "/v1/sessions",
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("login"), "got: {err}");
    }

    #[tokio::test]
    async fn a_hook_that_never_gives_up_is_stopped_by_the_engine() {
        // An unbounded loop in a client reads as a hang, which is the hardest
        // failure to attribute. The cap is the engine's, not the hook's.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let engine = HttpEngine::with_auth(Url::parse(&server.uri()).unwrap(), Forever);
        let err = engine
            .send(&WireRequest {
                method: "GET",
                path: "/v1/sessions",
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("times running"), "got: {err}");
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            MAX_ATTEMPTS as usize,
        );
    }

    #[tokio::test]
    async fn a_surfaced_401_reaches_the_caller_with_the_body_that_explains_it() {
        // The default hook's answer. A bare status is a fact without a reason;
        // the body is where the server names what was wrong.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"no tenant"}"#))
            .mount(&server)
            .await;

        let response = base(&server.uri())
            .send(&WireRequest {
                method: "GET",
                path: "/v1/sessions",
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.status, 401);
        assert_eq!(response.body, br#"{"error":"no tenant"}"#.to_vec());
    }

    #[tokio::test]
    async fn an_under_base_join_lands_beneath_the_gateway_prefix() {
        // The other half of what a gateway-fronted deployment needs, and the
        // reason the mode is a builder rather than a second engine.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/primary/tapes/v1/sessions/s-1/traces"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let engine =
            DirectHttp::new(Url::parse(&format!("{}/primary/tapes/", server.uri())).unwrap())
                .under_base();
        let response = engine
            .send(&WireRequest {
                method: "GET",
                path: "/v1/sessions/{id}/traces",
                path_params: vec![("id".to_owned(), "s-1".to_owned())],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.status, 200);
    }

    #[tokio::test]
    async fn an_injected_client_is_the_one_that_sends() {
        // The TLS/proxy injection point. Proved by giving the engine a client
        // that cannot reach anything but the mock, and seeing the request land.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/cassettes"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"cassettes":[]}"#))
            .mount(&server)
            .await;

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let engine = DirectHttp::new(Url::parse(&server.uri()).unwrap()).with_client(client);
        assert_eq!(
            engine.fetch_discovery().await.unwrap()["cassettes"],
            serde_json::json!([]),
        );
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
    async fn a_stream_refuses_a_non_success_status() {
        // An export writes its body to a file. If a 500's error page were
        // handed back as a readable stream, that page would be the file.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/sessions/s-1/export"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let err = base(&server.uri())
            .execute_stream(&WireRequest {
                method: "GET",
                path: "/v1/sessions/{id}/export",
                path_params: vec![("id".to_owned(), "s-1".to_owned())],
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ApiStatus { status: 500, .. }),
            "got {err:?}",
        );
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
        let got: Value = client.call("listSessions", Vec::new()).await.unwrap();
        assert_eq!(got["items"][0]["id"], "s1");
    }
}
