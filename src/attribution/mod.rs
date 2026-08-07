//! Session attribution.
//!
//! Attribution is how a captured harness session acquires a real identity —
//! session id, fork parent, cwd, and the acting subject — rather than a
//! synthetic root hash. This module is the extracted form of paperd's
//! `proxy::session::*`, which validated peer-PID attribution and fork-parent
//! discovery against real Claude and Codex traffic; it now backs both
//! `tapesctl start` and `paper start`, so the two capture paths attribute
//! identically by construction rather than by review.
//!
//! # Layout
//!
//! Harness-specific knowledge lives in a submodule named after its harness;
//! everything else is shared:
//!
//! * [`claude`] — the sessions-directory lane: session file, watcher, and
//!   fork-parent recovery.
//! * [`codex`] — the open-rollout lane: `session_meta` reader, per-PID open
//!   files, and the rollout watcher.
//! * [`codex_app`] — the Codex desktop app's lifecycle-hook lane: the parsed,
//!   allowlisted shape of the hook payloads an installed plugin's command
//!   receives. The app shares Codex's wire protocol and rollout tree but is
//!   configured rather than launched, so its identity evidence arrives at
//!   lifecycle boundaries instead of through a peer-PID lookup.
//! * [`peer_pid`] and [`peer_trust`] — harness-agnostic, and therefore no
//!   longer defined here: they live in `tapes-capture` and are re-exported at
//!   these paths. `peer_pid` maps an accepted loopback connection to one of a
//!   candidate PID set via per-OS kernel APIs; `peer_trust` walks the process
//!   tree to decide whether that peer is the launched harness or one of its
//!   descendants. Both lanes use them, and so would a lane for a harness that
//!   does not exist yet — which is exactly why neither belongs in a crate
//!   whose contents change when a harness is added.
//! * [`pipeline`] — the composition: [`attribute`] takes one request's facts
//!   and returns one [`Attributed`] outcome, driving the primitives in the
//!   order that was validated against real traffic. Capture clients call this;
//!   the primitives are exposed for tests and for clients with unusual needs.
//!   A third re-implementation of this sequence is the design smell this
//!   module exists to prevent.
//!
//! The split is not cosmetic: the two harnesses answer "who sent this?" in
//! fundamentally different ways — Claude publishes a PID-indexed file, Codex
//! must be inferred from a file a live process holds open — and a flat module
//! list hid which of the generically-named pieces belonged to which. Which
//! shape a harness has is declared once in [`crate::harness`]; a harness that
//! needs neither lane (it attributes itself, like `pi`) contributes no module
//! here at all.
//!
//! Every lookup here is best-effort and time-budgeted: an absent field means
//! "unknown", never a sentinel. A capture client that cannot attribute a
//! request still emits a well-formed envelope (see [`crate::envelope`]) — it
//! just marks the harness `unknown`.

pub mod claude;
pub mod codex;
pub mod codex_app;
pub mod pipeline;

// `peer_pid` and `peer_trust` now live in `tapes-capture`: neither has ever
// needed a harness id to answer its question, so neither changes when a harness
// is added. They are re-exported at their original paths because a consumer
// pinning this crate by git rev should not have to move in lockstep with an
// internal boundary; the canonical spelling is `tapes_capture::…`.
pub use tapes_capture::{peer_pid, peer_trust};

// --- compatibility aliases ---------------------------------------------
//
// The pre-reorg flat paths, kept working so a consumer pinning this crate by
// git rev is not forced to move in lockstep with it. `attribution::watcher`
// really was the *Claude* watcher despite its generic name — that ambiguity is
// what the reorg removes, so prefer the canonical spelling on the right in new
// code.

pub use claude::fork_parent;
pub use claude::session as claude_session;
pub use claude::watcher;
pub use codex::process as codex_process;
pub use codex::session as codex_session;
pub use codex::watcher as codex_watcher;

// --- flattened re-exports ----------------------------------------------
//
// Unchanged from before the reorg: the names a capture client reaches for
// most, hoisted so the common case is one `use`.

pub use claude::{ClaudeSessionFile, default_sessions_dir};
pub use codex::request as codex_request;
pub use codex::{
    CODEX_ROLLOUT_ID_HEADERS, CodexHookEvidence, CodexRequestIdentity, CodexSelection,
    CodexSelectionEvidence, CodexSelectionResult, CodexSessionFile, CodexWatcherSnapshot,
    CodexWatcherSnapshotHandle, open_jsonl_sessions_by_pid, rollout_id as codex_rollout_id,
    spawn_codex_watcher,
};
pub use peer_pid::{PeerPidLookup, lookup as peer_pid_lookup};
pub use peer_trust::{
    is_launched_or_descendant, peer_is_launched_harness, peer_is_launched_harness_async,
};
pub use pipeline::{
    Attributed, AttributionConfig, AttributionOutcome, AttributionState, CodexProviderFilter,
    ForkParentCache, RequestFacts, UserAgentHarness, attribute, attribute_with_evidence,
};
pub use watcher::{Snapshot as WatcherSnapshotHandle, WatcherSnapshot, spawn as spawn_watcher};

/// Attribution facts discovered for a captured harness session.
///
/// Fields are optional because discovery is best-effort and time-budgeted; an
/// absent field means "unknown", never a sentinel.
///
/// This is the harness-agnostic summary a capture client carries once the
/// per-harness lookups above have run. `auth_subject` has no equivalent in the
/// per-harness session files: standalone clients default it to
/// `local:<os-username>` and allow an override (agents and CI set e.g.
/// `gardener-ci`), while on the platform the cloud edge stamps it from
/// validated JWT claims. Nothing parses the prefix — it is an opaque
/// attribution string, the same envelope field in both worlds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// The harness's own session identifier, if recovered.
    pub session_id: Option<String>,
    /// The parent session id for a forked/resumed session, if recovered.
    pub parent_session_id: Option<String>,
    /// The working directory the harness was launched in.
    pub cwd: Option<String>,
    /// The acting subject (`local:<user>` standalone; gateway-stamped on the
    /// platform). Empty/None is allowed.
    pub auth_subject: Option<String>,
}
