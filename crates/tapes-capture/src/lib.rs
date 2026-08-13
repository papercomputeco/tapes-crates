#![doc = include_str!("../README.md")]
//!
//! # Module map
//!
//! The README's seam list above says what each module is for. This is the same
//! set with the arguments that decided their boundaries, and with links.
//!
//! * [`envelope`] — the `X-Tapes-*` request-header contract: the producer that
//!   turns a resolved session identity into the on-wire header set, the header
//!   names and caps that set obeys, and the vocabulary of harness ids it
//!   stamps. It is a wire format, and a wire format that changed when a harness
//!   was added would not be one. Every id it can carry is declared here, and
//!   the harness registry takes its ids from this list rather than restating
//!   them — that direction is the point, not an accident: the envelope is the
//!   contract a harness declaration must be consistent *with*.
//! * [`gateway`] — two protocols, not one. The launch-nonce protocol names the
//!   proxy, mints a per-launch secret, and checks the header that echoes it
//!   back; the provider-route protocol lets one gateway address serve several
//!   upstream providers by labelling the path. Both are *protocol*; the plugin
//!   files written against them are artifacts and live with the harness they
//!   are installed into. Conflating the two is what let a protocol change ride
//!   along with an artifact change.
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
//!
//! # Where the envelope contract is written down
//!
//! [`envelope`] documents the producer. The contract it implements is
//! documented beside the fixture corpus that seals it, in
//! `vendor/tapes-envelope-fixtures/SOURCE.md`, and the parts a parser author
//! needs are summarised in `envelope::fixtures` (present with the
//! `envelope-fixtures` feature; docs.rs builds it) — including the rule that
//! makes several vendored copies one corpus:
//!
//! * The corpus is vendored into **every** implementation of the contract, in
//!   every language, and all copies must move together from one upstream
//!   revision. A copy that moves alone is a suite that goes green against bytes
//!   no other implementation has.
//! * `DIGEST` is what makes that checkable: sort the case files by base name,
//!   feed `"<basename>  <sha256>\n"` for each into SHA-256, and compare. The
//!   recipe is deliberately trivial so each language restates it in a few lines
//!   rather than sharing an implementation that would itself need vendoring.
//! * Cases carry a direction — `roundtrip`, `encode`, or `decode` — that says
//!   which half of the contract asserts them. A producer runs the first two and
//!   skips the third by design; a parser runs the first and third.
//!
//! # Names
//!
//! The repository is `tapes-crates`; this crate is one of its four members.
//! `tapes` is a different repository entirely — the server whose ingest reads
//! these headers back, and the authoring home of the fixture corpus vendored
//! here.
#![warn(missing_docs)]

pub mod envelope;
pub mod gateway;
pub mod peer_pid;
pub mod peer_trust;
pub mod session;

pub use gateway::{
    GATEWAY_NONCE_ENV, GATEWAY_NONCE_HEADER, GATEWAY_PROVIDER_ROUTE_PREFIX,
    GATEWAY_PROVIDER_ROUTES_ENV, GATEWAY_PROVIDER_ROUTES_ON, GATEWAY_SCHEMA_ENV, GATEWAY_URL_ENV,
    nonce_matches, provider_route, split_provider_route,
};
pub use peer_pid::{PeerPidLookup, lookup as peer_pid_lookup};
pub use peer_trust::{
    is_launched_or_descendant, peer_is_launched_harness, peer_is_launched_harness_async,
};
pub use session::HarnessSession;
