//! The per-session push trigger.
//!
//! Extracted from paperd's `transcript_upload::uploader`. A client evaluates
//! every tracked session on a timer and asks [`decide`] whether to push that
//! session's transcript files now. Three things can trigger a push:
//!
//! * **Quiescence** — files changed since the last successful push and the newest
//!   mtime is at least [`TriggerPolicy::quiescence`] old. The common case: a turn
//!   finishes, the harness stops writing, the transcript lands half a minute
//!   later.
//! * **Periodic safety net** — files changed and at least
//!   [`TriggerPolicy::periodic`] has passed since the last push (or since the
//!   session was first seen). Covers sessions that write continuously and never
//!   quiesce.
//! * **Exit** — the harness process is gone. Final push, then the client retires
//!   the session.
//!
//! # Why this is safe to get wrong in the eager direction
//!
//! The trigger only ever decides *when* to push, never *whether* the data is
//! wanted, because the transcript-ingest endpoint is idempotent by construction:
//! the server keys rows on a content hash of the records array, so re-pushing
//! unchanged content answers `deduped` and a grown transcript appends a new
//! version. That makes every retry, every duplicate tick, and every push after a
//! client restart safe. The design leans on that instead of client-side
//! cleverness — which is also why the fingerprint feeding
//! [`TriggerInput::dirty`] is deliberately coarse (size + mtime): any drift
//! re-pushes, and the server sorts it out.
//!
//! # What stays with the client
//!
//! Failure backoff. A client that cannot reach ingest must not hammer it, but
//! *how long* to wait, how many times, and when to give up depend on how that
//! client authenticates and what it can fall back to — so the client owns the
//! backoff schedule and merely reports the result through
//! [`TriggerInput::in_backoff`], which gates everything.

use std::time::Duration;

/// Default idle window after the last transcript write before a push.
pub const DEFAULT_QUIESCENCE: Duration = Duration::from_secs(30);

/// Default safety-net push interval for sessions that never quiesce.
pub const DEFAULT_PERIODIC: Duration = Duration::from_secs(300);

/// Suggested cadence for evaluating the trigger.
///
/// Not consumed by [`decide`] — a client owns its own timer — but part of the
/// machine's design: at a 5 s tick a 30 s quiescence window resolves within
/// 35 s, which is the latency budget the window was chosen against. A much
/// coarser tick silently widens that budget.
pub const DEFAULT_TICK: Duration = Duration::from_secs(5);

/// Timing thresholds for [`decide`].
///
/// Separated from a client's own configuration — endpoints, credentials, backoff
/// schedule — so the state machine can be exercised with synthetic values and so
/// the two clients cannot drift on the thresholds that decide when a transcript
/// becomes visible in tapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerPolicy {
    /// Idle window after the last transcript write before a push.
    pub quiescence: Duration,
    /// Safety-net push interval for never-quiescent sessions.
    pub periodic: Duration,
}

impl Default for TriggerPolicy {
    fn default() -> Self {
        Self {
            quiescence: DEFAULT_QUIESCENCE,
            periodic: DEFAULT_PERIODIC,
        }
    }
}

/// Why a push fired. Clients log this with every upload batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushReason {
    /// The harness stopped writing for [`TriggerPolicy::quiescence`].
    Quiescence,
    /// [`TriggerPolicy::periodic`] elapsed with changes still unpushed.
    Periodic,
    /// The harness process is gone; this is the final push.
    Exit,
}

impl PushReason {
    /// Stable lower-case label for logs and metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quiescence => "quiescence",
            Self::Periodic => "periodic",
            Self::Exit => "exit",
        }
    }
}

/// Pure inputs to the per-session trigger decision, separated from the IO that
/// gathers them so the state machine is unit-testable with synthetic values.
#[derive(Debug, Clone, Copy)]
pub struct TriggerInput {
    /// Any file's fingerprint differs from the last successful push (including
    /// files never pushed). See [`super::files::fingerprint`].
    pub dirty: bool,
    /// The harness process behind this session is no longer running it.
    pub exited: bool,
    /// Age of the newest mtime across the session's files. `None` when no files
    /// exist yet — a session can be attributed on the wire before its first
    /// transcript flush.
    pub idle_for: Option<Duration>,
    /// Time since the last successful push, falling back to time since the
    /// session was first tracked, so the periodic net has a baseline before the
    /// first push.
    pub since_last_push: Duration,
    /// A previous push failed and its backoff window is still open. The client
    /// owns the schedule; this is only its verdict.
    pub in_backoff: bool,
}

/// The trigger state machine.
///
/// Order matters, and each step earns its position: backoff gates everything (a
/// failing endpoint must not be hammered on every tick), a clean session is
/// never pushed (there is nothing new to say), exit outranks the timers (the
/// final state should land promptly rather than waiting out a window the harness
/// will never close), then quiescence, then the periodic net.
#[must_use]
pub fn decide(policy: &TriggerPolicy, input: &TriggerInput) -> Option<PushReason> {
    if input.in_backoff {
        return None;
    }
    if !input.dirty {
        return None;
    }
    if input.exited {
        return Some(PushReason::Exit);
    }
    if input.idle_for.is_some_and(|idle| idle >= policy.quiescence) {
        return Some(PushReason::Quiescence);
    }
    if input.since_last_push >= policy.periodic {
        return Some(PushReason::Periodic);
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A live, dirty, just-written session: the baseline every case below
    /// perturbs one field of.
    fn base_input() -> TriggerInput {
        TriggerInput {
            dirty: true,
            exited: false,
            idle_for: Some(Duration::from_secs(0)),
            since_last_push: Duration::from_secs(0),
            in_backoff: false,
        }
    }

    /// Carried over from paperd's `decide_clean_session_never_pushes`.
    #[test]
    fn clean_session_never_pushes() {
        let input = TriggerInput {
            dirty: false,
            exited: false,
            idle_for: Some(Duration::from_secs(3600)),
            since_last_push: Duration::from_secs(3600),
            ..base_input()
        };
        assert_eq!(decide(&TriggerPolicy::default(), &input), None);
    }

    /// Carried over from paperd's `decide_quiescence_fires_after_idle_window`.
    #[test]
    fn quiescence_fires_after_idle_window() {
        let policy = TriggerPolicy::default();
        let input = TriggerInput {
            idle_for: Some(Duration::from_secs(31)),
            ..base_input()
        };
        assert_eq!(decide(&policy, &input), Some(PushReason::Quiescence));
        // Still writing → no push yet.
        let busy = TriggerInput {
            idle_for: Some(Duration::from_secs(5)),
            ..base_input()
        };
        assert_eq!(decide(&policy, &busy), None);
    }

    /// Carried over from paperd's
    /// `decide_periodic_fires_for_never_quiescent_session`.
    #[test]
    fn periodic_fires_for_never_quiescent_session() {
        let input = TriggerInput {
            idle_for: Some(Duration::from_secs(2)),
            since_last_push: Duration::from_secs(301),
            ..base_input()
        };
        assert_eq!(
            decide(&TriggerPolicy::default(), &input),
            Some(PushReason::Periodic),
        );
    }

    /// Carried over from paperd's `decide_exit_outranks_timers`.
    #[test]
    fn exit_outranks_timers() {
        let input = TriggerInput {
            exited: true,
            idle_for: Some(Duration::from_secs(0)),
            ..base_input()
        };
        assert_eq!(
            decide(&TriggerPolicy::default(), &input),
            Some(PushReason::Exit),
        );
    }

    /// Carried over from paperd's `decide_backoff_gates_everything`.
    #[test]
    fn backoff_gates_everything() {
        let input = TriggerInput {
            exited: true,
            idle_for: Some(Duration::from_secs(3600)),
            since_last_push: Duration::from_secs(3600),
            in_backoff: true,
            ..base_input()
        };
        assert_eq!(decide(&TriggerPolicy::default(), &input), None);
    }

    /// The thresholds are exactly inclusive: a window is satisfied *at* its
    /// boundary, not one tick after. Worth pinning because the comparison is
    /// `>=` and a client's tick will land on the boundary regularly.
    #[test]
    fn thresholds_are_inclusive_at_the_boundary() {
        let policy = TriggerPolicy::default();
        assert_eq!(
            decide(
                &policy,
                &TriggerInput {
                    idle_for: Some(policy.quiescence),
                    ..base_input()
                },
            ),
            Some(PushReason::Quiescence),
        );
        assert_eq!(
            decide(
                &policy,
                &TriggerInput {
                    idle_for: Some(policy.quiescence - Duration::from_nanos(1)),
                    since_last_push: policy.periodic,
                    ..base_input()
                },
            ),
            Some(PushReason::Periodic),
        );
    }

    /// A session with no transcript files yet (`idle_for: None`) cannot quiesce,
    /// but the periodic net still reaches it — and exit still retires it. This is
    /// the wire-attributed-before-first-flush case.
    #[test]
    fn a_session_with_no_files_yet_still_reaches_the_other_triggers() {
        let policy = TriggerPolicy::default();
        let pending = TriggerInput {
            idle_for: None,
            ..base_input()
        };
        assert_eq!(decide(&policy, &pending), None);
        assert_eq!(
            decide(
                &policy,
                &TriggerInput {
                    since_last_push: Duration::from_secs(301),
                    ..pending
                },
            ),
            Some(PushReason::Periodic),
        );
        assert_eq!(
            decide(
                &policy,
                &TriggerInput {
                    exited: true,
                    ..pending
                },
            ),
            Some(PushReason::Exit),
        );
    }

    /// The default policy is the one paperd shipped: 30 s quiescence, 5 min
    /// periodic. Pinned so adopting the crate's default cannot silently
    /// re-tune a consumer's push latency.
    #[test]
    fn default_policy_matches_the_shipped_thresholds() {
        let policy = TriggerPolicy::default();
        assert_eq!(policy.quiescence, Duration::from_secs(30));
        assert_eq!(policy.periodic, Duration::from_secs(300));
        assert_eq!(DEFAULT_TICK, Duration::from_secs(5));
    }

    #[test]
    fn push_reason_labels_are_stable() {
        assert_eq!(PushReason::Quiescence.as_str(), "quiescence");
        assert_eq!(PushReason::Periodic.as_str(), "periodic");
        assert_eq!(PushReason::Exit.as_str(), "exit");
    }
}
