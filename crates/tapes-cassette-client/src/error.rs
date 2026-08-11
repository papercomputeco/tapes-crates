//! Error type for the cassette-client machinery.
//!
//! Consumers are expected to wrap these variants in their own error type —
//! `tapesctl` maps each one onto the CLI error it surfaced before the
//! extraction, so the user-facing messages there are unchanged. The displays
//! here mirror those messages so a consumer that passes them through verbatim
//! reads the same way.

use snafu::Snafu;

/// Convenience alias defaulting the error to this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors surfaced by the cassette machinery.
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

    /// The HTTP client itself could not be constructed. Requests error out
    /// rather than fall back to a client with different (redirect-following)
    /// behavior.
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

    /// Discovery named an OpenAPI document somewhere other than on this
    /// server. Refused rather than followed: `Url::join` treats an absolute
    /// URL as a replacement, so honouring it would fetch a spec from a host
    /// the user never named.
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

    /// A cassette noun parsed but is not on the surface. Only reachable if the
    /// surface changed between building the parser and dispatching.
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

    /// `--body` was not JSON. Checked before sending so the failure names the
    /// quoting mistake rather than arriving as a cassette's schema error.
    #[snafu(display("--body is not valid JSON"))]
    InvalidBody {
        /// Underlying JSON failure.
        source: serde_json::Error,
    },

    /// The parsed body could not be re-rendered for sending. Only reachable
    /// if serde_json emits a value it cannot serialize back.
    #[snafu(display("could not render the request body"))]
    RenderBody {
        /// Underlying JSON failure.
        source: serde_json::Error,
    },
}
