//! Claude Code attribution.
//!
//! Claude publishes its own identity: every live `claude` process writes
//! `~/.claude/sessions/<pid>.json` and keeps it current. Attribution is
//! therefore a PID question — find which candidate PID owns the loopback
//! connection this request arrived on, then read that PID's file.
//!
//! * [`session`] — the shape of `~/.claude/sessions/<pid>.json` and its reader.
//! * [`watcher`] — polls the sessions directory every 1 s and publishes the
//!   candidate-PID set plus parsed metadata as one wait-free snapshot.
//! * [`fork_parent`] — bounded scan of `~/.claude/projects/<cwd>/*.jsonl` to
//!   recover fork-parent lineage. Callers cache the result per session id;
//!   discovery is time-budgeted, not free.
//!
//! The peer-PID lookup itself is harness-agnostic and lives one level up in
//! [`tapes_capture::peer_pid`] — Codex uses the same syscalls against a
//! different question. What is Claude-specific is only what the PID is used
//! *for*: indexing a directory Claude maintains.
//!
//! Contrast [`crate::attribution::codex`], whose harness publishes no such
//! directory and must be identified through the rollout file a live process
//! holds open. That difference — a harness that announces itself versus one
//! that must be inferred — is the reason attribution is split per harness at
//! all, and it is captured declaratively by
//! [`crate::harness::AttributionStrategy`].

pub mod fork_parent;
pub mod session;
pub mod watcher;

pub use session::{ClaudeSessionFile, default_sessions_dir};
pub use watcher::{Snapshot as WatcherSnapshotHandle, WatcherSnapshot, spawn as spawn_watcher};
