//! The tapes read contract, and the machinery that drives requests from it.
//!
//! # What this crate is
//!
//! The tapes read API is a *published contract*: sealed in the tapes repository
//! (`api/CONTRACT`) and attached to releases as an OpenAPI document. Clients do
//! not build against the tapes working tree — they build against a published
//! release asset. This crate vendors that asset once, so the system holds one
//! copy rather than one per client, and turns it into requests:
//!
//! - [`contract`] — the vendored document, reduced to an operation table;
//!   resolve an operation by its `operationId`, route values by the location
//!   the document declared for each, and refuse a parameter it never declared.
//! - [`invoke`] — the URL builder, in the two path modes a root-mounted server
//!   and a gateway-prefixed one respectively need.
//! - [`transport`] — the seam a consumer plugs its own HTTP client into, and
//!   the generic call surface over it.
//! - [`coverage`] — the gate that fails a build when a contract bump adds an
//!   operation the client neither exposes nor deliberately allow-lists.
//!
//! # What this crate is not
//!
//! It has no HTTP client, no authentication, no notion of a tenant, and no
//! opinion about how a response is rendered — or, beyond the policy recorded in
//! [`transport`], about whether a response is typed at all. Every one of those
//! is a consumer's, and each consumer's answer is different.
//!
//! It also does not hold the coverage tables. Those describe one client's
//! surface; see [`coverage`] for why sharing them would break the gate.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod contract;
pub mod coverage;
pub mod error;
pub mod invoke;
pub mod transport;

pub use contract::{CoreSurface, TAPES_API_YAML, call_for, core, ops};
pub use error::{Error, Result};
pub use invoke::{PathMode, call_url};
pub use transport::{ReadOperations, ReadTransport};

// Re-exported so a consumer naming a `Call` does not have to depend on the
// cassette crate directly just to spell the type this one hands it.
pub use tapes_cassette_client::Call;
