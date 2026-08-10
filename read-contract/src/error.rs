//! What can go wrong between a named operation and a request.
//!
//! Every variant here is refusal, not failure: the contract has no such
//! operation, does not declare that parameter, or cannot produce a URL from
//! the values given. A consumer maps these onto its own error type — see the
//! crate docs — so its users keep reading its wording, not this crate's.

use snafu::Snafu;

/// This crate's result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A contract-layer failure.
///
/// `#[snafu(module)]` puts the generated context selectors in a private
/// `error` module rather than at this module's root, matching the sibling
/// crates: the selectors are construction detail, and a consumer matches on
/// the variants.
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
#[non_exhaustive]
pub enum Error {
    /// The vendored contract embedded in this build did not parse, or reduced
    /// to nothing. Only reachable from a build whose vendored document is
    /// corrupt — this crate's own tests fail before such a build ships.
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

    /// A response could not be decoded into the type the caller asked for.
    ///
    /// Unreachable for the untyped instantiation — every JSON document is a
    /// `serde_json::Value` — so this only fires where a consumer chose a typed
    /// model, which is exactly where it should be visible.
    #[snafu(display("could not decode the tapes API response into the requested type"))]
    Decode {
        /// Underlying JSON failure.
        source: serde_json::Error,
    },
}
