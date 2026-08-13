//! Choosing which rollout a Codex request belongs to.
//!
//! This is the hard half of the Codex lane, and the reason it is here rather
//! than in a capture client: the ladder below, its bounded wait, and the order
//! of its rungs were validated against real Codex traffic — including
//! sub-thread families, cold watchers, and the desktop app — and a second
//! implementation would drift in ways only a parity corpus would catch. Every
//! client's `start codex` path reaches it through
//! [`crate::attribution::attribute`].
//!
//! # The ladder
//!
//! Rungs are ordered by how *exactly* their evidence identifies a thread, not
//! by how cheap they are. The first four each end the search:
//!
//! 1. **Hook-exact.** The request names a thread for which a consumer's
//!    lifecycle hooks independently reported a session, and that thread's
//!    rollout is live. Two unrelated sources agreeing on one opaque id is the
//!    strongest evidence available.
//! 2. **Child-transcript-exact.** The request is child-shaped and exactly one
//!    live rollout declares itself that sub-thread, with matching root and
//!    immediate parent ([`super::request::transcript_matches_child`]). When
//!    lifecycle evidence also names the pair, the selection records that it
//!    was hook-backed — same session, stronger provenance.
//! 3. **Marker.** The launch marker the consumer stamped identifies the
//!    *process*, so it separates concurrent launches but never threads within
//!    one.
//! 4. **Peer PID.** The rollouts the calling process holds open. Same
//!    granularity as the marker, and available when no marker was stamped.
//!
//! Rungs 3 and 4 both narrow by the rollout the request names before
//! deciding, and both refuse rather than guess when several DISTINCT live
//! sessions survive: the request's rollout id narrows the candidate set, and a
//! set that still holds more than one live session is refused rather than
//! resolved. A missed attribution heals when the
//! transcript is reconciled; a wrong one is permanent and silently corrupts a
//! sub-thread family's shape.
//!
//! # The bounded wait
//!
//! Every rung reads state that a just-started `codex` has not written yet, so
//! the ladder runs inside a poll loop under [`AttributionConfig::codex_timeout`].
//! What the loop waits *for* depends on the evidence:
//!
//! * With no exact evidence in prospect, the first rung that answers wins and
//!   the loop returns immediately.
//! * When the request carries corroborated thread evidence — lifecycle hooks
//!   name its thread, or it is child-shaped — a heuristic answer is not good
//!   enough yet: the exact rollout is probably milliseconds from appearing,
//!   and taking the heuristic now would permanently attach the turn to the
//!   wrong thread. The loop keeps polling for an exact rung while *retaining*
//!   the best heuristic answer it has seen, and falls back to it only when the
//!   budget runs out. Retaining across polls matters because a watcher
//!   snapshot can go transiently empty; without it, a late empty scan would
//!   replace a usable answer with nothing.
//!
//! Capture never blocks past the budget. A request that ends with no session
//! still emits an envelope carrying its own account of itself — see
//! [`super::request::request_envelope`].

use tokio::time::Instant;
use tracing::warn;

use super::request::{CodexRequestIdentity, transcript_matches_child};
use super::{CodexSessionFile, open_jsonl_sessions_by_pid, session as codex_session};
use crate::attribution::pipeline::{AttributionConfig, AttributionState, RequestFacts};
use tapes_capture::peer_pid;

/// Lifecycle evidence a consumer has collected out of band.
///
/// The desktop app reports session and sub-thread boundaries through an
/// installed plugin's hook command rather than through anything observable on
/// the wire ([`crate::attribution::codex_app`]). Delivery of those
/// observations — what process receives them, how they are authenticated,
/// where they are stored — is deployment knowledge and stays with the
/// consumer, so the ladder takes the *question* as a trait rather than owning
/// the store that answers it.
///
/// A consumer with no hook lane passes `None` and the hook rungs simply never
/// fire; nothing else about the ladder changes. That is the whole reason this
/// is injected rather than forked: one algorithm, two evidence situations.
pub trait CodexHookEvidence: std::fmt::Debug + Send + Sync {
    /// Has lifecycle evidence named this exact opaque session id?
    fn has_hook_session(&self, session_id: &str) -> bool;
    /// Does lifecycle evidence associate this exact child agent id with this
    /// exact parent session id?
    fn has_hook_subagent(&self, parent_session_id: &str, agent_id: &str) -> bool;
}

/// Which rung answered, and with what.
///
/// Retained alongside the selected session so a consumer can record *why* a
/// turn was attributed the way it was. Attribution repair is only possible
/// against a decision whose basis was written down.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum CodexSelection {
    /// Lifecycle evidence and the request named the same live rollout.
    HookExact { session_id: String },
    /// A child-shaped request joined its own transcript, and lifecycle
    /// evidence independently named the same parent/child pair.
    HookSubagentExact {
        session_id: String,
        parent_session_id: String,
    },
    /// A child-shaped request joined its own transcript, on transcript
    /// lineage alone.
    ChildTranscriptExact {
        session_id: String,
        parent_session_id: String,
    },
    /// The launch marker matched exactly one live rollout file.
    MarkerUnique { session_id: String },
    /// The marker matched several files of ONE session (rollout rotation);
    /// the most recently modified won.
    MarkerNewest { session_id: String },
    /// The calling process held exactly one live rollout open.
    PeerPidUnique { session_id: String },
    /// The calling process held several files of ONE session open; the most
    /// recently modified won.
    PeerPidNewest { session_id: String },
    /// The unmarked recency fallback resolved a single live session.
    RecentUnique { session_id: String },
    /// No rung produced a session — including because a rung refused an
    /// ambiguous set rather than guess.
    #[default]
    NoMatch,
}

/// The candidate sets each rung considered, and the rung that answered.
///
/// Deliberately not `#[non_exhaustive]`: a consumer's own tests need to build
/// one to exercise how it journals a decision, and the field set here IS the
/// contract. Growth happens in [`CodexSelection`], which is non-exhaustive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexSelectionEvidence {
    /// Live rollouts declaring themselves the sub-thread the request claims.
    pub child_candidates: Vec<CodexSessionFile>,
    /// Live rollouts stamped with the consumer's launch marker.
    pub marker_candidates: Vec<CodexSessionFile>,
    /// The PID behind the loopback connection, when it resolved.
    pub peer_pid: Option<i32>,
    /// Live rollouts that PID holds open.
    pub peer_candidates: Vec<CodexSessionFile>,
    /// Every live rollout of ours in the recency window.
    pub recent_candidates: Vec<CodexSessionFile>,
    /// Which rung answered.
    pub selection: CodexSelection,
}

/// One request's selection outcome.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexSelectionResult {
    /// The rollout the ladder chose, if any.
    pub session: Option<CodexSessionFile>,
    /// What it considered on the way there.
    pub evidence: CodexSelectionEvidence,
}

/// One pass of the ladder, plus the fallback the outer loop may still want.
struct Attempt {
    result: CodexSelectionResult,
    recent_fallback: Option<CodexSessionFile>,
}

impl Attempt {
    /// Replace this pass's outcome with the recency fallback, if there is one.
    fn with_recent_fallback(mut self) -> CodexSelectionResult {
        if let Some(session) = self.recent_fallback {
            self.result.evidence.selection = CodexSelection::RecentUnique {
                session_id: session.session_id.clone(),
            };
            self.result.session = Some(session);
        }
        self.result
    }
}

/// Run the ladder for one request under the configured budget.
pub async fn select(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
    identity: &CodexRequestIdentity,
) -> CodexSelectionResult {
    // Corroborated thread evidence is worth waiting for; see the module docs.
    let wait_for_exact = !identity.conflicting_metadata
        && identity.thread_id.as_deref().is_some_and(|thread_id| {
            facts
                .codex_hook_evidence
                .is_some_and(|hooks| hooks.has_hook_session(thread_id))
                || identity.is_child_shaped()
        });
    let deadline = Instant::now() + config.codex_timeout;
    let mut retained: Option<CodexSelectionResult> = None;
    loop {
        let attempt = select_once(state, config, facts, identity);
        let exact = matches!(
            attempt.result.evidence.selection,
            CodexSelection::HookExact { .. }
                | CodexSelection::HookSubagentExact { .. }
                | CodexSelection::ChildTranscriptExact { .. }
        );
        if exact || (!wait_for_exact && attempt.result.session.is_some()) {
            return attempt.result;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // Prefer the newest usable answer from the final poll; only fall
            // back to a retained one if this poll produced nothing.
            if wait_for_exact && attempt.result.session.is_some() {
                return attempt.result;
            }
            if wait_for_exact && facts.codex_marker.is_none() && attempt.recent_fallback.is_some() {
                return attempt.with_recent_fallback();
            }
            if let Some(retained) = retained {
                return retained;
            }
            // A marker was supplied and never matched: the launch told us
            // exactly which provider to expect, so falling back to "some
            // recent session" would override explicit information with a
            // guess. Only the unmarked path may fall back.
            return if facts.codex_marker.is_none() {
                attempt.with_recent_fallback()
            } else {
                attempt.result
            };
        }

        if wait_for_exact {
            if attempt.result.session.is_some() {
                retained = Some(attempt.result);
            } else if facts.codex_marker.is_none() && attempt.recent_fallback.is_some() {
                retained = Some(attempt.with_recent_fallback());
            }
        }

        tokio::time::sleep(config.codex_poll.min(remaining)).await;
    }
}

/// The rollout evidence this request offers, in the crate's one order.
///
/// A supplied identity supersedes [`RequestFacts::codex_rollout_id`]: it reads
/// the same two headers but additionally withholds them when the request
/// contradicts itself, and a lane that then re-read the raw header would undo
/// exactly that guard.
fn rollout_evidence<'a>(
    facts: RequestFacts<'a>,
    identity: &'a CodexRequestIdentity,
) -> Option<&'a str> {
    match facts.codex_identity {
        Some(_) => identity.rollout_id(),
        None => facts.codex_rollout_id,
    }
}

fn select_once(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
    identity: &CodexRequestIdentity,
) -> Attempt {
    let cutoff = time::OffsetDateTime::now_utc() - config.codex_recent_window;
    let snapshot = state.codex_watcher.load_full();
    let recent: Vec<&CodexSessionFile> = snapshot
        .sessions
        .iter()
        .filter(|session| is_live_candidate(session, config, cutoff))
        .collect();
    let rollout_id = rollout_evidence(facts, identity);
    let recent_candidates: Vec<CodexSessionFile> = recent.iter().copied().cloned().collect();
    let recent_fallback = narrow_by_rollout_id(rollout_id, recent_candidates.clone())
        .and_then(|candidates| {
            one_live_session_or_refuse(
                "recent-session",
                "multiple recent sessions and the request named no rollout",
                candidates,
            )
        })
        .map(|resolved| resolved.session);

    let child_matches: Vec<&CodexSessionFile> = if identity.is_child_shaped() {
        recent
            .iter()
            .copied()
            .filter(|session| transcript_matches_child(session, identity))
            .collect()
    } else {
        Vec::new()
    };
    let child_candidates: Vec<CodexSessionFile> = child_matches.iter().copied().cloned().collect();

    let mut evidence = CodexSelectionEvidence {
        child_candidates,
        recent_candidates,
        ..CodexSelectionEvidence::default()
    };

    // Rung 1 — hook-exact. Codex sends the active thread as `thread-id`; a
    // root turn names the session and a sub-thread names itself. Trust that
    // exact id only when the consumer's hook lane independently delivered
    // lifecycle evidence for the same opaque value.
    if !identity.conflicting_metadata
        && let Some(hook_session_id) = identity.thread_id.as_deref()
        && facts
            .codex_hook_evidence
            .is_some_and(|hooks| hooks.has_hook_session(hook_session_id))
        && let Some(session) = recent
            .iter()
            .copied()
            .find(|session| session.session_id == hook_session_id)
    {
        evidence.selection = CodexSelection::HookExact {
            session_id: session.session_id.clone(),
        };
        return Attempt {
            result: CodexSelectionResult {
                session: Some(session.clone()),
                evidence,
            },
            recent_fallback,
        };
    }

    // Rung 2 — child-transcript-exact. Exactly one live rollout declaring
    // itself this sub-thread; more than one is not an identification.
    if let ([session], Some(parent_session_id)) = (
        child_matches.as_slice(),
        identity.parent_thread_id.as_deref(),
    ) {
        let parent_session_id = parent_session_id.to_owned();
        let hook_backed = identity.thread_id.as_deref().is_some_and(|agent_id| {
            facts
                .codex_hook_evidence
                .is_some_and(|hooks| hooks.has_hook_subagent(&parent_session_id, agent_id))
        });
        evidence.selection = if hook_backed {
            CodexSelection::HookSubagentExact {
                session_id: session.session_id.clone(),
                parent_session_id,
            }
        } else {
            CodexSelection::ChildTranscriptExact {
                session_id: session.session_id.clone(),
                parent_session_id,
            }
        };
        return Attempt {
            result: CodexSelectionResult {
                session: Some((*session).clone()),
                evidence,
            },
            recent_fallback,
        };
    }

    // Rung 3 — the launch marker.
    if let Some(marker) = facts.codex_marker {
        let matches: Vec<CodexSessionFile> = recent
            .iter()
            .copied()
            .filter(|session| session.has_model_provider(marker))
            .cloned()
            .collect();
        evidence.marker_candidates = matches.clone();
        if let Some(resolved) = marker_match(rollout_id, matches) {
            let session_id = resolved.session.session_id.clone();
            evidence.selection = if resolved.from_unique_candidate {
                CodexSelection::MarkerUnique { session_id }
            } else {
                CodexSelection::MarkerNewest { session_id }
            };
            return Attempt {
                result: CodexSelectionResult {
                    session: Some(resolved.session),
                    evidence,
                },
                recent_fallback,
            };
        }
    }

    // Rung 4 — the rollouts the calling process holds open. Single-attempt on
    // purpose: this runs inside `select`'s poll loop, which already retries
    // under its own deadline, so a transient scan miss is re-tried on the next
    // round. The retrying lookup would block this worker between its attempts
    // and stack its backoff inside the loop's budget.
    let peer_pid = facts
        .peer
        .and_then(|peer| peer_pid::lookup_owner_once(peer).pid);
    evidence.peer_pid = peer_pid;
    let matches: Vec<CodexSessionFile> = peer_pid
        .into_iter()
        .flat_map(open_jsonl_sessions_by_pid)
        .filter_map(|path| codex_session::read(&path))
        .filter(|session| is_live_candidate(session, config, cutoff))
        .collect();
    evidence.peer_candidates = matches.clone();
    let resolved = peer_match(rollout_id, matches);
    evidence.selection = resolved
        .as_ref()
        .map_or(CodexSelection::NoMatch, |resolved| {
            let session_id = resolved.session.session_id.clone();
            if resolved.from_unique_candidate {
                CodexSelection::PeerPidUnique { session_id }
            } else {
                CodexSelection::PeerPidNewest { session_id }
            }
        });
    Attempt {
        result: CodexSelectionResult {
            session: resolved.map(|resolved| resolved.session),
            evidence,
        },
        recent_fallback,
    }
}

fn is_live_candidate(
    session: &CodexSessionFile,
    config: &AttributionConfig,
    cutoff: time::OffsetDateTime,
) -> bool {
    config.codex_provider.matches_session(session)
        && session.modified_at.is_some_and(|ts| ts >= cutoff)
}

/// Narrow a candidate set to the rollout the request itself names.
///
/// This is the only evidence that separates *threads* inside one Codex
/// process. A `codex` running sub-threads holds the parent rollout and every
/// child rollout open at once and stamps each request with the id of the
/// rollout it belongs to, so an exact match against each candidate's own
/// session id is a decision rather than a guess.
///
/// Returns `None` — refuse — when the request named a rollout and none of the
/// candidates are it. The request is authoritative about its own identity, so
/// "not one of these" is information, not a reason to fall back to a tie-break
/// over the others. Returns the set untouched when there is no evidence to
/// apply, leaving each rung's own policy in force.
pub(crate) fn narrow_by_rollout_id(
    rollout_id: Option<&str>,
    candidates: Vec<CodexSessionFile>,
) -> Option<Vec<CodexSessionFile>> {
    let Some(rollout_id) = rollout_id else {
        return Some(candidates);
    };
    if candidates.is_empty() {
        // Nothing to disagree with yet — the rollout may still be appearing on
        // disk, and the caller's bounded poll is what waits for it.
        return Some(candidates);
    }
    let selected: Vec<CodexSessionFile> = candidates
        .iter()
        .filter(|session| session.session_id == rollout_id)
        .cloned()
        .collect();
    if selected.is_empty() {
        let refs: Vec<&CodexSessionFile> = candidates.iter().collect();
        warn!(
            rollout_id,
            count = candidates.len(),
            sample = ?sample_of(&refs),
            "codex-session: the request names a rollout none of the live candidates \
             are; refusing to guess",
        );
        return None;
    }
    Some(selected)
}

/// One rung's answer, plus whether the evidence that produced it left exactly
/// one candidate standing.
///
/// The flag is what separates "this rung identified a session" from "this rung
/// found several files and collapsed them", which is a distinction a journal
/// reading back an attribution decision needs. It reflects the set the rung
/// actually chose from — after any narrowing — because that is the set the
/// decision was made over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) session: CodexSessionFile,
    pub(crate) from_unique_candidate: bool,
}

/// Resolve a candidate set down to one session, refusing on ambiguity.
///
/// Several files sharing ONE session id are rollout rotation — newest wins,
/// since they are the same session either way. Several DISTINCT live sessions
/// mean the evidence that produced this set does not actually identify a
/// single session, and picking the newest would attach this request to
/// whichever thread happened to flush last. A missed attribution heals through
/// transcript reconciliation; a wrong one is permanent, so refuse.
pub(crate) fn one_live_session_or_refuse(
    reason: &str,
    detail: &str,
    candidates: Vec<CodexSessionFile>,
) -> Option<Resolved> {
    let from_unique_candidate = candidates.len() == 1;
    let mut ids: Vec<&str> = candidates
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() > 1 {
        let refs: Vec<&CodexSessionFile> = candidates.iter().collect();
        warn!(
            reason,
            count = candidates.len(),
            sample = ?sample_of(&refs),
            "codex-session: {detail}; refusing to guess",
        );
        return None;
    }
    unique_or_newest(reason, candidates).map(|session| Resolved {
        session,
        from_unique_candidate,
    })
}

/// Resolve marker matches. The marker is fresh per launch BY CONTRACT, so
/// several live files may only legitimately share one when they belong to the
/// same session (rollout rotation) — newest wins there. Two DISTINCT live
/// sessions sharing a marker means either a consumer reused a provider id
/// across concurrent processes or one process is running sub-threads; picking
/// the newest would silently attach one thread's traffic to the other's
/// session. The request's own rollout id settles it when present; otherwise
/// the marker rung refuses and the ladder falls through to peer-PID evidence.
pub(crate) fn marker_match(
    rollout_id: Option<&str>,
    candidates: Vec<CodexSessionFile>,
) -> Option<Resolved> {
    let candidates = narrow_by_rollout_id(rollout_id, candidates)?;
    one_live_session_or_refuse(
        "marker",
        "marker shared by multiple LIVE sessions and the request named no rollout",
        candidates,
    )
}

/// Resolve the rollouts one PID holds open.
///
/// The rung's original premise was that a `codex` process holds exactly one
/// live rollout and any others are stale files, which made newest-wins safe.
/// Sub-threads break that premise: the parent rollout and every child rollout
/// are open at once and ALL are live, so newest-wins attaches a turn to
/// whichever thread wrote last. The request's own rollout id distinguishes
/// them; without it, several live sessions under one PID are simply ambiguous.
pub(crate) fn peer_match(
    rollout_id: Option<&str>,
    candidates: Vec<CodexSessionFile>,
) -> Option<Resolved> {
    let candidates = narrow_by_rollout_id(rollout_id, candidates)?;
    one_live_session_or_refuse(
        "peer-open-file",
        "one process holds multiple LIVE rollouts open (a sub-thread family) and the \
         request named no rollout",
        candidates,
    )
}

/// Collapse candidates that are already known to be ONE session.
///
/// Every caller reaches this through [`one_live_session_or_refuse`], which has
/// already established that the set holds at most one distinct session id, so
/// a tie here is rollout rotation and newest-wins is a choice between files
/// rather than between sessions. Do not call this directly on a set whose
/// session ids have not been checked — that is the newest-wins guess this lane
/// used to make.
pub(crate) fn unique_or_newest(
    reason: &str,
    candidates: Vec<CodexSessionFile>,
) -> Option<CodexSessionFile> {
    match candidates.as_slice() {
        [] => None,
        [session] => Some(session.clone()),
        _ => {
            let refs: Vec<&CodexSessionFile> = candidates.iter().collect();
            warn!(
                reason,
                count = candidates.len(),
                sample = ?sample_of(&refs),
                "codex-session: one session across multiple rollout files; \
                 using most recently modified",
            );
            candidates
                .into_iter()
                .max_by_key(|session| session.modified_at.unwrap_or(session.timestamp))
        }
    }
}

fn sample_of(candidates: &[&CodexSessionFile]) -> Vec<String> {
    candidates
        .iter()
        .take(3)
        .map(|session| format!("{} ({})", session.session_id, session.path.display()))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::attribution::codex::CodexWatcherSnapshot;
    use crate::attribution::pipeline::CodexProviderFilter;
    use crate::attribution::{WatcherSnapshot, pipeline::AttributionConfig};
    use arc_swap::ArcSwap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    /// A hook store that answers from two fixed sets, standing in for a
    /// consumer's lifecycle-evidence registry.
    #[derive(Debug, Default)]
    struct Hooks {
        sessions: Vec<String>,
        subagents: Vec<(String, String)>,
    }

    impl Hooks {
        fn with_session(session_id: &str) -> Self {
            Self {
                sessions: vec![session_id.to_owned()],
                subagents: Vec::new(),
            }
        }

        fn with_subagent(parent: &str, agent: &str) -> Self {
            Self {
                sessions: Vec::new(),
                subagents: vec![(parent.to_owned(), agent.to_owned())],
            }
        }
    }

    impl CodexHookEvidence for Hooks {
        fn has_hook_session(&self, session_id: &str) -> bool {
            self.sessions.iter().any(|id| id == session_id)
        }

        fn has_hook_subagent(&self, parent_session_id: &str, agent_id: &str) -> bool {
            self.subagents
                .iter()
                .any(|(parent, agent)| parent == parent_session_id && agent == agent_id)
        }
    }

    fn config() -> AttributionConfig {
        AttributionConfig::new(
            CodexProviderFilter::new("paper-openai"),
            crate::harness::RegistryUserAgents,
        )
    }

    fn state_with(sessions: Vec<CodexSessionFile>) -> AttributionState {
        AttributionState::new(
            Arc::new(ArcSwap::from_pointee(WatcherSnapshot::default())),
            Arc::new(ArcSwap::from_pointee(CodexWatcherSnapshot { sessions })),
        )
    }

    fn session(id: &str, modified_at: time::OffsetDateTime) -> CodexSessionFile {
        session_with_provider(id, modified_at, "paper-openai")
    }

    fn session_with_provider(
        id: &str,
        modified_at: time::OffsetDateTime,
        provider: &str,
    ) -> CodexSessionFile {
        CodexSessionFile {
            session_id: id.to_owned(),
            root_session_id: Some(id.to_owned()),
            parent_thread_id: None,
            subagent_kind: None,
            timestamp: modified_at,
            modified_at: Some(modified_at),
            cwd: Some("/tmp/work".to_owned()),
            originator: Some("codex-tui".to_owned()),
            cli_version: Some("0.139.0".to_owned()),
            source: Some("cli".to_owned()),
            thread_source: Some("user".to_owned()),
            model_provider: Some(provider.to_owned()),
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
        }
    }

    fn descendant_session(
        id: &str,
        root: &str,
        parent: &str,
        kind: &str,
        modified_at: time::OffsetDateTime,
    ) -> CodexSessionFile {
        let mut session = session(id, modified_at);
        session.root_session_id = Some(root.to_owned());
        session.parent_thread_id = Some(parent.to_owned());
        session.thread_source = Some("subagent".to_owned());
        session.source = Some(format!(r#"{{"subagent":{{"other":"{kind}"}}}}"#));
        session.subagent_kind = Some(kind.to_owned());
        session
    }

    fn child_session(
        id: &str,
        parent: &str,
        kind: &str,
        modified_at: time::OffsetDateTime,
    ) -> CodexSessionFile {
        descendant_session(id, parent, parent, kind, modified_at)
    }

    fn descendant_identity(
        root: &str,
        parent: &str,
        child: &str,
        kind: &str,
    ) -> CodexRequestIdentity {
        CodexRequestIdentity {
            correlation_id: "correlation-child".to_owned(),
            session_id: Some(root.to_owned()),
            thread_id: Some(child.to_owned()),
            parent_thread_id: Some(parent.to_owned()),
            turn_id: Some("turn-child".to_owned()),
            subagent_kind: Some(kind.to_owned()),
            conflicting_metadata: false,
        }
    }

    fn child_identity(parent: &str, child: &str, kind: &str) -> CodexRequestIdentity {
        descendant_identity(parent, parent, child, kind)
    }

    fn thread_identity(thread_id: Option<&str>) -> CodexRequestIdentity {
        CodexRequestIdentity {
            correlation_id: "correlation-test".to_owned(),
            thread_id: thread_id.map(str::to_owned),
            ..CodexRequestIdentity::default()
        }
    }

    fn facts<'a>(
        marker: Option<&'a str>,
        identity: &'a CodexRequestIdentity,
        hooks: Option<&'a dyn CodexHookEvidence>,
    ) -> RequestFacts<'a> {
        RequestFacts {
            codex_marker: marker,
            codex_route: true,
            codex_identity: Some(identity),
            codex_hook_evidence: hooks,
            ..RequestFacts::default()
        }
    }

    fn once(
        state: &AttributionState,
        marker: Option<&str>,
        identity: &CodexRequestIdentity,
        hooks: Option<&dyn CodexHookEvidence>,
    ) -> CodexSelectionResult {
        select_once(state, &config(), facts(marker, identity, hooks), identity).result
    }

    // --- the exact rungs -------------------------------------------------

    #[test]
    fn hook_evidence_and_a_named_thread_beat_a_newer_sibling() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("hook-session", now - time::Duration::seconds(1)),
            session("newest-session", now),
        ]);
        let hooks = Hooks::with_session("hook-session");
        let got = once(
            &state,
            Some("paper-openai"),
            &thread_identity(Some("hook-session")),
            Some(&hooks),
        );

        assert_eq!(got.session.unwrap().session_id, "hook-session");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::HookExact {
                session_id: "hook-session".to_owned(),
            },
        );
    }

    #[test]
    fn a_child_shaped_request_joins_its_own_transcript() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("parent", now),
            child_session(
                "child",
                "parent",
                "guardian",
                now - time::Duration::seconds(1),
            ),
        ]);
        let identity = child_identity("parent", "child", "guardian");
        let got = once(&state, Some("paper-openai"), &identity, None);

        assert_eq!(got.session.unwrap().session_id, "child");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::ChildTranscriptExact {
                session_id: "child".to_owned(),
                parent_session_id: "parent".to_owned(),
            },
        );
        assert_eq!(got.evidence.child_candidates.len(), 1);
    }

    #[test]
    fn a_nested_child_joins_on_its_direct_parent_not_the_root() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("root", now),
            child_session(
                "parent",
                "root",
                "explorer",
                now - time::Duration::seconds(1),
            ),
            descendant_session(
                "child",
                "root",
                "parent",
                "guardian",
                now - time::Duration::seconds(2),
            ),
        ]);
        let identity = descendant_identity("root", "parent", "child", "guardian");
        let got = once(&state, Some("paper-openai"), &identity, None);

        assert_eq!(got.session.unwrap().session_id, "child");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::ChildTranscriptExact {
                session_id: "child".to_owned(),
                parent_session_id: "parent".to_owned(),
            },
        );
    }

    #[test]
    fn lifecycle_evidence_upgrades_a_child_join_to_hook_backed() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("parent", now),
            child_session(
                "child",
                "parent",
                "guardian",
                now - time::Duration::seconds(1),
            ),
        ]);
        let hooks = Hooks::with_subagent("parent", "child");
        let identity = child_identity("parent", "child", "guardian");
        let got = once(&state, Some("paper-openai"), &identity, Some(&hooks));

        assert_eq!(
            got.evidence.selection,
            CodexSelection::HookSubagentExact {
                session_id: "child".to_owned(),
                parent_session_id: "parent".to_owned(),
            },
        );
    }

    #[test]
    fn duplicate_child_transcripts_are_not_an_identification() {
        // Two files for one child is rollout rotation, but the child rung
        // demands a singleton; the marker rung then narrows on the request's
        // own thread id and resolves the rotation.
        let now = time::OffsetDateTime::now_utc();
        let child = child_session(
            "child",
            "parent",
            "guardian",
            now - time::Duration::seconds(1),
        );
        let state = state_with(vec![session("parent", now), child.clone(), child]);
        let identity = child_identity("parent", "child", "guardian");
        let got = once(&state, Some("paper-openai"), &identity, None);

        assert_eq!(got.evidence.child_candidates.len(), 2);
        assert_eq!(got.session.unwrap().session_id, "child");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::MarkerNewest {
                session_id: "child".to_owned(),
            },
        );
    }

    // --- contradiction disables identity ---------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_self_contradicting_request_gets_no_identity_rungs() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("hook-session", now - time::Duration::seconds(1)),
            session("newest-session", now),
        ]);
        let hooks = Hooks::with_session("hook-session");
        let identity = CodexRequestIdentity::from_headers(&{
            let mut headers = http::HeaderMap::new();
            headers.insert("thread-id", "hook-session".parse().unwrap());
            headers.insert(
                "x-codex-turn-metadata",
                r#"{"thread_id":"contradictory-session"}"#.parse().unwrap(),
            );
            headers
        });
        assert!(identity.conflicting_metadata);

        // `hook-session` is live and hook evidence names it, so an identity the
        // ladder trusted would resolve it immediately. The contradiction takes
        // the whole identity out of play — hook-exact cannot fire, and the
        // thread id may not narrow the marker rung either — leaving two
        // distinct live sessions and nothing to choose between them.
        let config = config();
        let got = tokio::time::timeout(
            config.codex_timeout + config.codex_poll,
            select(
                &state,
                &config,
                facts(Some("paper-openai"), &identity, Some(&hooks)),
                &identity,
            ),
        )
        .await
        .expect("the ladder must still return inside its budget");

        assert_eq!(got.session, None);
        assert_eq!(got.evidence.selection, CodexSelection::NoMatch);
    }

    // --- the heuristic rungs ---------------------------------------------

    #[test]
    fn a_marker_selects_its_own_launch() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session_with_provider("sid-1", now, "paper-openai-one"),
            session_with_provider("sid-2", now, "paper-openai-two"),
        ]);
        let got = once(
            &state,
            Some("paper-openai-two"),
            &CodexRequestIdentity::default(),
            None,
        );
        assert_eq!(got.session.unwrap().session_id, "sid-2");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::MarkerUnique {
                session_id: "sid-2".to_owned(),
            },
        );
    }

    #[test]
    fn a_named_thread_narrows_an_ambiguous_marker_without_hook_evidence() {
        // The marker cannot separate threads of one launch. The request's own
        // rollout id can, and an exact match against a live rollout's own
        // session id is a decision rather than a guess — so it applies even
        // with no lifecycle evidence to corroborate it.
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("named-thread", now - time::Duration::seconds(1)),
            session("newest-session", now),
        ]);
        let got = once(
            &state,
            Some("paper-openai"),
            &thread_identity(Some("named-thread")),
            None,
        );
        assert_eq!(got.session.unwrap().session_id, "named-thread");
    }

    #[test]
    fn an_ambiguous_marker_with_no_thread_evidence_refuses() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("a", now - time::Duration::seconds(1)),
            session("b", now),
        ]);
        let got = once(
            &state,
            Some("paper-openai"),
            &CodexRequestIdentity::default(),
            None,
        );
        assert_eq!(got.session, None);
        assert_eq!(got.evidence.selection, CodexSelection::NoMatch);
    }

    #[test]
    fn a_request_naming_an_invisible_rollout_refuses_the_visible_one() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![session("visible", now)]);
        let got = once(
            &state,
            Some("paper-openai"),
            &thread_identity(Some("not-on-disk-yet")),
            None,
        );
        assert_eq!(got.session, None);
    }

    #[test]
    fn marker_rotation_of_one_session_still_resolves_to_the_newest_file() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("sid-a", now - time::Duration::minutes(5)),
            session("sid-a", now),
        ]);
        let got = once(
            &state,
            Some("paper-openai"),
            &CodexRequestIdentity::default(),
            None,
        );
        assert_eq!(got.session.unwrap().session_id, "sid-a");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::MarkerNewest {
                session_id: "sid-a".to_owned(),
            },
        );
    }

    // --- the bounded wait ------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_child_shaped_request_waits_for_its_transcript_to_appear() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![session("parent", now)]);
        let identity = child_identity("parent", "child", "guardian");
        let config = config();
        let mut task = std::pin::pin!(select(
            &state,
            &config,
            facts(Some("paper-openai"), &identity, None),
            &identity,
        ));
        assert!(
            tokio::time::timeout(StdDuration::from_millis(1), task.as_mut())
                .await
                .is_err(),
            "a child-shaped request should wait briefly for its exact transcript",
        );

        state.codex_watcher.store(Arc::new(CodexWatcherSnapshot {
            sessions: vec![
                session("parent", now),
                child_session("child", "parent", "guardian", now),
            ],
        }));
        tokio::time::advance(config.codex_poll).await;

        let got = tokio::time::timeout(config.codex_timeout, task.as_mut())
            .await
            .expect("the child transcript must join inside the budget");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::ChildTranscriptExact {
                session_id: "child".to_owned(),
                parent_session_id: "parent".to_owned(),
            },
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hook_evidence_waits_out_a_ready_heuristic_answer() {
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![session("fallback-session", now)]);
        let hooks = Hooks::with_session("hook-session");
        let identity = thread_identity(Some("hook-session"));
        let config = config();
        let mut task = std::pin::pin!(select(
            &state,
            &config,
            facts(Some("paper-openai"), &identity, Some(&hooks)),
            &identity,
        ));
        assert!(
            tokio::time::timeout(StdDuration::from_millis(1), task.as_mut())
                .await
                .is_err(),
            "a ready heuristic must not win while exact hook evidence is pending",
        );

        state.codex_watcher.store(Arc::new(CodexWatcherSnapshot {
            sessions: vec![
                session("fallback-session", now),
                session("hook-session", now),
            ],
        }));
        tokio::time::advance(config.codex_poll).await;
        let got = tokio::time::timeout(config.codex_timeout, task.as_mut())
            .await
            .expect("the exact transcript must join inside the budget");

        assert_eq!(got.session.unwrap().session_id, "hook-session");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::HookExact {
                session_id: "hook-session".to_owned(),
            },
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_exact_wait_that_times_out_still_returns_within_the_budget() {
        // Hook evidence names a thread whose rollout never appears. Capture
        // must not block past the budget; with no trustworthy rollout among
        // the live candidates the ladder ends undecided rather than guessing.
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![
            session("older-fallback", now - time::Duration::seconds(1)),
            session("newest-fallback", now),
        ]);
        let hooks = Hooks::with_session("missing-hook-session");
        let identity = thread_identity(Some("missing-hook-session"));
        let config = config();
        let mut task = std::pin::pin!(select(
            &state,
            &config,
            facts(Some("paper-openai"), &identity, Some(&hooks)),
            &identity,
        ));
        assert!(
            tokio::time::timeout(StdDuration::from_millis(1), task.as_mut())
                .await
                .is_err(),
            "hook-backed attribution should wait before giving up",
        );
        let got = tokio::time::timeout(config.codex_timeout, task.as_mut())
            .await
            .expect("the ladder must return within the existing budget");
        assert_eq!(got.session, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_exact_wait_retains_a_usable_answer_across_an_empty_scan() {
        // The child rung wants a transcript that declares itself a subagent.
        // Here the only rollout named `child` is an ordinary one, so the exact
        // rung keeps missing while the marker rung — narrowed to the very same
        // id — keeps answering. That answer must survive a watcher snapshot
        // that goes transiently empty before the budget expires; otherwise a
        // late empty scan replaces a usable attribution with nothing.
        let now = time::OffsetDateTime::now_utc();
        let state = state_with(vec![session("child", now)]);
        let identity = child_identity("parent", "child", "guardian");
        let config = config();
        let mut task = std::pin::pin!(select(
            &state,
            &config,
            facts(Some("paper-openai"), &identity, None),
            &identity,
        ));
        assert!(
            tokio::time::timeout(StdDuration::from_millis(1), task.as_mut())
                .await
                .is_err(),
            "a child-shaped request waits for its exact transcript",
        );

        state
            .codex_watcher
            .store(Arc::new(CodexWatcherSnapshot::default()));
        tokio::time::advance(config.codex_poll).await;
        tokio::time::advance(config.codex_timeout).await;

        let got = task.await;
        assert_eq!(got.session.unwrap().session_id, "child");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::MarkerUnique {
                session_id: "child".to_owned(),
            },
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_explicit_marker_never_falls_back_to_a_recent_session() {
        let state = state_with(vec![session_with_provider(
            "sid-1",
            time::OffsetDateTime::now_utc(),
            "paper-openai-one",
        )]);
        let identity = CodexRequestIdentity::default();
        let got = select(
            &state,
            &config(),
            facts(Some("paper-openai-missing"), &identity, None),
            &identity,
        )
        .await;
        assert_eq!(got.session, None);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unmarked_request_falls_back_to_a_single_recent_session() {
        let state = state_with(vec![session("sole", time::OffsetDateTime::now_utc())]);
        let identity = CodexRequestIdentity::default();
        let got = select(&state, &config(), facts(None, &identity, None), &identity).await;
        assert_eq!(got.session.unwrap().session_id, "sole");
        assert_eq!(
            got.evidence.selection,
            CodexSelection::RecentUnique {
                session_id: "sole".to_owned(),
            },
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stale_and_ambiguous_recent_sessions_are_both_refused() {
        let stale = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
        let identity = CodexRequestIdentity::default();
        let got = select(
            &state_with(vec![session("stale", stale)]),
            &config(),
            facts(None, &identity, None),
            &identity,
        )
        .await;
        assert_eq!(got.session, None);

        let now = time::OffsetDateTime::now_utc();
        let got = select(
            &state_with(vec![session("a", now), session("b", now)]),
            &config(),
            facts(None, &identity, None),
            &identity,
        )
        .await;
        assert_eq!(got.session, None);
    }

    // --- the refusal primitives ------------------------------------------

    fn subagent_family() -> Vec<CodexSessionFile> {
        let now = time::OffsetDateTime::now_utc();
        vec![
            session("sid-parent", now - time::Duration::seconds(30)),
            session("sid-child-a", now - time::Duration::seconds(20)),
            session("sid-child-b", now - time::Duration::seconds(1)),
        ]
    }

    #[test]
    fn peer_lane_selects_the_thread_the_request_names_not_the_newest() {
        let got = peer_match(Some("sid-parent"), subagent_family())
            .expect("the named rollout is right there among the candidates");
        assert_eq!(got.session.session_id, "sid-parent");
        assert!(got.from_unique_candidate, "narrowing left exactly one");
    }

    #[test]
    fn peer_lane_refuses_a_live_family_with_no_thread_evidence() {
        assert!(peer_match(None, subagent_family()).is_none());
    }

    #[test]
    fn peer_lane_refuses_when_the_named_rollout_is_not_among_the_candidates() {
        assert!(peer_match(Some("sid-elsewhere"), subagent_family()).is_none());
    }

    #[test]
    fn a_lone_rollout_still_attributes_without_thread_evidence() {
        let sole = vec![session("sid-sole", time::OffsetDateTime::now_utc())];
        let got = peer_match(None, sole).expect("one live rollout is unambiguous");
        assert_eq!(got.session.session_id, "sid-sole");
    }

    #[test]
    fn thread_evidence_still_collapses_rotation_of_the_named_session() {
        let now = time::OffsetDateTime::now_utc();
        let got = peer_match(
            Some("sid-child-a"),
            vec![
                session("sid-child-a", now - time::Duration::minutes(4)),
                session("sid-child-a", now - time::Duration::seconds(2)),
                session("sid-parent", now - time::Duration::seconds(1)),
            ],
        )
        .expect("rotation of the named session resolves");
        assert_eq!(got.session.session_id, "sid-child-a");
        assert!(
            !got.from_unique_candidate,
            "two files of one session is a collapse, not an identification",
        );
    }

    #[test]
    fn an_empty_candidate_set_is_a_miss_not_a_refusal() {
        assert!(peer_match(Some("sid-parent"), Vec::new()).is_none());
        assert!(narrow_by_rollout_id(Some("sid-parent"), Vec::new()).is_some());
    }

    #[test]
    fn the_marker_lane_is_thread_aware_too() {
        let got = marker_match(Some("sid-child-b"), subagent_family())
            .expect("thread evidence resolves what the marker cannot");
        assert_eq!(got.session.session_id, "sid-child-b");
    }

    #[test]
    fn marker_shared_by_distinct_live_sessions_refuses() {
        let now = time::OffsetDateTime::now_utc();
        assert!(
            marker_match(
                None,
                vec![
                    session_with_provider("sid-a", now, "tapesctl-openai-x"),
                    session_with_provider("sid-b", now, "tapesctl-openai-x"),
                ],
            )
            .is_none()
        );
    }

    #[test]
    fn rotation_ties_break_to_the_most_recently_modified() {
        let now = time::OffsetDateTime::now_utc();
        let got = unique_or_newest(
            "marker",
            vec![
                session("sid-a", now - time::Duration::minutes(5)),
                session("sid-a", now - time::Duration::seconds(5)),
            ],
        );
        assert_eq!(got.map(|s| s.session_id), Some("sid-a".to_owned()));
    }
}
