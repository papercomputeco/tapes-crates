//! `tapes-capture` — the half of client-side capture knowledge that does not
//! know which harness produced a turn.
//!
//! # The membership test
//!
//! A module belongs here when **adding one more harness would not change it**.
//! Everything that fails that test — the registry, launch recipes, config patch
//! grammars, plugin artifacts, and the per-harness attribution lanes — lives in
//! the harness crate instead, which depends on this one.
//!
//! The dependency runs exactly one way. A harness module may reach for a
//! capture primitive; nothing here may reach back, because the moment a capture
//! primitive knows a harness's name it stops being the thing every harness
//! shares. Cargo enforces that now rather than review.
//!
//! # What is here
//!
//! * [`envelope`] — the `X-Tapes-*` request-header contract: the producer that
//!   turns a resolved session identity into the on-wire header set, the header
//!   names and caps that set obeys, and the vocabulary of harness ids it
//!   stamps. It is a wire format, and a wire format that changed when a harness
//!   was added would not be one. Every id it can carry is declared here, and
//!   the harness registry takes its ids from this list rather than restating
//!   them — that direction is the point, not an accident: the envelope is the
//!   contract a harness declaration must be consistent *with*.
//! * [`gateway`] — the capture-gateway environment contract and the launch-nonce
//!   protocol: the variables that name the proxy, the per-launch secret, the
//!   header that echoes it back, and the constant-time match. This is the
//!   *protocol*; the plugin files written against it are artifacts and live with
//!   the harness they are installed into. Conflating the two is what let a
//!   protocol change ride along with an artifact change.
//! * [`peer_pid`] — maps an accepted loopback connection to one of a candidate
//!   PID set via per-OS kernel APIs.
//! * [`peer_trust`] — the ancestry walk that answers whether the process on the
//!   other end of a connection is the harness this client launched, or one of
//!   its descendants.
//! * [`session`] — [`HarnessSession`], the trait a harness crate implements to
//!   describe one of its sessions to the envelope producer. It is the shape of
//!   the boundary rather than a primitive: stating what is needed, so nothing
//!   here has to import a supplier of it.
//!
//! [`peer_pid`] and [`peer_trust`] together are the question every capture
//! client asks before it believes anything a connection tells it about itself,
//! and neither has ever needed a harness id to answer it.

pub mod envelope;
pub mod gateway;
pub mod peer_pid;
pub mod peer_trust;
pub mod session;

pub use gateway::{
    GATEWAY_NONCE_ENV, GATEWAY_NONCE_HEADER, GATEWAY_SCHEMA_ENV, GATEWAY_URL_ENV, nonce_matches,
};
pub use peer_pid::{PeerPidLookup, lookup as peer_pid_lookup};
pub use peer_trust::{
    is_launched_or_descendant, peer_is_launched_harness, peer_is_launched_harness_async,
};
pub use session::HarnessSession;
