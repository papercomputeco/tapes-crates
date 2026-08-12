//! `tapes-cassette-client` — shared client-side machinery for the generated
//! tapes cassette surface.
//!
//! A tapes deployment serves *cassettes*: independently built API extensions
//! that core reverse-proxies under `/v1/cassettes/<name>`. This crate turns
//! the set a server actually serves into CLI subcommands, so a client can
//! offer `<cassette> <method>` against a server whose cassettes the binary
//! has never heard of. Extracted from `tapesctl`, which remains the reference
//! consumer; the machinery lives here so every capture client generates the
//! same surface from the same document the same way.
//!
//! # Generated at runtime, not at build time
//!
//! "Generated" here means *discovered when the process starts*, not *code
//! emitted by a build script*. That is forced by the contract on both ends:
//!
//! - **The cassette set is deployment configuration.** An operator lists
//!   cassette OpenAPI URLs (`cassettes = [...]`, `TAPES_CASSETTES`,
//!   `--cassettes`), and core fetches and admits them at runtime; nothing about
//!   the set is known to core at *its* build time, let alone to a client's.
//!   Clients ship as prebuilt binaries, so a compiled-in list would be one
//!   deployment's cassettes frozen into every user's install — and the users
//!   most likely to run a custom cassette are exactly the ones a stale list
//!   would fail.
//! - **Discovery is shaped for polling clients.** `/v1/cassettes` references
//!   each OpenAPI document rather than inlining it, and publishes a digest
//!   precisely so a client can decide whether a fetch is worth making. The
//!   per-cassette route answers `If-None-Match` with a 304, and keeps serving a
//!   cached document while the cassette itself is down. None of that machinery
//!   has a purpose if the consumer is a code generator run once.
//! - **Build-time generation would put a live server in the build graph.**
//!   The consumers build under Nix and cross-compile through Dagger; a
//!   `cargo build` that must reach a running tapes API to emit its CLI is not
//!   a build that reproduces.
//!
//! # The module map
//!
//! - [`discovery`] — the serde model of `GET /v1/cassettes`, tolerant of
//!   fields it does not act on.
//! - [`spec`] — the reducer from an OpenAPI document to the five things a CLI
//!   needs per operation. Consumers parameterize it with their reserved flag
//!   names via [`spec::ReducerConfig`].
//! - [`cache`] — the per-server on-disk surface cache, named and aged by the
//!   consumer via [`cache::CacheConfig`] so existing installs' paths and file
//!   formats do not move.
//! - [`command`] — clap command synthesis from a surface, and
//!   [`command::resolve_invocation`] back from a parse. Executing and printing
//!   stay with the consumer.
//! - [`invoke`] — [`Call`], and building its URL with per-segment
//!   percent-encoding.
//! - [`transport`] — the [`SpecTransport`] seam the cache fetches through, and
//!   [`DirectHttp`], a no-auth, no-redirect implementation.
//!
//! # Failure is not fatal
//!
//! Every step degrades instead of failing: no server configured, an
//! unreachable one, a spec that does not parse — each costs the cassette nouns
//! and nothing else. A consumer's hand-written surface must keep working on a
//! machine that cannot reach any tapes server at all.

pub mod cache;
pub mod command;
pub mod discovery;
pub mod error;
pub mod invoke;
pub mod spec;
pub mod transport;

pub use cache::CacheConfig;
pub use discovery::{Discovery, DiscoveryEntry};
pub use error::{Error, Result};
pub use invoke::Call;
pub use spec::{Cassette, Location, Method, Param, ReducerConfig, Surface};
pub use transport::{DirectHttp, SpecFetch, SpecTransport};
