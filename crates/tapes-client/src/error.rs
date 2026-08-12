//! One taxonomy for everything that can go wrong between a named operation and
//! a decoded response.
//!
//! # Why one enum and not one per surface
//!
//! The sealed contract surface and the discovered cassette surface used to
//! carry an error type each, and the two disagreed in ways nothing checked: a
//! non-success status was a rich variant on one side and absent from the other,
//! a URL failure had two spellings, and "could not decode" meant *the bytes are
//! not JSON* in one crate and *the JSON is not the requested type* in the
//! other. A consumer wrapping both got two vocabularies for one API and had to
//! decide, per variant, whether the difference was meaningful. It never was.
//!
//! The variants below are grouped by the four things that actually happen:
//!
//! - **Contract** — a refusal. The operation, parameter, or body a caller named
//!   disagrees with the document, and nothing is sent. These are build defects
//!   at the call site, which is why they are loud and name the offender.
//! - **Transport** — the request could not be delivered or the client could not
//!   be built. Carries a [`TransportError`], which is deliberately opaque: the
//!   seam admits implementations that have never heard of HTTP.
//! - **ApiStatus** — the request arrived and the server refused it. The body
//!   travels with the status because every tapes error body names the offending
//!   parameter, and the bare status never does.
//! - **Decode** — the bytes came back and are not what was asked for.
//!
//! This enum is `#[non_exhaustive]`: it is the shared vocabulary of a growing
//! surface, and a consumer that matches it must say what it does with a
//! condition its build predates rather than fail to compile when one appears.

use snafu::Snafu;

use crate::transport::TransportError;

/// This crate's result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Anything that can go wrong driving a tapes API call.
///
/// `#[snafu(module)]` puts the generated context selectors in a nested `error`
/// module rather than at this module's root: the selectors are construction
/// detail, and a consumer matches on the variants.
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
#[non_exhaustive]
pub enum Error {
    // ---- Contract refusals: nothing was sent ----
    /// The contract embedded in this build did not parse, or reduced to
    /// nothing. Only reachable from a build whose vendored document is corrupt
    /// — this crate's own tests fail before such a build ships.
    #[snafu(display("the vendored {surface} contract embedded in this build did not parse"))]
    VendoredContract {
        /// Which contract failed.
        surface: &'static str,
    },

    /// A caller named an operation the contract does not have. A build defect
    /// wherever the coverage gate runs, which is the point of the gate.
    #[snafu(display("the vendored tapes-api contract has no operation {operation:?}"))]
    ContractOperation {
        /// The operation id that failed to resolve.
        operation: String,
    },

    /// A caller tried to send a parameter the contract does not declare on
    /// that operation. Refused rather than sent: an undeclared parameter is
    /// exactly the drift a vendored contract exists to catch, and a server
    /// that ignores an unknown query parameter would hide it.
    #[snafu(display(
        "the vendored tapes-api contract does not declare parameter {parameter:?} on {operation:?}"
    ))]
    ContractParameter {
        /// The operation being called.
        operation: String,
        /// The undeclared wire name.
        parameter: String,
    },

    /// A caller had no value for a path parameter the operation requires, so
    /// no URL can be built — the substitution would leave a literal `{id}`
    /// segment addressing nothing.
    #[snafu(display(
        "operation {operation:?} requires path parameter {parameter:?} and none was supplied"
    ))]
    ContractPathParameter {
        /// The operation being called.
        operation: String,
        /// The missing path parameter.
        parameter: String,
    },

    /// A caller had no value for a query or header parameter the contract
    /// marks required.
    ///
    /// A missing path parameter cannot produce a URL at all, so it was always
    /// refused; a missing required query parameter produces a URL that is
    /// perfectly well-formed and still not a request the contract describes.
    /// The server answers it with a 400 whose wording is its own, which is a
    /// worse error later instead of a precise one now — and on an operation
    /// whose filter is what scopes the response, a client that guessed wrong
    /// about requiredness would be asking a different question than it thinks.
    #[snafu(display(
        "operation {operation:?} requires {location} parameter {parameter:?} and none was supplied"
    ))]
    ContractRequiredParameter {
        /// The operation being called.
        operation: String,
        /// The missing parameter's wire name.
        parameter: String,
        /// Where the contract declared it: `query` or `header`.
        location: &'static str,
    },

    /// A caller's request body disagrees with what the operation declares.
    ///
    /// Both directions are refusals, and the reason is the same: a body-shaped
    /// mismatch is invisible on the wire. An operation whose `requestBody` is
    /// required, called without one, reaches the server as a syntactically
    /// fine request that means nothing; a body sent to an operation that
    /// declares none is dropped by whatever is in front of the handler. Either
    /// way the call site looks correct.
    #[snafu(display("operation {operation:?} {detail}"))]
    ContractBody {
        /// The operation being called.
        operation: String,
        /// What is wrong, phrased to complete the sentence.
        detail: &'static str,
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

    /// A spec described an operation with a verb that is not an HTTP method.
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

    // ---- URL construction ----
    /// A URL could not be built from the base and the contract's path.
    #[snafu(display("could not build the request URL"))]
    Url {
        /// Underlying parse failure.
        source: url::ParseError,
    },

    /// The base URL cannot carry a path (`mailto:`, `data:`), so no route can
    /// be joined onto it.
    #[snafu(display("the base URL cannot be a base for API paths"))]
    NotABase,

    // ---- Transport ----
    /// The request could not be delivered.
    ///
    /// The source is opaque on purpose. A transport may be an HTTP client, a
    /// local socket carrying opaque frames, or a test double, and this layer
    /// has no business naming any of their error types — that is precisely the
    /// coupling the seam exists to prevent.
    /// The source is rendered inline because a transport's refusal is often the
    /// whole diagnosis — "the server answered with a redirect" is actionable,
    /// "could not reach the tapes API" alone is not — and a consumer that
    /// prints only the top-level error would otherwise lose it.
    #[snafu(display("could not reach the tapes API: {source}"))]
    Transport {
        /// Underlying transport failure.
        source: TransportError,
    },

    /// The transport itself could not be constructed. Requests error out
    /// rather than fall back to a client with different (redirect-following)
    /// behavior.
    #[snafu(display("could not initialize the HTTP client"))]
    ClientInit,

    // ---- The server answered, and said no ----
    /// The server answered with a non-success status. The body is carried
    /// because every tapes error body names the offending parameter.
    #[snafu(display("tapes API returned {status} for {endpoint}: {body}"))]
    ApiStatus {
        /// HTTP status returned.
        status: u16,
        /// Endpoint that was called.
        endpoint: String,
        /// Response body, verbatim.
        body: String,
    },

    // ---- Decoding ----
    /// The response could not be decoded: it is not JSON, or it is JSON that
    /// is not the type the caller asked for.
    ///
    /// The second half is unreachable for the untyped instantiation — every
    /// JSON document is a [`serde_json::Value`] — so a decode failure on a
    /// caller-chosen model is visible exactly where that choice was made.
    #[snafu(display("could not decode the tapes API response"))]
    Decode {
        /// Underlying JSON failure.
        source: serde_json::Error,
    },

    // ---- Request bodies supplied by a user ----
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
