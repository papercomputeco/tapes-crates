//! The seam a consumer plugs its own HTTP client into.
//!
//! # Why the transport is a seam rather than a client
//!
//! The two consumers of this contract do not share a transport and should not
//! be made to. One speaks to a tapes server directly with no credential at
//! all; the other mints a fresh token per request, sends it under a header
//! chosen so `Authorization` stays free for upstream provider credentials,
//! retries once on a 401, and pins its TLS. None of that is contract
//! knowledge. [`ReadTransport`] is the line between them: this crate decides
//! *which request*, the consumer decides *how it is sent*.
//!
//! The shape is [`tapes_cassette_client::SpecTransport`]'s, which already
//! solved this for cassettes, minus discovery (the read lane has a vendored
//! document and needs none) and plus streaming (export must not be buffered).
//!
//! # Why the response type is a parameter
//!
//! [`ReadOperations::call_operation`] is generic in `T`, and the crate takes no
//! view on what it should be — because the right answer is genuinely
//! per-operation, and the migration adopts an explicit policy for it:
//!
//! - **Rendering operations** decode into generated types, so a client can lay
//!   fields out rather than print a document.
//! - **Fidelity operations** — export, raw turns — stay
//!   [`serde_json::Value`]. A typed decode there silently truncates the
//!   archive an old client writes of a newer server's data, and it fails at
//!   the *response* level, so one unmodelled field can blank a whole page.
//!
//! Whichever a consumer picks, the rule for anything this crate ever types is
//! that enums carry a fallback variant: an added variant must never error an
//! old client.

use serde::de::DeserializeOwned;
use serde_json::Value;
use tapes_cassette_client::Call;

use crate::contract::{self, core};
use crate::error::{Error, Result};

/// What the read lane needs from a transport: no discovery, no spec fetch.
///
/// The associated error is only ever displayed or returned to the consumer's
/// own layer, so a consumer's existing error type plugs in without conversion.
pub trait ReadTransport {
    /// The transport's own error type.
    type Error: std::fmt::Display;

    /// Execute one described call and decode the JSON response.
    ///
    /// Untyped at this level on purpose: a transport's job is bytes in, bytes
    /// out. [`ReadOperations::call_operation`] is where a response becomes a
    /// consumer's chosen type.
    fn execute(&self, call: &Call<'_>) -> impl Future<Output = Result<Value, Self::Error>>;

    /// Execute one described call and hand back the live response.
    ///
    /// An export can be far larger than a session's working set, and there is
    /// no reason to hold it in memory on the way to a file. An implementation
    /// must surface a non-success status as an error here rather than as a
    /// readable body, so a caller streaming to a file can never write an error
    /// page into it.
    fn execute_stream(
        &self,
        call: &Call<'_>,
    ) -> impl Future<Output = Result<reqwest::Response, Self::Error>>;
}

/// Calling the vendored contract's operations over a [`ReadTransport`].
///
/// Blanket-implemented: a consumer implements the transport and gets these for
/// free. The `From<Error>` bound is how the contract layer's refusals reach the
/// consumer's own error type without this crate knowing anything about it.
pub trait ReadOperations: ReadTransport
where
    Self::Error: From<Error>,
{
    /// Resolve one core operation in the vendored contract and call it.
    ///
    /// Every hand-written URL builder a consumer used to carry is this line:
    /// the verb, the path template, and the parameter routing all come from
    /// `contracts/tapes-api.yaml`.
    ///
    /// Equivalent to [`ReadOperations::call_operation_with_body`] with no
    /// body, which is what every read operation wants. An operation whose
    /// `requestBody` the contract marks required is refused rather than sent
    /// without one — the same refusal, for the same reason, as at
    /// [`crate::contract::call_for`].
    fn call_operation<T: DeserializeOwned>(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
    ) -> impl Future<Output = Result<T, Self::Error>> {
        self.call_operation_with_body(operation_id, values, None)
    }

    /// Resolve one core operation and call it with a request body.
    ///
    /// The body travels the same route as every other value: the contract
    /// decides whether the operation accepts one, requires one, or takes none,
    /// and a disagreement in either direction is refused before anything is
    /// sent. Without this method the capability would exist at
    /// [`crate::contract::call_for_with_body`] and be unreachable from the
    /// facade every consumer actually calls — which would put the payload back
    /// in exactly the place it can go missing quietly.
    fn call_operation_with_body<T: DeserializeOwned>(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
        body: Option<String>,
    ) -> impl Future<Output = Result<T, Self::Error>> {
        async move {
            let method = core()?.method(operation_id)?;
            let call = contract::call_for_with_body(method, values, body)?;
            let value = self.execute(&call).await?;
            Ok(serde_json::from_value(value).map_err(|source| Error::Decode { source })?)
        }
    }

    /// Resolve one core operation and stream its response.
    ///
    /// Bodyless, and deliberately so: nothing in this contract both streams a
    /// response and takes a request body. That is an observation about the
    /// document rather than a rule, so it is not enforced here — an operation
    /// that did take a required body would be refused by
    /// [`crate::contract::call_for`] with the same loud error as anywhere
    /// else, which is a signal to add the body-bearing sibling rather than a
    /// payload going missing.
    fn stream_operation(
        &self,
        operation_id: &str,
        values: Vec<(&str, String)>,
    ) -> impl Future<Output = Result<reqwest::Response, Self::Error>> {
        async move {
            let method = core()?.method(operation_id)?;
            let call = contract::call_for(method, values)?;
            self.execute_stream(&call).await
        }
    }
}

impl<Tr> ReadOperations for Tr
where
    Tr: ReadTransport,
    Tr::Error: From<Error>,
{
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::ops;
    use crate::invoke::{PathMode, call_url};
    use serde::Deserialize;
    use std::cell::RefCell;
    use url::Url;

    /// A transport that records what it was asked to send and answers with a
    /// canned response — enough to prove the contract layer routed the values
    /// and that `T` is the caller's choice, without a socket.
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

    impl ReadTransport for Recorder {
        type Error = Error;

        async fn execute(&self, call: &Call<'_>) -> Result<Value, Error> {
            let url = call_url(&self.base, call, PathMode::UnderBase)?;
            self.seen.borrow_mut().push(url.to_string());
            self.bodies.borrow_mut().push(call.body.clone());
            Ok(self.response.clone())
        }

        async fn execute_stream(&self, _call: &Call<'_>) -> Result<reqwest::Response, Error> {
            unimplemented!("not exercised by these tests")
        }
    }

    #[tokio::test]
    async fn an_operation_is_routed_through_the_contract_and_the_consumers_transport() {
        let recorder = Recorder::new(
            "https://acme.example/primary/tapes/",
            serde_json::json!({"items": []}),
        );
        let _: Value = recorder
            .call_operation(ops::GET_SESSION_TRACES, vec![("id", "s-1".to_owned())])
            .await
            .unwrap();

        assert_eq!(
            recorder.seen.borrow()[0],
            "https://acme.example/primary/tapes/v1/sessions/s-1/traces",
        );
    }

    #[tokio::test]
    async fn the_untyped_instantiation_passes_unknown_fields_through() {
        // The fidelity half of the per-operation policy: a field this build
        // has never heard of must survive to the caller.
        let recorder = Recorder::new(
            "http://127.0.0.1:8081",
            serde_json::json!({"items": [{"id": "s1", "a_field_from_the_future": 7}]}),
        );
        let got: Value = recorder
            .call_operation(ops::LIST_SESSIONS, Vec::new())
            .await
            .unwrap();
        assert_eq!(got["items"][0]["a_field_from_the_future"], 7);
    }

    #[tokio::test]
    async fn a_typed_instantiation_decodes_into_the_consumers_own_model() {
        // The rendering half: the crate takes no view, so the same operation
        // over the same transport yields whatever the caller asked for.
        #[derive(Debug, Deserialize)]
        struct Listing {
            next_cursor: String,
        }

        let recorder = Recorder::new(
            "http://127.0.0.1:8081",
            serde_json::json!({"items": [], "next_cursor": "abc"}),
        );
        let got: Listing = recorder
            .call_operation(ops::LIST_SESSIONS, Vec::new())
            .await
            .unwrap();
        assert_eq!(got.next_cursor, "abc");
    }

    #[tokio::test]
    async fn an_undeclared_parameter_is_refused_before_the_transport_is_reached() {
        let recorder = Recorder::new("http://127.0.0.1:8081", Value::Null);
        let err = recorder
            .call_operation::<Value>(ops::GET_SESSION, vec![("payolad", "full".to_owned())])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("payolad"), "got: {err}");
        assert!(
            recorder.seen.borrow().is_empty(),
            "nothing may be sent for a call the contract refused",
        );
    }

    #[tokio::test]
    async fn a_body_supplied_at_the_facade_reaches_the_transport() {
        // The gap this closes: the body capability existed one layer down and
        // the facade every consumer actually calls routed around it, so a
        // payload passed at this level would have been dropped on the way to
        // a request that still looked correct.
        let recorder = Recorder::new("http://127.0.0.1:8081", serde_json::json!({"id": "sk-1"}));
        let _: Value = recorder
            .call_operation_with_body(
                "createSkill",
                Vec::new(),
                Some(r#"{"name":"x"}"#.to_owned()),
            )
            .await
            .unwrap();

        assert_eq!(
            recorder.bodies.borrow().as_slice(),
            [Some(r#"{"name":"x"}"#.to_owned())],
        );
    }

    #[tokio::test]
    async fn the_bodyless_facade_refuses_an_operation_that_requires_a_body() {
        // The same loud error as at the lower layer, pinned here too: the
        // whole point of the refusal is that it survives every route to the
        // wire, and a facade that quietly sent the request anyway would be
        // the original silence with an extra layer on top.
        let recorder = Recorder::new("http://127.0.0.1:8081", Value::Null);
        let err = recorder
            .call_operation::<Value>("createSkill", Vec::new())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("requires a request body"),
            "got: {err}",
        );
        assert!(
            recorder.seen.borrow().is_empty(),
            "nothing may be sent for a call the contract refused",
        );
    }

    #[tokio::test]
    async fn the_facade_refuses_a_body_on_an_operation_that_declares_none() {
        let recorder = Recorder::new("http://127.0.0.1:8081", Value::Null);
        let err = recorder
            .call_operation_with_body::<Value>(
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
        assert!(recorder.seen.borrow().is_empty());
    }

    #[tokio::test]
    async fn the_bodyless_facade_still_sends_no_body_for_an_ordinary_read() {
        // Plumbing a body through must not start attaching one where none was
        // asked for: every read operation goes out exactly as before.
        let recorder = Recorder::new("http://127.0.0.1:8081", serde_json::json!({"items": []}));
        let _: Value = recorder
            .call_operation(ops::LIST_SESSIONS, Vec::new())
            .await
            .unwrap();
        assert_eq!(recorder.bodies.borrow().as_slice(), [None]);
    }
}
