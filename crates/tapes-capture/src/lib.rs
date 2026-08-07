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
//!
//! Together the last two are the question every capture client asks before it
//! believes anything a connection tells it about itself, and neither has ever
//! needed a harness id to answer it.

pub mod gateway;
pub mod peer_pid;
pub mod peer_trust;

pub use gateway::{
    GATEWAY_NONCE_ENV, GATEWAY_NONCE_HEADER, GATEWAY_SCHEMA_ENV, GATEWAY_URL_ENV, nonce_matches,
};
pub use peer_pid::{PeerPidLookup, lookup as peer_pid_lookup};
pub use peer_trust::{
    is_launched_or_descendant, peer_is_launched_harness, peer_is_launched_harness_async,
};
