//! Codex attribution.
//!
//! Codex writes no PID-indexed session file, so there is nothing to look up by
//! PID the way [`crate::attribution::claude`] does. Identity is recovered from
//! the rollout JSONL a live `codex` process holds open, which makes every
//! Codex lookup indirect: find the open file, read its `session_meta` row,
//! and decide whether that session is ours.
//!
//! * [`session`] — the `session_meta` row's shape, its reader, and
//!   [`session::rollout_id`], which reads off the request itself which rollout
//!   a turn belongs to.
//! * [`process`] — which `.jsonl` files a given PID currently holds open, via
//!   per-OS process-file APIs.
//! * [`watcher`] — polls the rollout directory and publishes recently-modified
//!   sessions as a wait-free snapshot, the way the Claude watcher does for its
//!   sessions directory.
//!
//! Two consequences run through the whole lane. Rollout files linger for hours
//! after a session ends, so every candidate is filtered by a recency window;
//! and one `codex` process running subagents holds the parent rollout and every
//! child rollout open at once, so neither the PID nor a launch marker
//! identifies a *thread*. Only [`session::rollout_id`] does. Where that
//! evidence is absent and several live sessions remain, the pipeline refuses
//! rather than guesses — see [`crate::attribution::pipeline`].

pub mod process;
pub mod session;
pub mod watcher;

pub use process::open_jsonl_sessions_by_pid;
pub use session::{CODEX_ROLLOUT_ID_HEADERS, CodexSessionFile, rollout_id};
pub use watcher::{
    CodexWatcherSnapshot, Snapshot as CodexWatcherSnapshotHandle, spawn as spawn_codex_watcher,
};
