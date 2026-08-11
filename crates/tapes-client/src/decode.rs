//! One decode policy, for both surfaces.
//!
//! # The two halves, and why they are separate functions
//!
//! Decoding a tapes response is two decisions that used to live in two crates
//! and disagree:
//!
//! 1. **Bytes to a document.** Is this a success? If not, the body is the error
//!    message and must survive. Is the body empty? A 204 is a real answer, not
//!    a decode failure. That is [`json`].
//! 2. **Document to a caller's type.** That is [`typed`], and this crate takes
//!    no view on what the type should be.
//!
//! Keeping them separate is what lets a consumer that already holds a decoded
//! document — one whose own client did the fetching — reach the same typed
//! decode the transport-driven path uses, instead of a second one that rounds
//! differently.
//!
//! # Why `T` is the caller's choice
//!
//! The right answer is genuinely per-operation:
//!
//! - **Rendering operations** decode into models, so a client can lay fields
//!   out rather than print a document.
//! - **Fidelity operations** — export, raw turns — stay [`Value`]. A typed
//!   decode there silently truncates the archive an old client writes of a
//!   newer server's data, and it fails at the *response* level, so one
//!   unmodelled field can blank a whole page.
//!
//! Whichever a consumer picks, the rule for anything this crate ever types is
//! that enums carry a fallback variant: an added variant must never error an
//! old client.

use serde::de::DeserializeOwned;
use serde_json::Value;
use snafu::ResultExt;

use crate::error::{Error, Result, error};
use crate::transport::WireResponse;

/// Turn one response's bytes into a JSON document.
///
/// A non-success status becomes [`Error::ApiStatus`] carrying the body, because
/// every tapes error body is `{"error": "..."}` and the bare status never names
/// the offending parameter.
///
/// A successful response with no body decodes to [`Value::Null`] rather than
/// failing: cassette routes are free to answer 204, and a client that treated
/// that as malformed would refuse a perfectly good answer.
pub fn json(response: &WireResponse) -> Result<Value> {
    if !response.is_success() {
        return Err(Error::ApiStatus {
            status: response.status,
            endpoint: response.endpoint.clone(),
            body: String::from_utf8_lossy(&response.body).into_owned(),
        });
    }
    if response.body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&response.body).context(error::DecodeSnafu)
}

/// Decode a document into the type the caller asked for.
pub fn typed<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).context(error::DecodeSnafu)
}

/// Both halves, for the common case.
pub fn json_typed<T: DeserializeOwned>(response: &WireResponse) -> Result<T> {
    typed(json(response)?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn response(status: u16, body: &str) -> WireResponse {
        WireResponse::new(
            status,
            "http://127.0.0.1:8081/v1/sessions".to_owned(),
            Vec::new(),
            body.as_bytes().to_vec(),
        )
    }

    #[test]
    fn an_error_body_is_surfaced_with_the_status() {
        // The bare status never names the offending parameter; the body does.
        let err = json(&response(400, r#"{"error":"invalid cursor"}"#)).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("400"), "got: {rendered}");
        assert!(rendered.contains("invalid cursor"), "got: {rendered}");
        assert!(rendered.contains("/v1/sessions"), "got: {rendered}");
    }

    #[test]
    fn a_successful_empty_body_is_null_not_a_decode_failure() {
        // Pinned from the cassette surface, and now the rule for both: a 204
        // is an answer.
        assert_eq!(json(&response(204, "")).unwrap(), Value::Null);
        assert_eq!(json(&response(200, "  \n ")).unwrap(), Value::Null);
    }

    #[test]
    fn the_untyped_decode_passes_unknown_fields_through() {
        // The fidelity half of the per-operation policy: a field this build
        // has never heard of must survive to the caller.
        let got: Value = json_typed(&response(
            200,
            r#"{"items":[{"a_field_from_the_future":7}]}"#,
        ))
        .unwrap();
        assert_eq!(got["items"][0]["a_field_from_the_future"], 7);
    }

    #[test]
    fn a_typed_decode_reads_the_consumers_own_model() {
        #[derive(Debug, Deserialize)]
        struct Listing {
            next_cursor: String,
        }

        let got: Listing =
            json_typed(&response(200, r#"{"items":[],"next_cursor":"abc"}"#)).unwrap();
        assert_eq!(got.next_cursor, "abc");
    }

    #[test]
    fn a_body_that_is_not_json_is_a_decode_failure_and_not_a_status_one() {
        let err = json(&response(200, "<html>nope</html>")).unwrap_err();
        assert!(err.to_string().contains("could not decode"), "got: {err}",);
    }
}
