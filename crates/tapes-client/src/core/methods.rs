//! Calling the sealed contract's operations over a transport.
//!
//! Every hand-written URL builder a client used to carry is one line here: the
//! verb, the path template, and the parameter routing all come from
//! `contracts/tapes-api.yaml`. A parameter the contract does not declare is
//! refused before anything is sent, because a server that ignores an unknown
//! query parameter would otherwise hide the drift a vendored contract exists to
//! catch.
//!
//! # The typed surface is the default
//!
//! The named methods return the models in [`crate::core::models`], because the
//! shape of a sealed response is not a consumer's opinion — it is published,
//! vendored here, and held to the document by a build-time gate. A client that
//! modelled it privately was keeping a second copy of a shared fact.
//!
//! The generic seam is still here, one layer down: [`CoreClient::call`] is
//! generic in its response type and reaches every operation by `operationId`,
//! including the ones no method below names. That is the **escape hatch**, and
//! it is the right tool in two places — an operation this crate has not typed
//! yet, and the fidelity reads where a typed decode would quietly truncate an
//! archive of a newer server's data. It is not the default, and a call site
//! that reaches for it should be able to say which of those two it is.
//!
//! The named methods remain conveniences over [`CoreClient::call`] and nothing
//! more: the same operation table, the same routing, the same refusals. Anything
//! else would be a second contract that can disagree with the first.

use std::sync::OnceLock;

use serde::Serialize;
use serde::de::DeserializeOwned;
use snafu::ResultExt;

use crate::cassettes::discovery::Discovery;
use crate::core::contract::{self, core, ops};
use crate::core::models::params::ContractParams;
use crate::core::models::{
    RawTurnHeaderItem, RawTurnListParams, RawTurnListResponse, SeedDemoRequest, SeedResult,
    SessionDetailResponse, SessionItem, SessionListParams, SessionListResponse,
    SessionTracesParams, SessionTracesResponse, SessionUpdateRequest, SpanItem,
    StandaloneTraceDetail, StatsParams, StatsResponse, TraceListParams, TraceListResponse,
    TraceParams,
};
use crate::decode;
use crate::error::{Result, error};
use crate::page;
use crate::transport::{StreamingTransport, TapesTransport, WireRequest};

/// The sealed read surface, bound to one transport.
#[derive(Debug, Clone, Copy)]
pub struct CoreClient<T> {
    transport: T,
}

impl<T> CoreClient<T> {
    /// Bind the sealed surface to a transport.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// The transport this surface calls through.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Take the transport back.
    #[must_use]
    pub fn into_transport(self) -> T {
        self.transport
    }
}

impl<T: TapesTransport> CoreClient<T> {
    /// Resolve one operation in the sealed contract and call it, decoding into
    /// a type the caller names.
    ///
    /// The escape hatch — see the module docs. Equivalent to
    /// [`CoreClient::call_with_body`] with no body, which is what every read
    /// operation wants. An operation whose `requestBody` the contract marks
    /// required is refused rather than sent without one.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn call<R: DeserializeOwned>(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
    ) -> Result<R> {
        self.call_with_body(operation_id, values, None).await
    }

    /// Resolve one operation and call it with a request body.
    ///
    /// The body travels the same route as every other value: the contract
    /// decides whether the operation accepts one, requires one, or takes none,
    /// and a disagreement in either direction is refused before anything is
    /// sent. Without this the capability would exist one layer down and be
    /// unreachable from the facade callers actually use — which is exactly
    /// where a payload goes missing quietly.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn call_with_body<R: DeserializeOwned>(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
        body: Option<String>,
    ) -> Result<R> {
        self.call_shaped(operation_id, values, &[], body).await
    }

    /// Resolve one operation and call it with claimed filter params appended
    /// to the query.
    ///
    /// The claimed-param variant of [`CoreClient::call`], for a consumer that
    /// decodes into its own type — a CLI passing the server's document
    /// through verbatim, say. The typed sessions listing routes
    /// [`SessionListParams::claimed`](crate::core::models::SessionListParams)
    /// through the identical path, so the two spellings cannot drift.
    ///
    /// The pairs travel exactly as given: appended to the query after the
    /// declared parameters, repeats and order preserved, names as data. See
    /// [`CoreClient::call_shaped`] for why they bypass the declared-parameter
    /// refusal without loosening it.
    ///
    /// The channel exists only where the sealed contract documents the claim
    /// extension ([`ops::CLAIM_BEARING_OPS`]): a non-empty `claimed` set on
    /// any other operation is refused before anything is sent, and an empty
    /// set is equivalent to [`CoreClient::call`] everywhere.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn call_with_claimed<R: DeserializeOwned>(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
        claimed: &[(String, String)],
    ) -> Result<R> {
        self.call_shaped(operation_id, values, claimed, None).await
    }

    /// The one request-building path behind every facade above.
    ///
    /// Declared `values` go through the contract check exactly as they always
    /// have. `claimed` pairs are appended to the query *after* that check, in
    /// the caller's order, because a claimed filter param is the one kind of
    /// parameter the vendored document cannot declare: a cassette claims it
    /// on the live server at admission time, and its semantics are entirely
    /// server-side and claim-gated. The names are data — an unclaimed name is
    /// ignored byte-identically by the server, and validating, normalizing,
    /// or dropping one here would silently replace that contract with this
    /// build's guess at it.
    ///
    /// Appending after `call_for` rather than inside it keeps the refusal
    /// honest: a *declared* name still cannot be misspelled into the claimed
    /// channel at a typed call site, because the typed params route through
    /// `values()`, and the untyped route was always the caller's to spell.
    ///
    /// The bypass is scoped, not general: a non-empty `claimed` set is
    /// refused up front unless the operation is in
    /// [`ops::CLAIM_BEARING_OPS`], because the sealed document names which
    /// surfaces carry the claim extension, and an unknown parameter on any
    /// other operation is exactly the drift the declared-parameter refusal
    /// exists to catch.
    async fn call_shaped<R: DeserializeOwned>(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
        claimed: &[(String, String)],
        body: Option<String>,
    ) -> Result<R> {
        if !claimed.is_empty() && !ops::CLAIM_BEARING_OPS.contains(&operation_id) {
            return error::ContractClaimsSnafu {
                operation: operation_id,
            }
            .fail();
        }
        let method = core()?.method(operation_id)?;
        let mut request = contract::call_for_with_body(method, values, body)?;
        request.query.extend(claimed.iter().cloned());
        let response = self
            .transport
            .send(&request)
            .await
            .context(error::TransportSnafu)?;
        decode::json_typed(&response)
    }

    /// Build the request for one operation without sending it.
    ///
    /// For a caller that needs to inspect or decorate a request — a page walk
    /// setting cursors, a test asserting a URL — without a second route to the
    /// wire that could route values differently.
    ///
    /// # Errors
    ///
    /// Any contract failure; see [`crate::Error`].
    pub fn request_for(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
    ) -> Result<WireRequest<'static>> {
        contract::call_for(core()?.method(operation_id)?, values)
    }

    /// Call an operation with a typed parameter set.
    async fn with_params<P: ContractParams, R: DeserializeOwned>(&self, params: &P) -> Result<R> {
        self.call(P::OPERATION, params.values()).await
    }

    /// Call an operation with a typed parameter set and a path value.
    async fn with_params_at<P: ContractParams, R: DeserializeOwned>(
        &self,
        params: &P,
        path: Vec<(&str, String)>,
    ) -> Result<R> {
        let mut values: Vec<(&str, String)> = params.values();
        values.extend(path);
        self.call(P::OPERATION, values).await
    }

    /// Call an operation with a typed request body.
    async fn with_body<B: Serialize, R: DeserializeOwned>(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
        body: &B,
    ) -> Result<R> {
        let rendered = serde_json::to_string(body).context(error::RenderBodySnafu)?;
        self.call_with_body(operation_id, values, Some(rendered))
            .await
    }

    /// `GET /v1/sessions` — one page of the sessions listing.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_sessions(&self, params: &SessionListParams) -> Result<SessionListResponse> {
        self.call_shaped(ops::LIST_SESSIONS, params.values(), &params.claimed, None)
            .await
    }

    /// Every session the listing matches, following `next_cursor` to the end.
    ///
    /// The cursor convention is [`crate::page`]'s, so this walk and a cassette
    /// listing's stop on the same three spellings of "no more pages" and share
    /// the guard against a server that repeats a cursor.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_all_sessions(&self, params: &SessionListParams) -> Result<Vec<SessionItem>> {
        page::walk(|cursor| {
            let mut params = params.clone();
            params.cursor = cursor;
            async move { Ok(self.list_sessions(&params).await?.into_page()) }
        })
        .await
    }

    /// `GET /v1/sessions/{id}` — one session record.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_session(&self, id: &str) -> Result<SessionDetailResponse> {
        self.call(ops::GET_SESSION, vec![("id", id.to_owned())])
            .await
    }

    /// `PATCH /v1/sessions/{id}` — rename a session.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn update_session(
        &self,
        id: &str,
        body: &SessionUpdateRequest,
    ) -> Result<SessionDetailResponse> {
        self.with_body(ops::UPDATE_SESSION, vec![("id", id.to_owned())], body)
            .await
    }

    /// `DELETE /v1/sessions/{id}` — delete a session and its subtree.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn delete_session(&self, id: &str) -> Result<()> {
        self.call(ops::DELETE_SESSION, vec![("id", id.to_owned())])
            .await
    }

    /// `GET /v1/sessions/{id}/traces` — one page of the derived span read
    /// model.
    ///
    /// The page is the first `limit` traces in turn order, closed early on
    /// its byte budget; `session` and `links` are whole on every page. A page
    /// shorter than `limit` is not the end — an empty `next_cursor` is. For
    /// the whole session in one envelope, see
    /// [`CoreClient::get_whole_session_traces`].
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_session_traces(
        &self,
        id: &str,
        params: &SessionTracesParams,
    ) -> Result<SessionTracesResponse> {
        self.with_params_at(params, vec![("id", id.to_owned())])
            .await
    }

    /// The whole composite view of one session, following `next_cursor` to
    /// the end.
    ///
    /// The first page's envelope — `session`, `links`, `schema` — with every
    /// page's `traces` appended in order and `next_cursor` cleared: what the
    /// unpaged response used to be. The cursor convention is
    /// [`crate::page`]'s, so this walk stops on the same three spellings of
    /// "no more pages" and shares the guard against a server that repeats a
    /// cursor. `params.cursor` is the walk's to set; a value passed in is
    /// replaced.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_whole_session_traces(
        &self,
        id: &str,
        params: &SessionTracesParams,
    ) -> Result<SessionTracesResponse> {
        // The first page's envelope, kept while `page::walk` owns the loop.
        // A `OnceLock` rather than a `RefCell` so the future stays `Send`
        // for whatever transport the caller bound; `set` on a later page is
        // a no-op by design, because the envelope repeats on every page and
        // the first one is as good as any.
        let envelope: OnceLock<SessionTracesResponse> = OnceLock::new();
        let traces = page::walk(|cursor| {
            let mut params = params.clone();
            params.cursor = cursor;
            let envelope = &envelope;
            async move {
                let mut response = self.get_session_traces(id, &params).await?;
                let page = response.take_page();
                let _ = envelope.set(response);
                Ok(page)
            }
        })
        .await?;
        // The walk fetched at least one page, so the envelope is set; the
        // default is unreachable and exists only to avoid an unwrap.
        let mut whole = envelope.into_inner().unwrap_or_default();
        whole.traces = traces;
        Ok(whole)
    }

    /// `GET /v1/sessions/{id}/raw_turns` — one page of the wire log behind a
    /// derivation.
    ///
    /// The page is the next `limit` headers in raw turn id order; an empty
    /// `next_cursor` is the end. For every header, see
    /// [`CoreClient::list_all_raw_turns`].
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_raw_turns(
        &self,
        id: &str,
        params: &RawTurnListParams,
    ) -> Result<RawTurnListResponse> {
        self.with_params_at(params, vec![("id", id.to_owned())])
            .await
    }

    /// Every raw turn header of one session, following `next_cursor` to the
    /// end.
    ///
    /// The cursor convention is [`crate::page`]'s, so this walk and every
    /// other listing's stop on the same three spellings of "no more pages"
    /// and share the guard against a server that repeats a cursor.
    /// `params.cursor` is the walk's to set; a value passed in is replaced.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_all_raw_turns(
        &self,
        id: &str,
        params: &RawTurnListParams,
    ) -> Result<Vec<RawTurnHeaderItem>> {
        page::walk(|cursor| {
            let mut params = params.clone();
            params.cursor = cursor;
            async move { Ok(self.list_raw_turns(id, &params).await?.into_page()) }
        })
        .await
    }

    /// `GET /v1/traces` — the trace summaries for one session.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_traces(&self, params: &TraceListParams) -> Result<TraceListResponse> {
        self.with_params(params).await
    }

    /// `GET /v1/traces/{trace_id}` — one page of a trace's spans, with its
    /// header, links, and owning session.
    ///
    /// Only `spans` is paged; `trace`, `links`, `schema`, and `session_id`
    /// are whole on every page. A page shorter than `limit` is not the end —
    /// an empty `next_cursor` is. For the whole trace in one envelope, see
    /// [`CoreClient::get_whole_trace`].
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_trace(
        &self,
        trace_id: &str,
        params: &TraceParams,
    ) -> Result<StandaloneTraceDetail> {
        self.with_params_at(params, vec![("trace_id", trace_id.to_owned())])
            .await
    }

    /// One whole trace, following `next_cursor` to its last span.
    ///
    /// The first page's envelope — `trace`, `links`, `schema`, `session_id` —
    /// with every page's `spans` appended in order and `next_cursor` cleared:
    /// what the unpaged response used to be. The cursor convention is
    /// [`crate::page`]'s, so this walk stops on the same three spellings of
    /// "no more pages" and shares the guard against a server that repeats a
    /// cursor. `params.cursor` is the walk's to set; a value passed in is
    /// replaced.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_whole_trace(
        &self,
        trace_id: &str,
        params: &TraceParams,
    ) -> Result<StandaloneTraceDetail> {
        // See `get_whole_session_traces` for why this is a `OnceLock`.
        let envelope: OnceLock<StandaloneTraceDetail> = OnceLock::new();
        let spans = page::walk(|cursor| {
            let mut params = params.clone();
            params.cursor = cursor;
            let envelope = &envelope;
            async move {
                let mut response = self.get_trace(trace_id, &params).await?;
                let page = response.take_page();
                let _ = envelope.set(response);
                Ok(page)
            }
        })
        .await?;
        let mut whole = envelope.into_inner().unwrap_or_default();
        whole.spans = spans;
        Ok(whole)
    }

    /// `GET /v1/traces/{trace_id}/spans/{span_id}` — one span, in full.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_span(&self, trace_id: &str, span_id: &str) -> Result<SpanItem> {
        self.call(
            ops::GET_SPAN,
            vec![
                ("trace_id", trace_id.to_owned()),
                ("span_id", span_id.to_owned()),
            ],
        )
        .await
    }

    /// `GET /v1/stats` — the aggregate rollups.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_stats(&self, params: &StatsParams) -> Result<StatsResponse> {
        self.with_params(params).await
    }

    /// `GET /v1/cassettes` — what this deployment serves.
    ///
    /// Decodes into the cassette surface's own model rather than a second copy
    /// of it: [`crate::cassettes::discovery`] reads the fields the generated
    /// command surface acts on, and modelling the document twice is the
    /// duplication this crate exists to end.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_cassettes(&self) -> Result<Discovery> {
        self.call(ops::LIST_CASSETTES, Vec::new()).await
    }

    /// `POST /v1/admin/seed/demo` — replay the demo corpora.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn seed_demo(&self, body: &SeedDemoRequest) -> Result<SeedResult> {
        self.with_body(ops::SEED_DEMO, Vec::new(), body).await
    }
}

impl<T: StreamingTransport> CoreClient<T> {
    /// Resolve one operation and stream its response.
    ///
    /// Bodyless, and deliberately so: nothing in this contract both streams a
    /// response and takes a request body. That is an observation about the
    /// document rather than a rule, so it is not enforced here — an operation
    /// that did take a required body would be refused with the same loud error
    /// as anywhere else, which is a signal to add the body-bearing sibling
    /// rather than a payload going missing.
    ///
    /// # Errors
    ///
    /// Any contract or transport failure; see [`crate::Error`].
    pub async fn stream(&self, operation_id: &str, values: Vec<(&str, String)>) -> Result<T::Body> {
        let method = core()?.method(operation_id)?;
        let request = contract::call_for(method, values)?;
        self.transport.send_stream(&request).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::core::models::params::PayloadDetail;
    use crate::path::{PathMode, call_url};
    use crate::transport::{TransportError, WireResponse};
    use serde::Deserialize;
    use serde_json::Value;
    use std::cell::RefCell;
    use url::Url;

    /// A transport that records what it was asked to send and answers with a
    /// canned response — enough to prove the contract layer routed the values,
    /// without a socket.
    ///
    /// It records the request body as well as the URL, because "the payload
    /// arrived at the transport" is the only place a facade that dropped it
    /// would be visible: every layer above still looks correct.
    struct Recorder {
        base: Url,
        responses: RefCell<Vec<Value>>,
        seen: RefCell<Vec<String>>,
        bodies: RefCell<Vec<Option<String>>>,
    }

    impl Recorder {
        fn new(base: &str, responses: Vec<Value>) -> Self {
            Self {
                base: Url::parse(base).unwrap(),
                responses: RefCell::new(responses),
                seen: RefCell::new(Vec::new()),
                bodies: RefCell::new(Vec::new()),
            }
        }
    }

    impl TapesTransport for Recorder {
        async fn send(
            &self,
            request: &WireRequest<'_>,
        ) -> std::result::Result<WireResponse, TransportError> {
            let url = call_url(&self.base, request, PathMode::UnderBase)
                .map_err(|error| TransportError::new(error.to_string()))?;
            self.seen.borrow_mut().push(url.to_string());
            self.bodies.borrow_mut().push(request.body.clone());
            let mut responses = self.responses.borrow_mut();
            let body = if responses.len() > 1 {
                responses.remove(0)
            } else {
                responses.first().cloned().unwrap_or(Value::Null)
            };
            Ok(WireResponse::new(
                200,
                url.to_string(),
                Vec::new(),
                body.to_string().into_bytes(),
            ))
        }
    }

    fn client(base: &str, response: Value) -> CoreClient<Recorder> {
        CoreClient::new(Recorder::new(base, vec![response]))
    }

    #[tokio::test]
    async fn an_operation_is_routed_through_the_contract_and_the_transport() {
        let client = client(
            "https://acme.example/primary/tapes/",
            serde_json::json!({"traces": []}),
        );
        let _ = client
            .get_session_traces("s-1", &SessionTracesParams::default())
            .await
            .unwrap();

        assert_eq!(
            client.transport().seen.borrow()[0],
            "https://acme.example/primary/tapes/v1/sessions/s-1/traces",
        );
    }

    #[tokio::test]
    async fn a_typed_method_decodes_the_contracts_own_shape() {
        // The default surface: the caller names no type, and the fields it
        // reads are the ones the sealed document publishes.
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({
                "items": [{"id": "s1", "rollup": {"turn_count": 3}}],
                "next_cursor": "abc",
            }),
        );
        let listing = client
            .list_sessions(&SessionListParams::default())
            .await
            .unwrap();

        assert_eq!(listing.items[0].id, "s1");
        assert_eq!(listing.items[0].rollup.turn_count, 3);
        assert_eq!(listing.next_cursor, "abc");
    }

    #[tokio::test]
    async fn a_typed_method_survives_a_field_it_has_never_heard_of() {
        // The rule the models are built on, exercised end to end: a newer
        // server is not a malformed response.
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"items": [{"id": "s1", "a_field_from_the_future": 7}]}),
        );
        let listing = client
            .list_sessions(&SessionListParams::default())
            .await
            .unwrap();
        assert_eq!(listing.items[0].id, "s1");
    }

    #[tokio::test]
    async fn the_generic_seam_still_decodes_into_a_callers_own_type() {
        // The escape hatch stays reachable, and stays untyped when a caller
        // asks for a document rather than a model.
        #[derive(Debug, Deserialize)]
        struct Listing {
            next_cursor: String,
        }

        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"items": [], "next_cursor": "abc"}),
        );
        let got: Listing = client.call(ops::LIST_SESSIONS, Vec::new()).await.unwrap();
        assert_eq!(got.next_cursor, "abc");

        let raw: Value = client.call(ops::LIST_SESSIONS, Vec::new()).await.unwrap();
        assert_eq!(raw["next_cursor"], "abc");
    }

    #[tokio::test]
    async fn a_typed_parameter_travels_under_the_contracts_own_name() {
        let client = client("http://127.0.0.1:8081", serde_json::json!({"traces": []}));
        let _ = client
            .get_session_traces(
                "s-1",
                &SessionTracesParams {
                    payload: Some(PayloadDetail::Preview),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            client.transport().seen.borrow()[0].ends_with("/traces?payload=preview"),
            "got: {:?}",
            client.transport().seen.borrow(),
        );
    }

    #[tokio::test]
    async fn a_page_request_travels_under_the_contracts_own_names_on_every_paged_read() {
        // The three reads the contract pages in place take `limit` and
        // `cursor` under exactly those names, after any parameter they
        // already had; a spelling of its own on any one of them would be a
        // second pagination convention.
        let client = client("http://127.0.0.1:8081", serde_json::json!({}));
        let _ = client
            .get_session_traces(
                "s-1",
                &SessionTracesParams {
                    payload: Some(PayloadDetail::Full),
                    limit: Some(50),
                    cursor: Some("c1".to_owned()),
                },
            )
            .await
            .unwrap();
        let _ = client
            .get_trace(
                "t-1",
                &TraceParams {
                    payload: None,
                    limit: Some(200),
                    cursor: Some("c2".to_owned()),
                },
            )
            .await
            .unwrap_err(); // `{}` lacks the required `session_id`; the URL was still built.
        let _ = client
            .list_raw_turns(
                "s-1",
                &RawTurnListParams {
                    limit: Some(1000),
                    cursor: Some("c3".to_owned()),
                },
            )
            .await
            .unwrap();

        let seen = client.transport().seen.borrow();
        assert_eq!(
            *seen,
            vec![
                "http://127.0.0.1:8081/v1/sessions/s-1/traces?payload=full&limit=50&cursor=c1",
                "http://127.0.0.1:8081/v1/traces/t-1?limit=200&cursor=c2",
                "http://127.0.0.1:8081/v1/sessions/s-1/raw_turns?limit=1000&cursor=c3",
            ],
        );
    }

    #[tokio::test]
    async fn a_standalone_trace_page_without_its_session_id_is_refused() {
        // The one required property in the contract: a server that drops
        // the guarantee it publishes is a decode failure, not a trace whose
        // session happens to be "".
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"trace": {"trace_id": "t-1"}, "spans": []}),
        );
        let err = client
            .get_trace("t-1", &TraceParams::default())
            .await
            .unwrap_err();
        let crate::Error::Decode { source } = &err else {
            panic!("expected a decode failure, got: {err}");
        };
        assert!(
            source.to_string().contains("session_id"),
            "the refusal must name the missing property: {source}",
        );
    }

    #[tokio::test]
    async fn a_null_cursor_ends_a_whole_read_instead_of_failing_it() {
        // The walker already reads absent, "" and null as the end of a
        // listing; a typed final page spelling it as null must decode too,
        // on every paged read, or an otherwise good read fails at its end.
        let traces = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"traces": [{"trace": {"trace_id": "t-1"}, "spans": []}], "next_cursor": null}),
        );
        let whole = traces
            .get_whole_session_traces("s-1", &SessionTracesParams::default())
            .await
            .unwrap();
        assert_eq!(whole.traces.len(), 1);
        assert_eq!(whole.next_cursor, "");

        let trace = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"session_id": "s-1", "trace": {"trace_id": "t-1"}, "spans": [{"span_id": "sp-1"}], "next_cursor": null}),
        );
        let whole = trace
            .get_whole_trace("t-1", &TraceParams::default())
            .await
            .unwrap();
        assert_eq!(whole.spans.len(), 1);
        assert_eq!(whole.next_cursor, "");

        let turns = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"items": [{"id": 7}], "next_cursor": null}),
        );
        let all = turns
            .list_all_raw_turns("s-1", &RawTurnListParams::default())
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn a_listing_walk_follows_the_cursor_to_the_end() {
        // The models and the crate's one pagination convention meet here: the
        // envelope becomes a `Page`, and `page::walk` owns the loop.
        let client = CoreClient::new(Recorder::new(
            "http://127.0.0.1:8081",
            vec![
                serde_json::json!({"items": [{"id": "s1"}], "next_cursor": "c1"}),
                serde_json::json!({"items": [{"id": "s2"}], "next_cursor": ""}),
            ],
        ));
        let sessions = client
            .list_all_sessions(&SessionListParams::default())
            .await
            .unwrap();

        assert_eq!(
            sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s2"],
        );
        assert!(
            client.transport().seen.borrow()[1].contains("cursor=c1"),
            "got: {:?}",
            client.transport().seen.borrow(),
        );
    }

    #[tokio::test]
    async fn a_session_traces_walk_keeps_the_first_envelope_and_appends_every_page() {
        // The composite is paged in `traces` alone: `session`, `links`, and
        // `schema` repeat on every page, so the whole-session helper keeps
        // one copy of them and concatenates the paged vector — leaving
        // exactly what the unpaged response used to look like.
        let client = CoreClient::new(Recorder::new(
            "http://127.0.0.1:8081",
            vec![
                serde_json::json!({
                    "schema": "20260615",
                    "session": {"id": "s-1"},
                    "links": [{"from_span_id": "a", "to_span_id": "b"}],
                    "traces": [{"trace": {"trace_id": "t-1"}}],
                    "next_cursor": "c1",
                }),
                serde_json::json!({
                    "schema": "20260615",
                    "session": {"id": "s-1"},
                    "links": [{"from_span_id": "a", "to_span_id": "b"}],
                    "traces": [{"trace": {"trace_id": "t-2"}}],
                }),
            ],
        ));
        let whole = client
            .get_whole_session_traces(
                "s-1",
                &SessionTracesParams {
                    payload: Some(PayloadDetail::Preview),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(whole.session.id, "s-1");
        assert_eq!(whole.schema, "20260615");
        assert_eq!(
            whole.links.len(),
            1,
            "the envelope is kept once, not per page"
        );
        assert_eq!(
            whole
                .traces
                .iter()
                .map(|t| t.trace.trace_id.as_str())
                .collect::<Vec<_>>(),
            vec!["t-1", "t-2"],
        );
        assert_eq!(whole.next_cursor, "", "a whole response has no next page");
        let seen = client.transport().seen.borrow();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].contains("cursor=c1"), "got: {seen:?}");
        assert!(
            seen.iter().all(|url| url.contains("payload=preview")),
            "every page of a walk must carry the caller's parameters: {seen:?}",
        );
    }

    #[tokio::test]
    async fn a_trace_walk_keeps_the_first_envelope_and_appends_every_span_page() {
        let client = CoreClient::new(Recorder::new(
            "http://127.0.0.1:8081",
            vec![
                serde_json::json!({
                    "session_id": "s-1",
                    "trace": {"trace_id": "t-1"},
                    "links": [{"from_span_id": "a", "to_span_id": "b"}],
                    "spans": [{"span_id": "sp-1"}, {"span_id": "sp-2"}],
                    "next_cursor": "c1",
                }),
                serde_json::json!({
                    "session_id": "s-1",
                    "trace": {"trace_id": "t-1"},
                    "links": [{"from_span_id": "a", "to_span_id": "b"}],
                    "spans": [{"span_id": "sp-3"}],
                    "next_cursor": "",
                }),
            ],
        ));
        let whole = client
            .get_whole_trace("t-1", &TraceParams::default())
            .await
            .unwrap();

        assert_eq!(whole.session_id, "s-1");
        assert_eq!(whole.trace.trace_id, "t-1");
        assert_eq!(
            whole.links.len(),
            1,
            "the envelope is kept once, not per page"
        );
        assert_eq!(
            whole
                .spans
                .iter()
                .map(|s| s.span_id.as_str())
                .collect::<Vec<_>>(),
            vec!["sp-1", "sp-2", "sp-3"],
        );
        assert_eq!(whole.next_cursor, "");
        assert!(
            client.transport().seen.borrow()[1].ends_with("/v1/traces/t-1?cursor=c1"),
            "got: {:?}",
            client.transport().seen.borrow(),
        );
    }

    #[tokio::test]
    async fn a_raw_turn_walk_follows_the_cursor_to_the_end() {
        let client = CoreClient::new(Recorder::new(
            "http://127.0.0.1:8081",
            vec![
                serde_json::json!({"items": [{"id": 1}], "next_cursor": "c1"}),
                serde_json::json!({"items": [{"id": 2, "raw_response_dropped": true}]}),
            ],
        ));
        let turns = client
            .list_all_raw_turns("s-1", &RawTurnListParams::default())
            .await
            .unwrap();

        assert_eq!(turns.iter().map(|t| t.id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(turns[1].raw_response_dropped);
        assert!(
            client.transport().seen.borrow()[1].ends_with("/v1/sessions/s-1/raw_turns?cursor=c1"),
            "got: {:?}",
            client.transport().seen.borrow(),
        );
    }

    #[tokio::test]
    async fn a_whole_read_stops_on_a_repeated_cursor_rather_than_hanging() {
        // The whole-envelope helpers ride `page::walk`, so they inherit its
        // guard: a server that hands back the cursor it was just given ends
        // the walk after one repeat instead of paging forever.
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({
                "session_id": "s-1",
                "trace": {"trace_id": "t-1"},
                "spans": [{"span_id": "sp-1"}],
                "next_cursor": "stuck",
            }),
        );
        let whole = client
            .get_whole_trace("t-1", &TraceParams::default())
            .await
            .unwrap();
        assert_eq!(client.transport().seen.borrow().len(), 2);
        assert_eq!(whole.spans.len(), 2);
    }

    #[tokio::test]
    async fn an_undeclared_parameter_is_refused_before_the_transport_is_reached() {
        let client = client("http://127.0.0.1:8081", Value::Null);
        let err = client
            .call::<Value>(ops::GET_SESSION, vec![("payolad", "full".to_owned())])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("payolad"), "got: {err}");
        assert!(
            client.transport().seen.borrow().is_empty(),
            "nothing may be sent for a call the contract refused",
        );
    }

    #[tokio::test]
    async fn a_typed_body_reaches_the_transport_as_the_contracts_own_json() {
        // The gap this closes: the body capability exists one layer down, and
        // a facade that routed around it would drop a payload passed here
        // while still producing a request that looked correct.
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"session": {"id": "s-1"}}),
        );
        let updated = client
            .update_session(
                "s-1",
                &SessionUpdateRequest {
                    display_name: Some("gum glow charm".to_owned()),
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.session.id, "s-1");
        let bodies = client.transport().bodies.borrow();
        let sent: Value = serde_json::from_str(bodies[0].as_deref().unwrap()).unwrap();
        assert_eq!(sent["display_name"], "gum glow charm");
    }

    #[tokio::test]
    async fn the_bodyless_facade_refuses_an_operation_that_requires_a_body() {
        // The whole point of the refusal is that it survives every route to
        // the wire; a facade that quietly sent the request anyway would be the
        // original silence with an extra layer on top.
        let client = client("http://127.0.0.1:8081", Value::Null);
        let err = client
            .call::<Value>(ops::UPDATE_SESSION, vec![("id", "s-1".to_owned())])
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("requires a request body"),
            "got: {err}",
        );
        assert!(client.transport().seen.borrow().is_empty());
    }

    #[tokio::test]
    async fn the_facade_refuses_a_body_on_an_operation_that_declares_none() {
        let client = client("http://127.0.0.1:8081", Value::Null);
        let err = client
            .call_with_body::<Value>(
                ops::GET_SESSION,
                vec![("id", "s-1".to_owned())],
                Some("{}".to_owned()),
            )
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("declares no request body"),
            "got: {err}",
        );
        assert!(client.transport().seen.borrow().is_empty());
    }

    #[tokio::test]
    async fn the_bodyless_facade_still_sends_no_body_for_an_ordinary_read() {
        // Plumbing a body through must not start attaching one where none was
        // asked for: every read operation goes out exactly as before.
        let client = client("http://127.0.0.1:8081", serde_json::json!({"items": []}));
        let _ = client
            .list_sessions(&SessionListParams::default())
            .await
            .unwrap();
        assert_eq!(client.transport().bodies.borrow().as_slice(), [None]);
    }

    impl crate::transport::StreamingTransport for Recorder {
        type Body = Vec<u8>;

        async fn send_stream(&self, request: &WireRequest<'_>) -> Result<Self::Body> {
            let url = call_url(&self.base, request, PathMode::UnderBase).map_err(|error| {
                crate::Error::Transport {
                    source: TransportError::new(error.to_string()),
                }
            })?;
            self.seen.borrow_mut().push(url.to_string());
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn the_stream_escape_hatch_builds_the_contract_url() {
        // Fidelity reads travel through the generic operation-id seam as
        // streams; the stream route must build the same contract URL as a
        // buffered call, or the two ways of asking would disagree.
        let client = CoreClient::new(Recorder::new(
            "http://127.0.0.1:8081",
            vec![serde_json::json!({})],
        ));
        let _ = client
            .stream(ops::LIST_RAW_TURNS, vec![("id", "s-1".to_owned())])
            .await
            .unwrap();
        let seen = client.transport().seen.borrow();
        assert_eq!(seen[0], "http://127.0.0.1:8081/v1/sessions/s-1/raw_turns");
    }

    #[tokio::test]
    async fn a_named_method_and_its_operation_id_build_the_same_request() {
        // The named methods must stay conveniences. If one ever routed a value
        // differently from the operation id it names, this crate would be back
        // to two ways of building a request that can disagree.
        let named = client("http://127.0.0.1:8081", serde_json::json!({}));
        let _ = named.get_span("t-1", "sp-1").await.unwrap();

        let raw = client("http://127.0.0.1:8081", serde_json::json!({}));
        let _: Value = raw
            .call(
                ops::GET_SPAN,
                vec![
                    ("trace_id", "t-1".to_owned()),
                    ("span_id", "sp-1".to_owned()),
                ],
            )
            .await
            .unwrap();

        assert_eq!(
            *named.transport().seen.borrow(),
            *raw.transport().seen.borrow()
        );
    }

    #[tokio::test]
    async fn claimed_params_append_to_the_query_in_order() {
        // The pairs travel exactly as given: after the declared parameters,
        // repeats preserved, order preserved. The names are runtime data the
        // vendored contract cannot declare — a cassette claims them on the
        // live server — so they bypass the declared-parameter refusal
        // without loosening it (the refusal test above still stands).
        let client = client("http://127.0.0.1:8081", serde_json::json!({"items": []}));
        let _ = client
            .list_sessions(&SessionListParams {
                limit: Some(25),
                claimed: vec![
                    ("flavor".to_owned(), "grape".to_owned()),
                    ("flavor".to_owned(), "sour cherry".to_owned()),
                    ("vintage".to_owned(), "1998".to_owned()),
                ],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            client.transport().seen.borrow()[0],
            "http://127.0.0.1:8081/v1/sessions?limit=25&flavor=grape&flavor=sour+cherry&vintage=1998",
        );
    }

    #[tokio::test]
    async fn claimed_values_are_percent_encoded_and_nothing_more() {
        // A unicode value is form-encoded on the way out, and that is the
        // whole of what happens to it: no normalization, no case folding, no
        // client-side filtering. Whether it matches anything is the server's
        // question — the claim's declared normalization profile lives there.
        let client = client("http://127.0.0.1:8081", serde_json::json!({"items": []}));
        let _ = client
            .list_sessions(&SessionListParams {
                claimed: vec![("flavor".to_owned(), "Grüße 🍇".to_owned())],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            client.transport().seen.borrow()[0],
            "http://127.0.0.1:8081/v1/sessions?flavor=Gr%C3%BC%C3%9Fe+%F0%9F%8D%87",
        );
    }

    #[tokio::test]
    async fn an_empty_claimed_set_leaves_the_request_as_it_always_was() {
        // No claimed pairs, no trace of the mechanism: the unfiltered
        // listing keeps its exact pre-feature spelling.
        let client = client("http://127.0.0.1:8081", serde_json::json!({"items": []}));
        let _ = client
            .list_sessions(&SessionListParams::default())
            .await
            .unwrap();
        assert_eq!(
            client.transport().seen.borrow()[0],
            "http://127.0.0.1:8081/v1/sessions",
        );
    }

    #[tokio::test]
    async fn the_page_walk_carries_claimed_params_onto_every_page() {
        // A filtered walk must stay filtered: the cursor mints under the
        // claimed set, and a page fetched without it would be answering a
        // different question mid-listing.
        let client = CoreClient::new(Recorder::new(
            "http://127.0.0.1:8081",
            vec![
                serde_json::json!({"items": [{"id": "s1"}], "next_cursor": "c1"}),
                serde_json::json!({"items": [{"id": "s2"}], "next_cursor": ""}),
            ],
        ));
        let _ = client
            .list_all_sessions(&SessionListParams {
                claimed: vec![("flavor".to_owned(), "grape".to_owned())],
                ..Default::default()
            })
            .await
            .unwrap();
        let seen = client.transport().seen.borrow();
        assert_eq!(seen.len(), 2);
        assert!(seen[1].contains("cursor=c1"), "got: {seen:?}");
        assert!(
            seen.iter().all(|url| url.contains("flavor=grape")),
            "every page of a filtered walk must carry the claimed pairs: {seen:?}",
        );
    }

    #[tokio::test]
    async fn the_typed_and_untyped_claimed_spellings_build_the_same_request() {
        // `call_with_claimed` is to `list_sessions` what `call` is to the
        // named methods: a spelling, not a second route. If the two ever
        // built different requests, the crate would be back to two ways of
        // asking one question.
        let named = client("http://127.0.0.1:8081", serde_json::json!({"items": []}));
        let _ = named
            .list_sessions(&SessionListParams {
                limit: Some(1),
                claimed: vec![("flavor".to_owned(), "grape".to_owned())],
                ..Default::default()
            })
            .await
            .unwrap();

        let raw = client("http://127.0.0.1:8081", serde_json::json!({"items": []}));
        let _: Value = raw
            .call_with_claimed(
                ops::LIST_SESSIONS,
                vec![("limit", "1".to_owned())],
                &[("flavor".to_owned(), "grape".to_owned())],
            )
            .await
            .unwrap();

        assert_eq!(
            *named.transport().seen.borrow(),
            *raw.transport().seen.borrow()
        );
    }

    #[tokio::test]
    async fn claimed_params_do_not_loosen_the_declared_parameter_refusal() {
        // The claimed channel is additive: a misspelled *declared* name in
        // `values` is still refused before anything is sent, claimed pairs
        // present or not.
        let client = client("http://127.0.0.1:8081", Value::Null);
        let err = client
            .call_with_claimed::<Value>(
                ops::LIST_SESSIONS,
                vec![("limt", "25".to_owned())],
                &[("flavor".to_owned(), "grape".to_owned())],
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("limt"), "got: {err}");
        assert!(
            client.transport().seen.borrow().is_empty(),
            "nothing may be sent for a call the contract refused",
        );
    }

    #[tokio::test]
    async fn claimed_pairs_on_a_non_claim_bearing_operation_are_refused() {
        // The claimed channel is a scoped bypass, not a general one: the
        // sealed contract documents the claim extension on the sessions
        // listing alone, so a non-empty claimed set anywhere else is a
        // contract refusal — before anything is sent, exactly like an
        // undeclared parameter.
        let client = client("http://127.0.0.1:8081", Value::Null);
        let err = client
            .call_with_claimed::<Value>(
                ops::GET_SESSION,
                vec![("id", "s-1".to_owned())],
                &[("flavor".to_owned(), "grape".to_owned())],
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("no claimed filter params"),
            "got: {err}",
        );
        assert!(
            client.transport().seen.borrow().is_empty(),
            "nothing may be sent for a call the contract refused",
        );
    }

    #[tokio::test]
    async fn an_empty_claimed_set_is_permitted_on_every_operation() {
        // No pairs, no restriction: `call_with_claimed` with an empty set is
        // `call` by another spelling, on claim-bearing operations and
        // otherwise alike.
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"session": {"id": "s-1"}}),
        );
        let _: Value = client
            .call_with_claimed(ops::GET_SESSION, vec![("id", "s-1".to_owned())], &[])
            .await
            .unwrap();
        assert_eq!(
            client.transport().seen.borrow()[0],
            "http://127.0.0.1:8081/v1/sessions/s-1",
        );
    }

    #[test]
    fn every_claim_bearing_operation_is_in_the_vendored_contract() {
        // The registry is a statement about the sealed document. An entry
        // naming an operation the document does not have would open the
        // claimed channel on nothing, and this is where that drift surfaces.
        for operation in ops::CLAIM_BEARING_OPS {
            assert!(
                core().unwrap().method(operation).is_ok(),
                "ops::CLAIM_BEARING_OPS names {operation:?}, which the vendored contract lacks",
            );
        }
    }
}
