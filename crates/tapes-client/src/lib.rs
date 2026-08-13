#![doc = include_str!("../README.md")]
//!
//! # Module map
//!
//! The README above is the manual: the design rule, the two surfaces, the
//! feature table, how to authenticate, and the migration tables from the two
//! crates this one absorbed. This is the same set of seams as links.
//!
//! **The floor**, one implementation each and shared by both surfaces:
//! [`transport`] (the seam), [`error`] (one taxonomy), [`decode`] (one policy),
//! [`page`] (one cursor convention), [`path`] (one join, two modes).
//!
//! **The two surfaces:** [`core`] is the sealed contract — [`core::contract`]
//! reduces the vendored document to an operation table, [`core::coverage`]
//! gates operations, [`core::models`] holds the shapes with
//! [`core::models::coverage`] gating them, and [`core::CoreClient`] is the call
//! surface. [`cassettes`] is the discovered equivalent.
//!
//! **Behind features:** [`cli`] synthesizes clap commands from a discovered
//! surface; [`http`] is the HTTP engine plus [`http::HttpAuth`], the credential
//! hook that replaced whole-transport implementations.
//!
//! Two gates are easy to conflate and fail differently. [`core::coverage`]
//! gates *operations*: it fails a build when a contract bump adds an operation
//! the client neither exposes nor allow-lists. [`core::models::coverage`] gates
//! *shapes*: it synthesises a document from each schema, round-trips it through
//! the model, and names by path anything the model drops. An operation gap is a
//! call you cannot make; a shape gap is a field you silently lose.
//!
//! # Names
//!
//! The repository is `tapes-crates`; this crate is one of its four members.
//! `tapes` is a different repository entirely — the server this client reads
//! from. Note also that [`core`](mod@crate::core) is a module here as well as a
//! Rust crate, and that [`cassettes::invoke`](mod@crate::cassettes::invoke)
//! names both a module and a function inside it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cassettes;
pub mod core;
pub mod decode;
pub mod error;
pub mod page;
pub mod path;
pub mod transport;

#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "direct-http")]
pub mod http;

pub use error::{Error, Result};
pub use page::Page;
pub use path::{PathMode, call_url};
pub use transport::{
    Call, SpecFetch, SpecTransport, StreamingTransport, TapesTransport, TransportError, Wire,
    WireRequest, WireResponse,
};

pub use crate::cassettes::{
    CacheConfig, Cassette, Discovery, DiscoveryEntry, Location, Method, Param, ReducerConfig,
    Surface,
};
pub use crate::core::{ContractModel, CoreClient, CoreSurface, TAPES_API_YAML, models, ops};

#[cfg(feature = "direct-http")]
pub use http::{DirectHttp, HttpAuth, HttpEngine, NoAuth, Rejected, Unauthorized};
