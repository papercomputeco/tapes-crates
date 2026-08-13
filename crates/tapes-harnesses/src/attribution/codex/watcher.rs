//! Polls Codex JSONL transcripts and exposes recent `session_meta` rows.
//!
//! Codex, unlike Claude, writes no per-process session file — a rollout is
//! named for when it started, not for the PID that owns it. There is therefore
//! no lookup that answers "which rollout is this process?", and this module
//! does not pretend otherwise: it maintains a *candidate set*, and the
//! disambiguation policy in [`super::select`] decides whether that set names a
//! session exactly enough to attribute a request to it. A set of one is an
//! answer; a set of several is a refusal, not a guess.
//!
//! # Shape
//!
//! [`spawn`] starts a background task and hands back a [`Snapshot`] — a shared
//! cell the task replaces once a second with a freshly scanned
//! [`CodexWatcherSnapshot`]. Readers never block on the scan and never see a
//! partially built set; they load the current snapshot and are done with it.
//!
//! Two bounds keep the scan from growing without limit. Rollouts older than 24
//! hours are dropped, because a request cannot belong to a session that ended
//! yesterday and keeping them only enlarges the ambiguity a caller has to
//! resolve. And the scan itself runs on a blocking thread rather than a runtime
//! worker: it is directory iteration plus a read per file, which is exactly the
//! shape that stalls every other future a worker owns.
//!
//! The task exits on its own when the last snapshot holder drops, so a consumer
//! stops it by dropping its handle rather than by calling anything.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use time::OffsetDateTime;

use super::session::{CodexSessionFile, read};

const RETENTION: time::Duration = time::Duration::hours(24);

/// The rollouts one scan found, as a single immutable value.
///
/// Replaced wholesale rather than mutated, which is what lets a reader hold one
/// for as long as it likes: the set it examines cannot change underneath it
/// mid-decision.
#[derive(Debug, Default)]
pub struct CodexWatcherSnapshot {
    /// Rollouts whose `session_meta` parsed and whose start is within the
    /// retention window, in no guaranteed order.
    ///
    /// Unordered on purpose — ordering is a policy question, and the two orders
    /// that matter (newest start, most recently written) belong to whichever
    /// rung of [`super::select`] is asking. A rollout that failed to parse is
    /// absent rather than represented, because a candidate nothing can be
    /// learned from can only widen an ambiguity.
    pub sessions: Vec<CodexSessionFile>,
}

/// A handle to the watcher's current [`CodexWatcherSnapshot`].
///
/// Read it with `load()`. Cloning the handle is cheap and gives another reader
/// of the same cell — it does not start a second watcher. The background task
/// holds only a weak reference, so dropping every clone is what stops it.
pub type Snapshot = Arc<ArcSwap<CodexWatcherSnapshot>>;

/// Scan `sessions_dir` now, then keep scanning it once a second in the
/// background.
///
/// Returns as soon as the *first* scan completes, so the handle is already
/// populated when a caller gets it — a watcher that returned empty and filled
/// in a second later would make the first request of a session unattributable
/// for no reason.
///
/// `sessions_dir` is Codex's rollout root: `$CODEX_HOME/sessions` when that is
/// set, otherwise `~/.codex/sessions`. The tree beneath it is walked in full,
/// since Codex nests rollouts by date.
///
/// # Panics
///
/// Does not panic, but **must be called from within a tokio runtime** — it
/// spawns. A client that builds its proxy inside the runtime it later serves on
/// satisfies this without doing anything extra.
///
/// A scan that fails is logged and skipped, leaving the previous snapshot in
/// place: a transient read error should cost a second of freshness, not the
/// whole candidate set.
#[must_use]
pub fn spawn(sessions_dir: PathBuf) -> Snapshot {
    let initial = scan(&sessions_dir);
    let snapshot: Snapshot = Arc::new(ArcSwap::from_pointee(initial));

    let weak = Arc::downgrade(&snapshot);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(slot) = weak.upgrade() else {
                break;
            };
            let dir = sessions_dir.clone();
            let next = match tokio::task::spawn_blocking(move || scan(&dir)).await {
                Ok(next) => next,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "codex-session-watcher: scan task failed",
                    );
                    continue;
                }
            };
            slot.store(Arc::new(next));
        }
    });

    snapshot
}

fn scan(dir: &Path) -> CodexWatcherSnapshot {
    let cutoff = OffsetDateTime::now_utc() - RETENTION;
    let mut out = CodexWatcherSnapshot::default();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(next_dir) = stack.pop() {
        let entries = match std::fs::read_dir(&next_dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::debug!(
                    dir = %next_dir.display(),
                    error = %err,
                    "codex-session-watcher: could not read sessions dir",
                );
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext| ext != "jsonl")
            {
                continue;
            }
            let Some(session) = read(&path) else {
                continue;
            };
            if session.timestamp >= cutoff || session.modified_at.is_some_and(|ts| ts >= cutoff) {
                out.sessions.push(session);
            }
        }
    }

    out.sessions.sort_by_key(|session| session.timestamp);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn scan_finds_nested_recent_jsonl_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("2026").join("06").join("15");
        std::fs::create_dir_all(&day).unwrap();
        let now = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        std::fs::write(
            day.join("rollout-test.jsonl"),
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"sid-1","timestamp":"{now}","cwd":"/tmp","model_provider":"paper-openai"}}}}"#
            ),
        )
        .unwrap();

        let got = scan(dir.path());
        assert_eq!(got.sessions.len(), 1);
        assert_eq!(got.sessions[0].session_id, "sid-1");
    }
}
