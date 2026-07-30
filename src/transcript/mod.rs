//! Transcript tailer.
//!
//! tapes' wire capture yields a complete call inventory but no causal/fork
//! skeleton — that lives only in the harness's on-disk transcripts
//! (`~/.claude/projects/<cwd-encoded>/<sid>.jsonl` plus each session's
//! `subagents/` directory). The transcript lane (`POST /v1/ingest/transcript`)
//! carries them. This module is the client-side half every capture client needs:
//!
//! * [`files`] — discovery of a session's upload set and JSONL→records
//!   conversion. Extracted verbatim from paperd, which mirrors the Go reference
//!   client, so every client produces byte-identical upload sets for the same
//!   on-disk state.
//! * [`trigger`] — the push state machine: 30 s quiescence, 5 min periodic safety
//!   net, and a final push on harness exit.
//! * [`payload`] — the ingest payload shape, a cross-language contract with the
//!   Go server.
//! * [`sweep`] — a startup scan of the transcript tree, which finds sessions that
//!   ended while the client was not running. New here rather than moved: it closes
//!   a gap in paperd's registry-driven discovery.
//!
//! The seed's `Transcript { path, harness }` placeholder is gone;
//! [`files::TranscriptFile`] is the real shape, and it carries the subagent id and
//! fork metadata that a transcript upload actually needs.
//!
//! # What stays with each client
//!
//! **Delivery, auth, and retry.** The HTTP call, the request timeout, the response
//! parsing, the credential — and the failure backoff schedule — differ per client,
//! and auth differs most of all: paperd rides its own `X-Paper-Auth` channel so
//! the Paper cloud edge admits the request, which is explicitly not part of the
//! tapes contract. So does each client's notion of *which* sessions to track:
//! paperd's registry is fed by its proxy's per-request attribution, and a
//! standalone client will have a different hook.
//!
//! # Why the eager design is safe
//!
//! Every push decision here errs toward pushing again, because the ingest endpoint
//! is idempotent by construction: the server keys rows on a content hash of the
//! records array, so unchanged content answers `deduped` and a grown transcript
//! appends a new version. The fingerprint driving
//! [`trigger::TriggerInput::dirty`] is deliberately coarse (size + mtime), retries
//! re-read the files rather than buffering anything, and [`sweep`] re-offers
//! transcripts a previous process may already have sent. The transcript files on
//! disk are the spool; there is no client-side queue to lose.

pub mod files;
pub mod payload;
pub mod sweep;
pub mod trigger;

pub use files::{
    FileFingerprint, SubagentMeta, TranscriptFile, fingerprint, jsonl_to_records, session_files,
};
pub use payload::{
    INGEST_PATH, IngestEnvelope, TranscriptPayload, TranscriptSession, build_payload,
};
pub use sweep::{SweepOptions, SweptSession, sweep};
pub use trigger::{
    DEFAULT_PERIODIC, DEFAULT_QUIESCENCE, DEFAULT_TICK, PushReason, TriggerInput, TriggerPolicy,
    decide,
};
