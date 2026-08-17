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

use serde::Serialize;
use serde::de::DeserializeOwned;
use snafu::ResultExt;

use crate::cassettes::discovery::Discovery;
use crate::core::contract::{self, core, ops};
use crate::core::models::params::ContractParams;
use crate::core::models::{
    CreateSkillRequest, ExportSessionParams, ExportSessionsParams, GenerateSkillRequest,
    PublishSkillRequest, RawTurnListResponse, SearchSpansParams, SeedDemoRequest, SeedResult,
    SessionDetailResponse, SessionItem, SessionListParams, SessionListResponse,
    SessionSkillsResponse, SessionTracesParams, SessionTracesResponse, SessionUpdateRequest,
    SkillResponse, SkillVersionResponse, SkillVersionsResponse, SkillsListParams,
    SkillsListResponse, SpanItem, SpanSearchOutput, StatsParams, StatsResponse, TraceDetail,
    TraceListParams, TraceListResponse, TraceParams, UpdateSkillRequest,
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
        let method = core()?.method(operation_id)?;
        let request = contract::call_for_with_body(method, values, body)?;
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
        self.with_params(params).await
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

    /// `GET /v1/sessions/{id}/traces` — the derived span read model.
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

    /// `GET /v1/sessions/{id}/raw_turns` — the wire log behind a derivation.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_raw_turns(&self, id: &str) -> Result<RawTurnListResponse> {
        self.call(ops::LIST_RAW_TURNS, vec![("id", id.to_owned())])
            .await
    }

    /// `GET /v1/sessions/{id}/skills` — the skills attributed to one session.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_session_skills(&self, id: &str) -> Result<SessionSkillsResponse> {
        self.call(ops::LIST_SESSION_SKILLS, vec![("id", id.to_owned())])
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

    /// `GET /v1/traces/{trace_id}` — one trace with its spans.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_trace(&self, trace_id: &str, params: &TraceParams) -> Result<TraceDetail> {
        self.with_params_at(params, vec![("trace_id", trace_id.to_owned())])
            .await
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

    /// `GET /v1/cassettes/search/spans` — semantic search over span
    /// embeddings, served by the search cassette.
    ///
    /// The one route here that is not the sealed contract's own. Span search
    /// was extracted from tapes core into the search cassette, which serves
    /// the identical request and response shapes under `/v1/cassettes/search`;
    /// core's `/v1/search/spans` is retirement-bound and no longer the copy
    /// deployments keep current. The sealed operation still supplies all of
    /// the parameter and response plumbing — only the path moves — so a
    /// contract change to the search shape still lands here at vendor time.
    ///
    /// A deployment that does not serve the search cassette answers 404;
    /// typed access over the *discovered* surface, which would make that a
    /// first-class "not served here" instead, is the follow-on this literal
    /// path is the bridge to.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn search_spans(&self, params: &SearchSpansParams) -> Result<SpanSearchOutput> {
        let mut request = self.request_for(ops::SEARCH_SPANS, params.values())?;
        request.path = "/v1/cassettes/search/spans";
        let response = self
            .transport
            .send(&request)
            .await
            .context(error::TransportSnafu)?;
        decode::json_typed(&response)
    }

    /// `GET /v1/stats` — the aggregate rollups.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_stats(&self, params: &StatsParams) -> Result<StatsResponse> {
        self.with_params(params).await
    }

    /// `GET /v1/skills` — one page of the skills listing.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_skills(&self, params: &SkillsListParams) -> Result<SkillsListResponse> {
        self.with_params(params).await
    }

    /// Every skill the listing matches, following `next_cursor` to the end.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_all_skills(&self, params: &SkillsListParams) -> Result<Vec<SkillResponse>> {
        page::walk(|cursor| {
            let mut params = params.clone();
            params.cursor = cursor;
            async move { Ok(self.list_skills(&params).await?.into_page()) }
        })
        .await
    }

    /// `GET /v1/skills/{id}` — one skill.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn get_skill(&self, id: &str) -> Result<SkillResponse> {
        self.call(ops::GET_SKILL, vec![("id", id.to_owned())]).await
    }

    /// `POST /v1/skills` — author a skill.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn create_skill(&self, body: &CreateSkillRequest) -> Result<SkillResponse> {
        self.with_body(ops::CREATE_SKILL, Vec::new(), body).await
    }

    /// `PUT /v1/skills/{id}` — apply the present fields onto a skill.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn update_skill(&self, id: &str, body: &UpdateSkillRequest) -> Result<SkillResponse> {
        self.with_body(ops::UPDATE_SKILL, vec![("id", id.to_owned())], body)
            .await
    }

    /// `DELETE /v1/skills/{id}` — delete a skill.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn delete_skill(&self, id: &str) -> Result<()> {
        self.call(ops::DELETE_SKILL, vec![("id", id.to_owned())])
            .await
    }

    /// `POST /v1/skills/{id}/duplicate` — fork a skill.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn duplicate_skill(&self, id: &str) -> Result<SkillResponse> {
        self.call(ops::DUPLICATE_SKILL, vec![("id", id.to_owned())])
            .await
    }

    /// `GET /v1/skills/{id}/versions` — one skill's published history.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn list_skill_versions(&self, id: &str) -> Result<SkillVersionsResponse> {
        self.call(ops::LIST_SKILL_VERSIONS, vec![("id", id.to_owned())])
            .await
    }

    /// `POST /v1/skills/{id}/versions` — publish an immutable snapshot.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn publish_skill(
        &self,
        id: &str,
        body: &PublishSkillRequest,
    ) -> Result<SkillVersionResponse> {
        self.with_body(ops::PUBLISH_SKILL, vec![("id", id.to_owned())], body)
            .await
    }

    /// `POST /v1/skills/generate` — generate a skill from sessions.
    ///
    /// # Errors
    ///
    /// Any contract, transport, status, or decode failure; see [`crate::Error`].
    pub async fn generate_skill(&self, body: &GenerateSkillRequest) -> Result<SkillResponse> {
        self.with_body(ops::GENERATE_SKILL, Vec::new(), body).await
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

    /// `GET /v1/sessions/{id}/export`, streamed.
    ///
    /// An export can be far larger than a session's working set, and there is
    /// no reason to hold it in memory on the way to a file. It stays untyped
    /// for the same reason: an archive written through a typed decode is an
    /// archive of the fields this build happened to know about.
    ///
    /// # Errors
    ///
    /// Any contract or transport failure; see [`crate::Error`].
    pub async fn export_session(&self, id: &str, params: &ExportSessionParams) -> Result<T::Body> {
        let mut values = params.values();
        values.push(("id", id.to_owned()));
        self.stream(ops::EXPORT_SESSION, values).await
    }

    /// `GET /v1/sessions/export`, streamed.
    ///
    /// # Errors
    ///
    /// Any contract or transport failure; see [`crate::Error`].
    pub async fn export_sessions(&self, params: &ExportSessionsParams) -> Result<T::Body> {
        self.stream(ops::EXPORT_SESSIONS, params.values()).await
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
        let client = client("http://127.0.0.1:8081", serde_json::json!({"id": "sk-1"}));
        let skill = client
            .create_skill(&CreateSkillRequest {
                name: "gum".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(skill.id, "sk-1");
        let bodies = client.transport().bodies.borrow();
        let sent: Value = serde_json::from_str(bodies[0].as_deref().unwrap()).unwrap();
        assert_eq!(sent["name"], "gum");
    }

    #[tokio::test]
    async fn the_bodyless_facade_refuses_an_operation_that_requires_a_body() {
        // The whole point of the refusal is that it survives every route to
        // the wire; a facade that quietly sent the request anyway would be the
        // original silence with an extra layer on top.
        let client = client("http://127.0.0.1:8081", Value::Null);
        let err = client
            .call::<Value>(ops::CREATE_SKILL, Vec::new())
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

    #[tokio::test]
    async fn search_spans_targets_the_search_cassette_route() {
        // The one deliberate departure from the operation table: span search
        // is served by the search cassette, and the sealed operation only
        // supplies the parameter plumbing. If this URL ever reads
        // /v1/search/spans again, the client has silently moved back to the
        // retirement-bound core route.
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"query": "q", "results": []}),
        );
        let _ = client
            .search_spans(&SearchSpansParams {
                query: "retry backoff".to_owned(),
                top_k: Some(3),
            })
            .await
            .unwrap();

        let seen = client.transport().seen.borrow();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].contains("/v1/cassettes/search/spans?"),
            "expected the cassette route, got {}",
            seen[0]
        );
        assert!(
            seen[0].contains("query=retry+backoff") || seen[0].contains("query=retry%20backoff")
        );
        assert!(seen[0].contains("top_k=3"));
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
}
