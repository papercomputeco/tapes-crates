//! The sealed surface: operations known at build time.
//!
//! The tapes read API is a *published contract*, sealed in the tapes repository
//! and attached to releases as an OpenAPI document. Clients do not build
//! against the tapes working tree — they build against a published release
//! asset. This crate vendors that asset once, in `contracts/tapes-api.yaml`,
//! pinned by fingerprint (see `contracts/PROVENANCE.md`), and turns it into
//! requests.
//!
//! - [`contract`] — the vendored document, reduced to an operation table.
//! - [`coverage`] — the gate that fails a build when a contract bump adds an
//!   operation the client neither exposes nor deliberately allow-lists.
//! - [`methods`] — the call surface over a [`crate::transport::TapesTransport`].
//!
//! This module is one half of a symmetry: [`crate::cassettes`] is the same
//! machinery over an operation table discovered at runtime. Both are thin —
//! everything that could drift between them lives in the shared floor
//! ([`crate::transport`], [`crate::error`], [`crate::decode`], [`crate::page`],
//! [`crate::path`]), so a sealed call and a discovered call differ only in
//! where their operation table came from.

pub mod contract;
pub mod coverage;
pub mod methods;

pub use contract::{CoreSurface, TAPES_API_YAML, call_for, call_for_with_body, core, ops};
pub use methods::CoreClient;
