//! One client for the whole tapes read surface.
//!
//! # What this crate is
//!
//! A tapes deployment answers two kinds of question. Some operations are a
//! *published contract*: sealed in the tapes repository, attached to releases
//! as an OpenAPI document, and known to a client at build time. Others belong
//! to *cassettes* — independently built API extensions that core
//! reverse-proxies, whose set is deployment configuration and is therefore
//! discovered when a process starts.
//!
//! They are different facts about a server, and they used to be different
//! crates. That was the mistake this crate corrects. The two surfaces need the
//! same things — send a request, read a status, decode a body, follow a cursor,
//! join a path onto a base — and each answer that got written twice drifted:
//! two error vocabularies for one API, two spellings of a URL failure, a
//! non-success status that was rich on one side and absent on the other, and a
//! conditional fetch that one path could not express at all.
//!
//! # The shape
//!
//! ```text
//! transport ── the seam: one trait, request in, status + bytes out
//! error ────── one taxonomy: Contract / Transport / ApiStatus / Decode
//! decode ───── one policy: bytes to document, document to caller's type
//! page ─────── one cursor convention
//! path ─────── one join, in the two modes deployments actually need
//!    │
//!    ├── core/ ────── the SEALED surface, table from the vendored contract
//!    └── cassettes/ ─ the DISCOVERED surface, table from a live document
//! ```
//!
//! **The design rule:** [`core`] and [`cassettes`] are thin method tables.
//! Everything that could drift lives once in the floor above them. A sealed
//! call and a discovered call go through the identical pipeline; the only
//! difference is where the operation table came from. When that stops being
//! true, the crate has stopped doing its job.
//!
//! # What is not here
//!
//! No authentication, no notion of a tenant, no opinion about how a response is
//! rendered, and — beyond [`decode`]'s policy — no view on whether a response
//! is typed at all. Each of those is a consumer's, and each consumer's answer
//! is different. The one transport this crate does ship, [`http::DirectHttp`],
//! is behind a feature and carries no credential.
//!
//! It also does not hold the coverage tables. Those describe one client's
//! surface; see [`core::coverage`] for why sharing them would break the gate.
//!
//! # Features
//!
//! - `cli` (default) — the generated clap surfaces. A consumer embedding this
//!   in a GUI takes `--no-default-features` and never compiles clap.
//! - `direct-http` (default) — [`http::DirectHttp`], an unauthenticated
//!   transport for one tapes server.

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
pub use crate::core::{CoreClient, CoreSurface, TAPES_API_YAML, ops};

#[cfg(feature = "direct-http")]
pub use http::DirectHttp;
