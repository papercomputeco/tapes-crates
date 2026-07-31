//! `tapes-harnesses` — shared, open-source client-side harness knowledge for
//! Tapes capture.
//!
//! This crate is the single home for everything a capture client needs to know
//! about a coding-agent harness on the client side. It is consumed by both
//! `tapesctl` (open) and paperd (closed), so ingest parity between
//! `tapesctl start` and `paper start` is **structural, not policed** — the same
//! code runs in both.
//!
//! Per the "Tapes and Cassettes" RFC, exactly three places hold harness
//! knowledge; this crate is one of them (the deriver and the envelope
//! spec/fixtures are the other two). It owns five responsibilities:
//!
//! - [`harness`] — the registry: one declaration per harness, bundling its id,
//!   User-Agent rule, launch support, attribution strategy, transcript
//!   location, and plugin needs. The other modules take their harness ids from
//!   it and consumers derive their supported-agent lists from it, so adding a
//!   harness starts in exactly one place. See `docs/adding-a-harness.md`.
//! - [`launch`] — per-harness env/config injection to run a harness under a
//!   capture proxy.
//! - [`attribution`] — session-file reads, fork-parent recovery, peer-PID
//!   lookup, and the codex session watcher, grouped per harness.
//! - [`transcript`] — discovering and packaging harness transcripts for the
//!   `POST /v1/ingest/transcript` lane.
//! - [`envelope`] — the `X-Tapes-*` header contract that carries attribution
//!   from any capture transport into ingest.
//!
//! [`attribution`] and [`envelope`] are extracted from paperd's
//! `proxy::session::*` and `proxy::headers` — the code that validated peer-PID
//! attribution, fork-parent discovery, and the `X-Tapes-*` producer against
//! real Claude and Codex traffic. The envelope's on-wire behaviour is pinned
//! by the shared cross-language fixture corpus vendored at
//! `vendor/tapes-envelope-fixtures/`, which the Go parsers table-test against
//! too, so producer and parser cannot drift silently.
//!
//! [`launch`] is extracted from paper's `cli/start.rs` per-agent env/config
//! injection, with the Go `tapes start` opencode/codex knowledge folded in —
//! including opencode, which paper never supported. Its recipes are pure: they
//! plan argv, environment, and config documents, and the consumer owns process
//! spawning and cleanup.
//!
//! [`transcript`] is extracted from paperd's transcript uploader — its
//! discovery/packaging half, the push trigger, and the ingest payload shape —
//! and adds a startup sweep of the transcript tree, which closes paperd's own
//! gap: a session that began and ended while the daemon was down is never
//! re-registered by live traffic, so its fork skeleton was previously lost.
//! Delivery, auth, and retry stay in each client.

pub mod attribution;
pub mod envelope;
pub mod harness;
pub mod launch;
pub mod transcript;
