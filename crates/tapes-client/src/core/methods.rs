//! Calling the sealed contract's operations over a transport.
//!
//! Every hand-written URL builder a client used to carry is one line here: the
//! verb, the path template, and the parameter routing all come from
//! `contracts/tapes-api.yaml`. A parameter the contract does not declare is
//! refused before anything is sent, because a server that ignores an unknown
//! query parameter would otherwise hide the drift a vendored contract exists to
//! catch.
//!
//! The named methods below are conveniences over [`CoreClient::call`] and
//! nothing more — they exist so a caller spells `get_session(id)` instead of
//! repeating an operation id and a parameter name, not to add a second way for
//! a request to be built. Anything not named here is still reachable by its
//! `operationId`, which is what keeps this list from becoming a second contract
//! that can disagree with the first.

use serde::de::DeserializeOwned;
use snafu::ResultExt;

use crate::core::contract::{self, core, ops};
use crate::decode;
use crate::error::{Result, error};
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
    /// Resolve one operation in the sealed contract and call it.
    ///
    /// Equivalent to [`CoreClient::call_with_body`] with no body, which is what
    /// every read operation wants. An operation whose `requestBody` the
    /// contract marks required is refused rather than sent without one.
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
    pub fn request_for(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
    ) -> Result<WireRequest<'static>> {
        contract::call_for(core()?.method(operation_id)?, values)
    }

    /// `GET /v1/sessions`
    pub async fn list_sessions<R: DeserializeOwned>(
        &self,
        values: Vec<(&str, String)>,
    ) -> Result<R> {
        self.call(ops::LIST_SESSIONS, values).await
    }

    /// `GET /v1/sessions/{id}`
    pub async fn get_session<R: DeserializeOwned>(&self, id: &str) -> Result<R> {
        self.call(ops::GET_SESSION, vec![("id", id.to_owned())])
            .await
    }

    /// `GET /v1/sessions/{id}/traces`
    pub async fn get_session_traces<R: DeserializeOwned>(&self, id: &str) -> Result<R> {
        self.call(ops::GET_SESSION_TRACES, vec![("id", id.to_owned())])
            .await
    }

    /// `GET /v1/sessions/{id}/raw_turns`
    pub async fn list_raw_turns<R: DeserializeOwned>(
        &self,
        id: &str,
        mut values: Vec<(&str, String)>,
    ) -> Result<R> {
        values.push(("id", id.to_owned()));
        self.call(ops::LIST_RAW_TURNS, values).await
    }

    /// `GET /v1/traces`
    pub async fn list_traces<R: DeserializeOwned>(&self, values: Vec<(&str, String)>) -> Result<R> {
        self.call(ops::LIST_TRACES, values).await
    }

    /// `GET /v1/traces/{trace_id}`
    pub async fn get_trace<R: DeserializeOwned>(&self, trace_id: &str) -> Result<R> {
        self.call(ops::GET_TRACE, vec![("trace_id", trace_id.to_owned())])
            .await
    }

    /// `GET /v1/traces/{trace_id}/spans/{span_id}`
    pub async fn get_span<R: DeserializeOwned>(&self, trace_id: &str, span_id: &str) -> Result<R> {
        self.call(
            ops::GET_SPAN,
            vec![
                ("trace_id", trace_id.to_owned()),
                ("span_id", span_id.to_owned()),
            ],
        )
        .await
    }

    /// `GET /v1/search/spans`
    pub async fn search_spans<R: DeserializeOwned>(
        &self,
        values: Vec<(&str, String)>,
    ) -> Result<R> {
        self.call(ops::SEARCH_SPANS, values).await
    }

    /// `GET /v1/cassettes`
    pub async fn list_cassettes<R: DeserializeOwned>(&self) -> Result<R> {
        self.call(ops::LIST_CASSETTES, Vec::new()).await
    }

    /// `POST /v1/admin/seed/demo`
    pub async fn seed_demo<R: DeserializeOwned>(&self, values: Vec<(&str, String)>) -> Result<R> {
        self.call(ops::SEED_DEMO, values).await
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
    pub async fn stream(&self, operation_id: &str, values: Vec<(&str, String)>) -> Result<T::Body> {
        let method = core()?.method(operation_id)?;
        let request = contract::call_for(method, values)?;
        self.transport.send_stream(&request).await
    }

    /// `GET /v1/sessions/{id}/export`, streamed.
    ///
    /// An export can be far larger than a session's working set, and there is
    /// no reason to hold it in memory on the way to a file.
    pub async fn export_session(&self, id: &str) -> Result<T::Body> {
        self.stream(ops::EXPORT_SESSION, vec![("id", id.to_owned())])
            .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
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
        response: Value,
        seen: RefCell<Vec<String>>,
        bodies: RefCell<Vec<Option<String>>>,
    }

    impl Recorder {
        fn new(base: &str, response: Value) -> Self {
            Self {
                base: Url::parse(base).unwrap(),
                response,
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
            Ok(WireResponse::new(
                200,
                url.to_string(),
                Vec::new(),
                self.response.to_string().into_bytes(),
            ))
        }
    }

    fn client(base: &str, response: Value) -> CoreClient<Recorder> {
        CoreClient::new(Recorder::new(base, response))
    }

    #[tokio::test]
    async fn an_operation_is_routed_through_the_contract_and_the_transport() {
        let client = client(
            "https://acme.example/primary/tapes/",
            serde_json::json!({"items": []}),
        );
        let _: Value = client.get_session_traces("s-1").await.unwrap();

        assert_eq!(
            client.transport().seen.borrow()[0],
            "https://acme.example/primary/tapes/v1/sessions/s-1/traces",
        );
    }

    #[tokio::test]
    async fn the_untyped_instantiation_passes_unknown_fields_through() {
        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"items": [{"id": "s1", "a_field_from_the_future": 7}]}),
        );
        let got: Value = client.list_sessions(Vec::new()).await.unwrap();
        assert_eq!(got["items"][0]["a_field_from_the_future"], 7);
    }

    #[tokio::test]
    async fn a_typed_instantiation_decodes_into_the_consumers_own_model() {
        // The crate takes no view, so the same operation over the same
        // transport yields whatever the caller asked for.
        #[derive(Debug, Deserialize)]
        struct Listing {
            next_cursor: String,
        }

        let client = client(
            "http://127.0.0.1:8081",
            serde_json::json!({"items": [], "next_cursor": "abc"}),
        );
        let got: Listing = client.list_sessions(Vec::new()).await.unwrap();
        assert_eq!(got.next_cursor, "abc");
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
    async fn a_body_supplied_at_the_facade_reaches_the_transport() {
        // The gap this closes: the body capability exists one layer down, and
        // a facade that routed around it would drop a payload passed here
        // while still producing a request that looked correct.
        let client = client("http://127.0.0.1:8081", serde_json::json!({"id": "sk-1"}));
        let _: Value = client
            .call_with_body(
                "createSkill",
                Vec::new(),
                Some(r#"{"name":"x"}"#.to_owned()),
            )
            .await
            .unwrap();

        assert_eq!(
            client.transport().bodies.borrow().as_slice(),
            [Some(r#"{"name":"x"}"#.to_owned())],
        );
    }

    #[tokio::test]
    async fn the_bodyless_facade_refuses_an_operation_that_requires_a_body() {
        // The whole point of the refusal is that it survives every route to
        // the wire; a facade that quietly sent the request anyway would be the
        // original silence with an extra layer on top.
        let client = client("http://127.0.0.1:8081", Value::Null);
        let err = client
            .call::<Value>("createSkill", Vec::new())
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
        let _: Value = client.list_sessions(Vec::new()).await.unwrap();
        assert_eq!(client.transport().bodies.borrow().as_slice(), [None]);
    }

    #[tokio::test]
    async fn a_named_method_and_its_operation_id_build_the_same_request() {
        // The named methods must stay conveniences. If one ever routed a value
        // differently from the operation id it names, this crate would be back
        // to two ways of building a request that can disagree.
        let named = client("http://127.0.0.1:8081", serde_json::json!({}));
        let _: Value = named.get_span("t-1", "sp-1").await.unwrap();

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
