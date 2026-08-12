//! The error type this crate published, retained for compatibility.
//!
//! # Why this one type did not become a re-export
//!
//! Every other item in this crate is now a re-export of [`tapes_client`], whose
//! single error taxonomy replaced the two that used to describe one API. This
//! type is the exception, and the reason is a property of Rust rather than a
//! design preference: a consumer that matches this enum *exhaustively* — no
//! wildcard arm — stops compiling the moment a variant is added or removed. The
//! merged taxonomy necessarily has both more variants and different ones, so
//! aliasing this name to it would break exactly the consumers this shim exists
//! to keep working.
//!
//! So the enum is preserved verbatim and is **inert**: nothing in this crate or
//! in `tapes-client` constructs one. The re-exported functions return
//! [`tapes_client::Error`], and a consumer's `From<tapes_cassette_client::Error>`
//! implementation compiles but is never reached. That is a deliberate, visible
//! seam rather than a hidden one — a consumer moving to `tapes-client` deletes
//! its conversion and writes one for the merged taxonomy, which is the same
//! work it would have done anyway, at a time it chooses.
//!
//! This module is deleted with the rest of the shim.

use snafu::Snafu;

/// Convenience alias defaulting the error to this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors this crate surfaced before its machinery moved.
///
/// Deliberately **not** `#[non_exhaustive]`: it was not, and a consumer
/// matching it without a wildcard is relying on that.
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
pub enum Error {
    /// An endpoint could not be built from the base URL.
    #[snafu(display("could not build the API endpoint"))]
    Url {
        /// Underlying join failure.
        source: url::ParseError,
    },

    /// The configured base URL cannot carry a path (e.g. `mailto:`), so no
    /// route can be appended to it.
    #[snafu(display("the tapes URL cannot be used as a base for API routes"))]
    NotABase,

    /// The HTTP client itself could not be constructed.
    #[snafu(display("could not initialize the HTTP client"))]
    ClientInit,

    /// The request itself failed.
    #[snafu(display("could not reach the tapes API"))]
    Send {
        /// Underlying transport failure.
        source: reqwest::Error,
    },

    /// The server answered with a non-success status. The body is carried
    /// because every tapes error body names the offending parameter.
    #[snafu(display("tapes API returned {status} for {endpoint}: {body}"))]
    Status {
        /// HTTP status returned.
        status: u16,
        /// Endpoint that was called.
        endpoint: String,
        /// Response body, verbatim.
        body: String,
    },

    /// The server answered with something that is not JSON.
    #[snafu(display("could not decode the tapes API response"))]
    Decode {
        /// Underlying JSON failure.
        source: serde_json::Error,
    },

    /// The server's response shape changed out from under this client.
    #[snafu(display("unexpected server contract: {detail}"))]
    Contract {
        /// What changed.
        detail: &'static str,
    },

    /// Discovery named an OpenAPI document somewhere other than on this server.
    #[snafu(display("cassette discovery named a non-relative OpenAPI path {path:?}"))]
    SpecPath {
        /// What discovery published.
        path: String,
    },

    /// A cassette's spec described an operation with a verb that is not an
    /// HTTP method.
    #[snafu(display("cassette spec used an unusable HTTP method {method:?}"))]
    Method {
        /// The offending verb.
        method: String,
    },

    /// A cassette noun parsed but is not on the surface.
    #[snafu(display("no cassette named {name:?} is served here"))]
    UnknownCassette {
        /// The noun that was invoked.
        name: String,
    },

    /// A cassette method parsed but is not on the cassette.
    #[snafu(display("cassette {cassette:?} has no method {method:?}"))]
    UnknownMethod {
        /// The cassette that was invoked.
        cassette: String,
        /// The method that was invoked.
        method: String,
    },

    /// `--body @<path>` could not be read.
    #[snafu(display("could not read the request body at {path}"))]
    BodyFile {
        /// Where the read was attempted.
        path: String,
        /// Underlying IO failure.
        source: std::io::Error,
    },

    /// `--body` was not JSON.
    #[snafu(display("--body is not valid JSON"))]
    InvalidBody {
        /// Underlying JSON failure.
        source: serde_json::Error,
    },

    /// The parsed body could not be re-rendered for sending.
    #[snafu(display("could not render the request body"))]
    RenderBody {
        /// Underlying JSON failure.
        source: serde_json::Error,
    },
}
