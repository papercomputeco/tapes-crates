//! `tapes-harnesses` — shared, open-source client-side harness knowledge for
//! Tapes capture.
//!
//! This crate is the single home for everything a capture client needs to know
//! about a coding-agent harness on the client side. It is consumed by both
//! `tapesctl` (open) and paperd (closed), so ingest parity between
//! `tapesctl start` and `paper start` is **structural, not policed** — the same
//! code runs in both.
//!
//! Exactly three places hold harness knowledge; this crate is one of them (the
//! deriver and the envelope spec/fixtures are the other two). It owns these
//! responsibilities:
//!
//! - [`harness`] — the registry: one declaration per harness, bundling its id,
//!   User-Agent rule, launch support, attribution strategy, transcript
//!   location, and plugin needs. The other modules take their harness ids from
//!   it and consumers derive their supported-agent lists from it, so adding a
//!   harness starts in exactly one place. See `docs/adding-a-harness.md`.
//! - [`launch`] — per-harness env/config injection to run a harness under a
//!   capture proxy.
//! - [`config`] — persistent harness-config patch grammars: how an installer
//!   patches a capture provider into a harness's *own* config file,
//!   idempotently and preserving the user's content. Where [`launch`] plans
//!   per-process config that dies with the process, this module owns the
//!   durable install a desktop app or long-lived integration needs.
//! - [`plugin`] — the artifacts a harness with no base-URL knob needs installed
//!   *into* it before capture is possible at all, and the environment contract
//!   those artifacts read. Consumers are installers; the bytes live here so
//!   every client installs the same ones.
//! - [`attribution`] — session-file reads, fork-parent recovery, peer-PID
//!   lookup, the peer-trust ancestry walk, and the codex session watcher,
//!   grouped per harness.
//! - [`transcript`] — discovering and packaging harness transcripts for the
//!   `POST /v1/ingest/transcript` lane.
//!
//! [`attribution`] is extracted from paperd's `proxy::session::*` — the code
//! that validated peer-PID attribution and fork-parent discovery against real
//! Claude and Codex traffic.
//!
//! # What is *not* here
//!
//! Everything above changes when a harness is added. The parts of capture that
//! do not live in [`tapes_capture`], which this crate depends on: the
//! `X-Tapes-*` envelope producer and its harness-id vocabulary, the
//! capture-gateway environment contract and launch-nonce protocol, peer-PID
//! lookup, and the peer-trust ancestry check. The edge runs one way by
//! construction: a harness module may reach for a capture primitive, and
//! nothing over there can reach back, because the moment a capture primitive
//! knows a harness's name it stops being the thing every harness shares.
//!
//! The envelope is the sharpest case, because the arrow points the way that
//! first looks backwards. Harness *ids* are envelope vocabulary — they are what
//! goes on the wire — so [`harness`] takes its ids from `tapes_capture` rather
//! than declaring them and having the envelope import them back. Reading them
//! the other way is what used to make the two mutually dependent, and it is why
//! the producer now asks for a `tapes_capture::HarnessSession` instead of
//! naming any harness's session type.
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
pub mod config;
pub mod harness;
pub mod launch;
pub mod plugin;
pub mod transcript;
