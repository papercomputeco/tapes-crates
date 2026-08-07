//! The attribution pipeline: one request in, one [`Attributed`] outcome out.
//!
//! The sibling modules in [`crate::attribution`] are *primitives* — read a
//! session file, look up a peer PID, scan for a fork parent. Composing them
//! into "who sent this request?" is the part that was hard to get right, and it
//! is the part that must not exist twice: paperd validated this exact sequence
//! against real Claude and Codex traffic, and a second capture client that
//! re-derived it would drift silently, mis-attributing sessions in ways only a
//! parity corpus would catch. So the composition lives here, and both
//! `tapesctl start` and `paper start` call [`attribute`].
//!
//! # The two lanes
//!
//! A request arrives on exactly one of two lanes, and the consumer decides
//! which by setting [`RequestFacts::codex_route`]:
//!
//! * **Claude lane.** Gate on the User-Agent resolving to Claude, then poll the watcher
//!   snapshot until the peer PID resolves to a parsed
//!   `~/.claude/sessions/<pid>.json`. On a hit, recover fork-parent lineage
//!   (cached per session id). A miss yields [`Attributed::UnknownHarness`].
//! * **Codex lane.** Codex writes no PID-indexed session file, so identity is
//!   recovered from the rollout JSONL a live `codex` process holds open, plus
//!   what the request says about itself. The rungs, their order, and their
//!   bounded wait live in [`super::codex::select`]; a miss still emits an
//!   envelope, because a Codex request carries its own account of its identity
//!   even when no rollout could be resolved.
//!
//! ## Threads inside one Codex process
//!
//! Neither the marker nor the PID identifies a *thread*. A `codex` running
//! subagents is one process, launched once, holding the parent rollout and
//! every child rollout open simultaneously — so both can legitimately see
//! several live sessions at once, and any tie-break among them attaches a turn
//! to whichever thread flushed last. The request itself is the only thing that
//! knows, which is why the Codex lane takes a whole
//! [`CodexRequestIdentity`] rather than a session id: the thread it names, the
//! parent it names, and whether its two accounts of itself agree.
//!
//! ## Why the miss cases differ
//!
//! [`Attributed::UnknownHarness`] emits an envelope that says "harness
//! unknown". A Codex-lane miss emits a *Codex* envelope with no session id but
//! with the request's own allowlisted identifiers and the consumer's
//! correlation id — the join key an attribution-repair pass needs to find the
//! turn again. That is not an identity the pipeline declined to assert: the
//! route already established the harness, and a child-shaped request names its
//! root session in its own headers whether or not any rollout was on disk.
//! Withholding it would make an unresolved turn permanently unrepairable.
//!
//! # What stays parameterized
//!
//! Nothing Paper-specific lives here. The consumer supplies:
//!
//! * the **marker header value** ([`RequestFacts::codex_marker`]) — the header
//!   *name* is the consumer's (paperd sends `X-Paper-Codex-Attribution`), and
//!   matching it against a launch recipe's `with_attribution_header` is the
//!   consumer's business;
//! * whether the request is **on a Codex route**
//!   ([`RequestFacts::codex_route`]) — route grammars are deployment
//!   knowledge, not harness knowledge;
//! * the **Codex provider id** ([`CodexProviderFilter`]) its launch recipe
//!   configured, so the pipeline can tell its own traffic from a Codex session
//!   pointed at some other provider;
//! * **lifecycle-hook evidence** ([`RequestFacts::codex_hook_evidence`]), when
//!   the consumer has a hook lane at all. It is injected as a trait rather
//!   than forked into a second algorithm: a consumer without one passes `None`
//!   and the hook rungs simply never fire;
//! * the **User-Agent → harness resolver** ([`UserAgentHarness`]), which
//!   answers "which harness sent this?" for the lane gate. Which User-Agents
//!   name which harness is a registry declaration that changes every time a
//!   harness is added — precisely what this module must not hold, or the
//!   composition becomes a second place a harness has to be taught about.
//!
//! The provider filter is why [`CodexProviderFilter`] exists rather than a
//! hardcoded `paper-openai` test: a standalone `tapesctl` names its provider
//! something else entirely, and a shared crate that only recognised Paper's
//! spelling would silently attribute nothing for every other consumer.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, PoisonError};

use tokio::time::{Duration, Instant};

use super::Attribution;
use super::claude::{ClaudeSessionFile, WatcherSnapshotHandle, fork_parent};
use super::codex::request::{self as codex_request, CodexRequestIdentity};
use super::codex::select::{CodexHookEvidence, CodexSelectionEvidence};
use super::codex::{CodexSessionFile, CodexWatcherSnapshotHandle, select as codex_select};
use tapes_capture::envelope::{HARNESS_ID_CLAUDE, HARNESS_ID_CODEX_APP, TapesAttribution};
use tapes_capture::peer_pid;

/// Default bound on the Claude-lane wait for a freshly-created session file.
///
/// A harness launched by the consumer writes its session file milliseconds
/// *after* its first request leaves; without this wait, that first request —
/// which for `-p` invocations may be the only request — attributes to a
/// synthetic unknown session while every later request attributes correctly,
/// splitting one logical session in two.
pub const DEFAULT_CLAUDE_TIMEOUT: Duration = Duration::from_secs(2);

/// Default poll interval while waiting on the Claude lane.
pub const DEFAULT_CLAUDE_POLL: Duration = Duration::from_millis(25);

/// Default bound on the Codex-lane wait. Same cold-start race as Claude.
pub const DEFAULT_CODEX_TIMEOUT: Duration = Duration::from_secs(2);

/// Default poll interval while waiting on the Codex lane.
pub const DEFAULT_CODEX_POLL: Duration = Duration::from_millis(25);

/// Default age cutoff for a Codex rollout file to be considered live traffic.
///
/// Codex rollout files linger for hours after their session ends, so an
/// unbounded scan would happily attribute today's request to yesterday's
/// session. Ten minutes is long enough to cover an idle session resuming and
/// short enough that a stale file is not a candidate.
pub const DEFAULT_CODEX_RECENT_WINDOW: time::Duration = time::Duration::minutes(10);

/// Which Codex `model_provider` values belong to this capture client.
///
/// Codex records the provider id it was launched with in the rollout file's
/// `session_meta` row. A capture client configures that id through its launch
/// recipe, so recognising it is how the pipeline separates "a Codex session
/// running through *my* proxy" from "a Codex session the user is running
/// against some other provider" — attributing the latter would attach a
/// stranger's session id to our traffic.
///
/// Matching is exact-or-suffixed: a filter built from `paper-openai` matches
/// `paper-openai` and `paper-openai-transparent`, but not `paper-openai2` and
/// not `other-openai`. The suffix form exists because recipes commonly append
/// a backend discriminator to the base id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProviderFilter {
    base: String,
}

impl CodexProviderFilter {
    /// Build a filter for a launch recipe's provider id.
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    /// Does this rollout file's `model_provider` belong to us?
    #[must_use]
    pub fn matches(&self, provider: Option<&str>) -> bool {
        provider.is_some_and(|provider| {
            provider == self.base || provider.starts_with(&format!("{}-", self.base))
        })
    }

    /// Convenience: does this session file's provider belong to us?
    #[must_use]
    pub fn matches_session(&self, session: &CodexSessionFile) -> bool {
        self.matches(session.model_provider.as_deref())
    }
}

/// Resolves a request's `User-Agent` to the harness it identifies.
///
/// The pipeline needs an answer to "which harness sent this?" before it can
/// pick a lane. It used to get one by calling a named harness's match rule
/// directly, which made a module that describes itself as harness-agnostic
/// depend on the registry — and meant that teaching the gate about a second
/// User-Agent-identified harness would be an edit *here*, in the composition,
/// rather than in the one place a harness is declared.
///
/// Injected instead, for the same reason [`CodexHookEvidence`] is: the
/// question belongs to the pipeline, the answer belongs to whoever knows what
/// harnesses exist. A registry-backed implementation ships with the harness
/// registry; a consumer with its own idea of which agents it captures can
/// supply another without forking the algorithm.
///
/// Returns the harness's canonical id — the same value the envelope stamps as
/// `X-Tapes-Harness-Id` — so the pipeline can compare an answer against the
/// vocabulary it already speaks, rather than holding a registry type.
pub trait UserAgentHarness: std::fmt::Debug + Send + Sync {
    /// The id of the harness `user_agent` identifies.
    ///
    /// `None` for a User-Agent no harness claims — a loopback `curl`, a health
    /// probe, or a harness selected by other evidence entirely. It must never
    /// be a substring match: a harness whose rule claimed `some-claude-like`
    /// would silently divert another agent's traffic into its lane.
    fn harness_id(&self, user_agent: &str) -> Option<&'static str>;
}

/// Timeouts and per-consumer identifiers the pipeline needs.
///
/// The timeouts default to the values a production capture client runs. The
/// provider filter has no default on purpose — see [`CodexProviderFilter`] —
/// and neither does the User-Agent resolver, because a default would have to
/// name the harnesses this module is trying not to know about.
#[derive(Debug, Clone)]
pub struct AttributionConfig {
    /// Bound on the Claude-lane wait.
    pub claude_timeout: Duration,
    /// Poll interval on the Claude lane.
    pub claude_poll: Duration,
    /// Bound on the Codex-lane wait.
    pub codex_timeout: Duration,
    /// Poll interval on the Codex lane.
    pub codex_poll: Duration,
    /// How recently a Codex rollout file must have been modified to count.
    pub codex_recent_window: time::Duration,
    /// Which Codex provider ids are ours.
    pub codex_provider: CodexProviderFilter,
    /// How a `User-Agent` resolves to a harness id.
    ///
    /// Shared rather than owned so the config stays cheap to clone per
    /// request; resolution is a pure read.
    pub user_agents: std::sync::Arc<dyn UserAgentHarness>,
}

impl AttributionConfig {
    /// Config with the production timeouts, the supplied provider filter, and
    /// the supplied User-Agent resolver.
    #[must_use]
    pub fn new(
        codex_provider: CodexProviderFilter,
        user_agents: impl UserAgentHarness + 'static,
    ) -> Self {
        Self {
            claude_timeout: DEFAULT_CLAUDE_TIMEOUT,
            claude_poll: DEFAULT_CLAUDE_POLL,
            codex_timeout: DEFAULT_CODEX_TIMEOUT,
            codex_poll: DEFAULT_CODEX_POLL,
            codex_recent_window: DEFAULT_CODEX_RECENT_WINDOW,
            codex_provider,
            user_agents: std::sync::Arc::new(user_agents),
        }
    }
}

/// Memoized fork-parent lookups, keyed by harness session id.
///
/// Fork-parent discovery scans the harness transcript tree under a 25 ms
/// budget. The parent relationship is immutable once a session exists, so a
/// discovered answer — **including `None`** — is final and is never
/// re-discovered for the same session id. Without this, every request in a
/// session would re-scan.
#[derive(Debug, Default)]
pub struct ForkParentCache {
    entries: Mutex<HashMap<String, ForkParentEntry>>,
}

/// A cached discovery outcome. A found parent is permanent (transcripts are
/// append-only; lineage does not change). A miss is NOT: the first requests
/// of a session race the transcript's appearance on disk, so a negative is
/// retried on a short interval until a give-up window closes, and only then
/// becomes permanent.
#[derive(Debug, Clone)]
enum ForkParentEntry {
    Parent(String),
    Negative { first: Instant, last_probe: Instant },
    PermanentlyNone,
}

/// How often a cached negative is re-probed while the give-up window is open.
const NEGATIVE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// How long a session's parent may keep being re-probed after the first miss.
/// Covers the transcript-appearance race at session start without scanning
/// forever for the (majority) sessions that genuinely have no parent.
const NEGATIVE_GIVE_UP: Duration = Duration::from_secs(30);

impl ForkParentCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `Some(outcome)` when the cache answers, `None` when the
    /// caller should (re-)discover. A cached negative answers only while its
    /// retry interval has not elapsed; past the give-up window it hardens
    /// into a permanent negative.
    fn get(&self, sid: &str) -> Option<Option<String>> {
        // `PoisonError::into_inner` recovers the guard even if a prior holder
        // panicked. The cache is best-effort and memory-only, so the worst
        // case from a poisoned read is one redundant discovery — much cheaper
        // than failing the request.
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match entries.get(sid) {
            None => None,
            Some(ForkParentEntry::Parent(parent)) => Some(Some(parent.clone())),
            Some(ForkParentEntry::PermanentlyNone) => Some(None),
            Some(ForkParentEntry::Negative { last_probe, .. }) => {
                // Hardening happens in `insert`, AFTER a probe — never here.
                // If it happened on expiry alone, a window that lapses
                // between probes would go permanent without one final look,
                // and a transcript appearing in that last interval would
                // stay undiscovered forever.
                if last_probe.elapsed() < NEGATIVE_RETRY_INTERVAL {
                    Some(None)
                } else {
                    None // interval elapsed: let the caller (re-)probe
                }
            }
        }
    }

    fn insert(&self, sid: String, parent: Option<String>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match parent {
            Some(parent) => {
                entries.insert(sid, ForkParentEntry::Parent(parent));
            }
            None => {
                let first = match entries.get(&sid) {
                    // A found parent is immutable lineage: a slower
                    // concurrent probe that missed must never erase it.
                    Some(ForkParentEntry::Parent(_)) => return,
                    Some(ForkParentEntry::Negative { first, .. }) => *first,
                    _ => Instant::now(),
                };
                if first.elapsed() >= NEGATIVE_GIVE_UP {
                    // This probe ran past the give-up window and still
                    // missed — the guaranteed final look. Harden.
                    entries.insert(sid, ForkParentEntry::PermanentlyNone);
                    return;
                }
                entries.insert(
                    sid,
                    ForkParentEntry::Negative {
                        first,
                        last_probe: Instant::now(),
                    },
                );
            }
        }
    }
}

/// The live state the pipeline reads: two watcher snapshots and the cache.
///
/// Snapshots are wait-free handles — the pipeline does one `load` per lookup
/// and never blocks a watcher tick.
#[derive(Debug, Clone)]
pub struct AttributionState {
    /// Claude candidate PIDs + parsed session metadata.
    pub claude_watcher: WatcherSnapshotHandle,
    /// Recently-seen Codex rollout files.
    pub codex_watcher: CodexWatcherSnapshotHandle,
    /// Memoized fork-parent lookups.
    pub fork_parents: std::sync::Arc<ForkParentCache>,
}

impl AttributionState {
    /// Bundle watcher handles with a fresh cache.
    #[must_use]
    pub fn new(
        claude_watcher: WatcherSnapshotHandle,
        codex_watcher: CodexWatcherSnapshotHandle,
    ) -> Self {
        Self {
            claude_watcher,
            codex_watcher,
            fork_parents: std::sync::Arc::new(ForkParentCache::new()),
        }
    }
}

/// The per-request facts the pipeline needs, extracted by the consumer.
///
/// Deliberately not an `http::Request`: the consumer has already parsed its own
/// route grammar and its own marker header name by this point, and passing the
/// whole request would invite the pipeline to start reading Paper-specific
/// headers itself.
///
/// Deliberately exhaustive, like [`Attributed`] and for the same reason.
/// Adding a field here breaks every consumer's literal, and that is the point:
/// a consumer that *has* the new evidence must be made to decide whether to
/// supply it. Letting the field default to "no evidence" would compile, and
/// would quietly capture less than the consumer is able to — one capture path
/// silently drifting below the other is precisely the parity failure this
/// crate exists to prevent, and unlike a compile error it announces nothing.
///
/// `Default` is still derived, for tests and for consumers that genuinely have
/// only a few facts to state.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestFacts<'a> {
    /// Peer address of the accepted loopback connection. The Claude lane maps
    /// this to a PID.
    pub peer: Option<SocketAddr>,
    /// The request's `User-Agent`, if any. Gates the Claude lane.
    pub user_agent: Option<&'a str>,
    /// Value of the consumer's Codex attribution marker header, if present and
    /// non-blank. The *name* of that header is the consumer's business.
    pub codex_marker: Option<&'a str>,
    /// The id of the rollout this Codex request says it belongs to, as
    /// resolved by [`super::codex::session::rollout_id`].
    ///
    /// Unlike [`Self::codex_marker`], the header names here are *harness*
    /// knowledge and so are the crate's — a consumer should pass
    /// `codex_session::rollout_id(request.headers())` rather than pick its
    /// own. This is the only per-request evidence that distinguishes threads
    /// within one Codex process, and without it a subagent family is
    /// indistinguishable by PID alone.
    ///
    /// `None` means *no evidence*, which leaves the conservative policy in
    /// force. It never means "matches nothing".
    ///
    /// Superseded by [`Self::codex_identity`] when that is supplied: the
    /// identity reads the same two headers and additionally withholds them
    /// from a request that contradicts itself.
    pub codex_rollout_id: Option<&'a str>,
    /// Whether the consumer's routing says this request is Codex traffic.
    pub codex_route: bool,
    /// Everything else the Codex request states about itself, parsed by
    /// [`CodexRequestIdentity::from_headers`].
    ///
    /// Passed as a reference rather than parsed here because the consumer
    /// needs the same value for its own diagnostics — and because it carries
    /// the consumer's per-request correlation id, which only the consumer can
    /// mint in a form its own records will agree with.
    ///
    /// `None` degrades gracefully: the identity-driven rungs stand down, the
    /// envelope carries no request-derived identifiers, and the lane behaves
    /// as it did before the identity vocabulary existed.
    pub codex_identity: Option<&'a CodexRequestIdentity>,
    /// Lifecycle-hook evidence, for consumers that have a hook lane.
    ///
    /// See [`CodexHookEvidence`] for why this is injected rather than assumed.
    pub codex_hook_evidence: Option<&'a dyn CodexHookEvidence>,
}

/// The outcome of attributing one request.
///
/// Deliberately exhaustive, unlike [`RequestFacts`]. A consumer that has not
/// been taught a newly added outcome should fail to build rather than fall
/// through a catch-all arm and file the turn as if nothing had happened —
/// silent unattribution is the failure mode this lane exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attributed {
    /// Claude traffic resolved to a session file, with any recovered lineage.
    Claude {
        /// The resolved `~/.claude/sessions/<pid>.json`.
        session: ClaudeSessionFile,
        /// Fork parent's harness session id, when lineage was recovered.
        parent_session_id: Option<String>,
    },
    /// Codex traffic, whether or not a rollout was resolved.
    ///
    /// The session is optional because a Codex request is self-describing:
    /// even with a cold watcher its headers name the thread and, for a
    /// sub-thread, the root session to key on. See the module docs on why the
    /// miss cases differ.
    Codex {
        /// The resolved rollout `session_meta`, when a rung answered.
        session: Option<CodexSessionFile>,
        /// What the request said about itself.
        identity: Box<CodexRequestIdentity>,
        /// The session was *configured* through the desktop app's plugin
        /// rather than launched, so it files under the `codex-app` harness.
        ///
        /// Decided by lifecycle-hook evidence for the id this envelope will
        /// key on — always the root for a sub-thread's request, matching the
        /// id the hook reported — so every turn of one session lands under one
        /// harness. Consumers with no hook lane never see it set.
        codex_app: bool,
    },
    /// Claude-lane miss: a known-shaped request we could not attribute.
    /// Emits the `harness_id: unknown` envelope.
    UnknownHarness,
    /// No assertion at all: emits no envelope and preserves whatever arrived.
    ///
    /// No longer produced by [`attribute`] — the Codex lane, which was its only
    /// source, now always emits at least the request's own account of itself.
    /// Retained because it is the only way to express "write nothing", which a
    /// consumer composing its own outcome may still need.
    Undecided,
}

impl Attributed {
    /// The envelope to stamp on the outbound request, or `None` when the
    /// pipeline declined to assert an identity (see the module docs on why the
    /// two miss cases differ).
    #[must_use]
    pub fn envelope(&self) -> Option<TapesAttribution> {
        match self {
            Self::Claude {
                session,
                parent_session_id,
            } => Some(TapesAttribution::from_session(
                session,
                parent_session_id.as_deref(),
            )),
            Self::Codex {
                session,
                identity,
                codex_app,
            } => {
                let mut envelope = match session {
                    Some(session) => codex_request::codex_envelope(session, identity),
                    None => codex_request::request_envelope(identity),
                };
                if *codex_app {
                    envelope.harness_id = HARNESS_ID_CODEX_APP.to_owned();
                }
                Some(envelope)
            }
            Self::UnknownHarness => Some(TapesAttribution::unknown()),
            Self::Undecided => None,
        }
    }

    /// Stamp this outcome's envelope onto an outbound request.
    ///
    /// Prefer this over stamping [`Self::envelope`] by hand: the unattributed
    /// case is not simply "write the unknown envelope". A harness that stamps
    /// its own complete `X-Tapes-*` envelope — an in-harness extension, which
    /// the peer-PID path cannot see — must have it **preserved**, because that
    /// envelope is better information than our own failure to attribute.
    /// Overwriting it with `harness_id: unknown` would discard a correct
    /// attribution and silently re-file those sessions. Any stale partial
    /// envelope is still cleared.
    pub fn stamp(
        &self,
        headers: &mut http::HeaderMap,
    ) -> Result<(), tapes_capture::envelope::HeaderError> {
        match self {
            // No assertion at all, so nothing is written — see the module docs
            // on the miss cases.
            Self::Undecided => Ok(()),
            Self::UnknownHarness => tapes_capture::envelope::inject_unattributed_envelope(headers),
            Self::Claude {
                session,
                parent_session_id,
            } => tapes_capture::envelope::inject_session_envelope(
                headers,
                session,
                parent_session_id.as_deref(),
            ),
            Self::Codex { .. } => match self.envelope() {
                Some(envelope) => {
                    tapes_capture::envelope::inject_tapes_attribution(headers, envelope)
                }
                None => Ok(()),
            },
        }
    }

    /// The harness-agnostic summary, for consumers that build an ingest
    /// payload directly rather than stamping headers.
    ///
    /// `auth_subject` is always `None` here — the pipeline discovers harness
    /// facts, and the acting subject is the consumer's to supply (a standalone
    /// client defaults it to `local:<user>`; on the platform the cloud edge
    /// stamps it from validated JWT claims).
    #[must_use]
    pub fn attribution(&self) -> Attribution {
        match self {
            Self::Claude {
                session,
                parent_session_id,
            } => Attribution {
                session_id: Some(session.session_id.clone()),
                parent_session_id: parent_session_id.clone(),
                cwd: session.cwd.clone(),
                auth_subject: None,
            },
            Self::Codex {
                session, identity, ..
            } => Attribution {
                session_id: codex_request::envelope_session_id(identity, session.as_ref())
                    .map(str::to_owned),
                parent_session_id: None,
                cwd: session.as_ref().and_then(|session| session.cwd.clone()),
                auth_subject: None,
            },
            Self::UnknownHarness | Self::Undecided => Attribution::default(),
        }
    }

    /// The resolved Claude session, when the Claude lane hit.
    ///
    /// Consumers use this for side effects an attributed request implies —
    /// paperd registers the session with its transcript uploader, because an
    /// attributed request is the proof that the session's traffic flows
    /// through it.
    #[must_use]
    pub fn claude_session(&self) -> Option<&ClaudeSessionFile> {
        match self {
            Self::Claude { session, .. } => Some(session),
            _ => None,
        }
    }

    /// The resolved Codex rollout, when a rung answered.
    #[must_use]
    pub fn codex_session(&self) -> Option<&CodexSessionFile> {
        match self {
            Self::Codex { session, .. } => session.as_ref(),
            _ => None,
        }
    }
}

/// An [`Attributed`] outcome plus the Codex ladder's working, for consumers
/// that record why a turn was attributed the way it was.
///
/// Separate from [`Attributed`] because the evidence is diagnostics, not
/// identity: it must not become something a consumer has to destructure to
/// stamp a header.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttributionOutcome {
    /// Who sent this request.
    pub attributed: Attributed,
    /// What the Codex ladder considered, for Codex traffic only.
    pub codex_evidence: Option<CodexSelectionEvidence>,
}

/// Attribute one request.
///
/// Dispatches to the Codex lane when the consumer says the request is Codex
/// traffic (by route or by marker), and to the Claude lane otherwise. Both
/// lanes are bounded: the Claude lane is additionally gated on the User-Agent
/// so a loopback `curl` or a health probe returns immediately without touching
/// the per-OS syscall path.
pub async fn attribute(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
) -> Attributed {
    attribute_with_evidence(state, config, facts)
        .await
        .attributed
}

/// Attribute one request and keep the Codex ladder's working.
///
/// Same algorithm as [`attribute`], which is a thin wrapper over it. A
/// consumer that journals attribution decisions — so an unresolved turn can be
/// repaired later — calls this instead of re-deriving the evidence itself.
pub async fn attribute_with_evidence(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
) -> AttributionOutcome {
    if facts.codex_route || facts.codex_marker.is_some() {
        let owned;
        let identity = match facts.codex_identity {
            Some(identity) => identity,
            None => {
                owned = CodexRequestIdentity::default();
                &owned
            }
        };
        let selected = codex_select::select(state, config, facts, identity).await;
        // The desktop-app split. A session with lifecycle-hook evidence was
        // configured through the app's plugin rather than launched, so it
        // files under `codex-app`, keeping app capture distinguishable from a
        // launched `codex` CLI. Keyed on the id the envelope will carry, so
        // every turn of one session lands under one harness.
        let codex_app = codex_request::envelope_session_id(identity, selected.session.as_ref())
            .is_some_and(|session_id| {
                facts
                    .codex_hook_evidence
                    .is_some_and(|hooks| hooks.has_hook_session(session_id))
            });
        return AttributionOutcome {
            attributed: Attributed::Codex {
                session: selected.session,
                identity: Box::new(identity.clone()),
                codex_app,
            },
            codex_evidence: Some(selected.evidence),
        };
    }

    let attributed = match attribute_claude(state, config, facts).await {
        Some(session) => {
            let parent_session_id = discover_parent_cached(state, &session).await;
            Attributed::Claude {
                session,
                parent_session_id,
            }
        }
        None => Attributed::UnknownHarness,
    };
    AttributionOutcome {
        attributed,
        codex_evidence: None,
    }
}

/// Claude lane: User-Agent gate, then a bounded poll for the session file.
///
/// The gate asks the injected resolver which harness the User-Agent names and
/// takes the lane only when the answer is the harness whose sessions this
/// lane's watcher indexes. Naming that harness here is not the dependency the
/// inversion removed: [`HARNESS_ID_CLAUDE`] is envelope vocabulary, shared by
/// every capture client, whereas the *rule* for matching it is a registry
/// declaration that changes whenever a harness is added or its User-Agent
/// changes. The lane keeps the former and no longer holds the latter.
async fn attribute_claude(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
) -> Option<ClaudeSessionFile> {
    let harness = facts
        .user_agent
        .and_then(|ua| config.user_agents.harness_id(ua));
    if harness != Some(HARNESS_ID_CLAUDE) {
        return None;
    }
    let peer = facts.peer?;

    let deadline = Instant::now() + config.claude_timeout;
    loop {
        if let Some(session) = attribute_claude_once(state, peer) {
            return Some(session);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        tokio::time::sleep(config.claude_poll.min(remaining)).await;
    }
}

fn attribute_claude_once(state: &AttributionState, peer: SocketAddr) -> Option<ClaudeSessionFile> {
    // A single `load` reads the candidate set and the metadata map atomically
    // across a watcher swap — see `WatcherSnapshot` for why bundling matters.
    let snapshot = state.claude_watcher.load_full();
    if snapshot.candidate_pids.is_empty() {
        return None;
    }
    let mut candidates: Vec<i32> = snapshot.candidate_pids.iter().copied().collect();
    candidates.sort_unstable();
    let pid = peer_pid::lookup(&candidates, peer).pid?;
    snapshot.pid_metadata.get(&pid).cloned()
}

/// Look up or discover the fork parent, memoizing the result.
async fn discover_parent_cached(
    state: &AttributionState,
    session: &ClaudeSessionFile,
) -> Option<String> {
    let sid = session.session_id.clone();
    if let Some(cached) = state.fork_parents.get(&sid) {
        return cached;
    }

    // Cache miss. Two concurrent requests for the same new sid may both
    // discover; the transcript is deterministic so they agree, and the
    // redundant scan is the only cost.
    let Some(cwd) = session.cwd.as_deref() else {
        // No cwd means no transcript to locate. Cache the `None` so every
        // subsequent request in this session does not retry.
        state.fork_parents.insert(sid, None);
        return None;
    };
    let parent = fork_parent::discover_parent(cwd, &sid).await;
    state.fork_parents.insert(sid, parent.clone());
    parent
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::attribution::codex::session as codex_session;
    use crate::attribution::{CodexWatcherSnapshot, WatcherSnapshot};
    use arc_swap::ArcSwap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tapes_capture::envelope::{HARNESS_ID_CLAUDE, HARNESS_ID_CODEX, HARNESS_ID_UNKNOWN};

    fn filter() -> CodexProviderFilter {
        CodexProviderFilter::new("paper-openai")
    }

    fn config() -> AttributionConfig {
        AttributionConfig::new(filter(), crate::harness::RegistryUserAgents)
    }

    fn state_with(claude: WatcherSnapshot, codex: CodexWatcherSnapshot) -> AttributionState {
        AttributionState::new(
            Arc::new(ArcSwap::from_pointee(claude)),
            Arc::new(ArcSwap::from_pointee(codex)),
        )
    }

    fn empty_state() -> AttributionState {
        state_with(WatcherSnapshot::default(), CodexWatcherSnapshot::default())
    }

    fn peer() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49152)
    }

    fn claude_session(sid: &str) -> ClaudeSessionFile {
        let raw = format!(r#"{{"pid":4242,"sessionId":"{sid}","cwd":"/tmp"}}"#);
        serde_json::from_str(&raw).unwrap()
    }

    fn codex_file(sid: &str, provider: &str, age: time::Duration) -> CodexSessionFile {
        let now = time::OffsetDateTime::now_utc();
        CodexSessionFile {
            session_id: sid.to_owned(),
            // A root rollout: these tests exercise recency and provider
            // filtering, neither of which reads lineage.
            root_session_id: None,
            parent_thread_id: None,
            subagent_kind: None,
            timestamp: now - age,
            modified_at: Some(now - age),
            cwd: Some("/tmp".to_owned()),
            originator: Some("codex_cli_rs".to_owned()),
            cli_version: Some("0.9.0".to_owned()),
            source: None,
            thread_source: None,
            model_provider: Some(provider.to_owned()),
            path: PathBuf::from(format!("/tmp/rollout-{sid}.jsonl")),
        }
    }

    // --- the UA gate ----------------------------------------------------

    /// The gate as the lane sees it: a User-Agent reaches the Claude lane only
    /// when the injected resolver names Claude. Driven through the real
    /// registry resolver, so this covers the wiring as well as the rule — the
    /// rule itself is pinned in `crate::harness`.
    #[test]
    fn the_lane_gate_matches_any_casing_but_only_as_a_prefix() {
        let claims = |ua: &str| config().user_agents.harness_id(ua) == Some(HARNESS_ID_CLAUDE);

        assert!(claims("claude-cli/2.1.145"));
        // The exact spelling Anthropic shipped on at least one beta build.
        assert!(claims("Claude-CLI/2.1.145"));
        assert!(claims("CLAUDE/0.0"));

        assert!(!claims("curl/8.0"));
        assert!(!claims("OpenAI/python"));
        assert!(!claims(""));
        // Substring matching would be wrong — we want a prefix.
        assert!(!claims("some-claude-like"));
    }

    /// A resolver that names a *different* harness must not open the Claude
    /// lane. The gate compares the resolved id rather than merely checking
    /// that something resolved, and this is the difference between the two:
    /// with a substring-ish "did anything match" gate, another agent's traffic
    /// would be handed to Claude's session-file watcher.
    #[tokio::test(start_paused = true)]
    async fn a_user_agent_naming_another_harness_does_not_take_the_claude_lane() {
        #[derive(Debug)]
        struct AlwaysCodex;
        impl UserAgentHarness for AlwaysCodex {
            fn harness_id(&self, _user_agent: &str) -> Option<&'static str> {
                Some(HARNESS_ID_CODEX)
            }
        }

        let state = empty_state();
        let config = AttributionConfig::new(filter(), AlwaysCodex);
        let facts = RequestFacts {
            peer: Some(peer()),
            user_agent: Some("claude-cli/2.1.145"),
            ..RequestFacts::default()
        };

        // Returning immediately is the assertion: a lane it may not take is
        // not a lane it waits on.
        let got = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            attribute(&state, &config, facts),
        )
        .await
        .expect("a foreign harness id must not open the claude lane");
        assert_eq!(got, Attributed::UnknownHarness);
    }

    #[tokio::test(start_paused = true)]
    async fn non_claude_callers_do_not_pay_the_bounded_wait() {
        let state = empty_state();
        let facts = RequestFacts {
            peer: Some(peer()),
            user_agent: Some("curl/8.0"),
            ..RequestFacts::default()
        };

        // start_paused means the 2 s wait would hang forever against a real
        // timeout; returning immediately is the assertion.
        let got = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            attribute(&state, &config(), facts),
        )
        .await
        .expect("non-claude callers must not wait");
        assert_eq!(got, Attributed::UnknownHarness);
    }

    /// A live loopback connection, so the peer-PID lookup has a real socket to
    /// resolve. Both endpoints are returned and must stay alive for the
    /// duration of the test — closing either unbinds the port.
    fn live_peer() -> (SocketAddr, std::net::TcpStream, std::net::TcpStream) {
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let server_addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(server_addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        let peer = client.local_addr().expect("peer addr");
        (peer, client, server)
    }

    fn snapshot_with_session(pid: i32, session_id: &str) -> WatcherSnapshot {
        let mut snapshot = WatcherSnapshot::default();
        snapshot.candidate_pids.insert(pid);
        let raw = format!(r#"{{"pid":{pid},"sessionId":"{session_id}","cwd":"/tmp"}}"#);
        snapshot
            .pid_metadata
            .insert(pid, serde_json::from_str(&raw).expect("parse"));
        snapshot
    }

    #[tokio::test(start_paused = true)]
    async fn a_claude_caller_waits_out_the_whole_budget_before_giving_up() {
        let state = empty_state();
        let facts = RequestFacts {
            peer: Some(peer()),
            user_agent: Some("claude-cli/2.1.161"),
            ..RequestFacts::default()
        };

        let config = config();
        let mut task = std::pin::pin!(attribute(&state, &config, facts));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), task.as_mut())
                .await
                .is_err(),
            "attribution must still be waiting before the budget expires",
        );

        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert_eq!(task.await, Attributed::UnknownHarness);
    }

    #[tokio::test(start_paused = true)]
    async fn a_session_file_appearing_mid_wait_is_picked_up() {
        // This is the race the bounded wait exists for: a harness the consumer
        // just launched writes its session file *after* its first request
        // leaves. Without the wait, that request — which for a `-p` invocation
        // may be the only one — lands in a separate synthetic session.
        let state = empty_state();
        let (peer, _client, _server) = live_peer();
        let pid = std::process::id() as i32;
        let facts = RequestFacts {
            peer: Some(peer),
            user_agent: Some("claude-cli/2.1.161"),
            ..RequestFacts::default()
        };

        let config = config();
        let mut task = std::pin::pin!(attribute(&state, &config, facts));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), task.as_mut())
                .await
                .is_err(),
            "attribution must wait while the session file is absent",
        );

        state
            .claude_watcher
            .store(std::sync::Arc::new(snapshot_with_session(
                pid,
                "mid-wait-session",
            )));

        // Bounded by the attribution budget itself rather than a short cliff:
        // the real peer-PID scan can miss a just-created socket on its first
        // try, and the task then re-sleeps a poll interval. Auto-advance must
        // be free to drive those retries.
        let got = tokio::time::timeout(DEFAULT_CLAUDE_TIMEOUT, task.as_mut())
            .await
            .expect("the session must be found inside the budget");
        assert_eq!(
            got.attribution().session_id.as_deref(),
            Some("mid-wait-session"),
        );
    }

    // --- the miss asymmetry ---------------------------------------------

    #[test]
    fn claude_lane_miss_emits_an_unknown_envelope() {
        let envelope = Attributed::UnknownHarness
            .envelope()
            .expect("claude-lane miss must still emit an envelope");
        assert_eq!(envelope.harness_id, HARNESS_ID_UNKNOWN);
        assert_eq!(envelope.session_id, None);
    }

    #[test]
    fn an_explicit_no_assertion_emits_no_envelope_at_all() {
        // Not "an envelope with no session id" — no envelope. The Codex lane
        // no longer produces this outcome (its misses still carry the
        // request's own account of itself), but the outcome remains the one
        // way to say "write nothing".
        assert!(Attributed::Undecided.envelope().is_none());
    }

    #[test]
    fn a_codex_miss_still_carries_the_request_identity_it_had() {
        // The repair contract: a turn nobody could attribute must still be
        // findable later, which means its correlation id has to go out.
        let attributed = Attributed::Codex {
            session: None,
            identity: Box::new(
                CodexRequestIdentity::default().with_correlation_id("correlation-1"),
            ),
            codex_app: false,
        };
        let envelope = attributed.envelope().expect("a codex miss still stamps");
        assert_eq!(envelope.harness_id, HARNESS_ID_CODEX);
        assert_eq!(envelope.session_id, None);
        assert_eq!(
            envelope.metadata[crate::attribution::codex::request::REQUEST_CORRELATION_METADATA_KEY],
            "correlation-1",
        );
    }

    #[test]
    fn hook_evidence_files_a_session_under_the_desktop_app_harness() {
        let attributed = Attributed::Codex {
            session: Some(codex_file(
                "sid-app",
                "paper-openai",
                time::Duration::seconds(1),
            )),
            identity: Box::new(CodexRequestIdentity::default()),
            codex_app: true,
        };
        let envelope = attributed.envelope().unwrap();
        assert_eq!(
            envelope.harness_id,
            tapes_capture::envelope::HARNESS_ID_CODEX_APP
        );
        assert_eq!(envelope.session_id.as_deref(), Some("sid-app"));
    }

    // --- provider filtering ---------------------------------------------

    #[test]
    fn provider_filter_matches_exact_and_suffixed_ids_only() {
        let f = filter();
        assert!(f.matches(Some("paper-openai")));
        assert!(f.matches(Some("paper-openai-transparent")));

        assert!(!f.matches(Some("paper-openai2")));
        assert!(!f.matches(Some("other-openai")));
        assert!(!f.matches(Some("")));
        assert!(!f.matches(None));
    }

    #[test]
    fn provider_filter_is_per_consumer_not_paper_specific() {
        // The whole point of parameterizing: a standalone client names its
        // provider something else and must still attribute its own traffic
        // while ignoring Paper's.
        let f = CodexProviderFilter::new("tapesctl-openai");
        assert!(f.matches(Some("tapesctl-openai")));
        assert!(f.matches(Some("tapesctl-openai-local")));
        assert!(!f.matches(Some("paper-openai")));
    }

    #[tokio::test(start_paused = true)]
    async fn codex_ignores_sessions_belonging_to_another_provider() {
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot.sessions.push(codex_file(
            "other",
            "some-other-provider",
            time::Duration::seconds(1),
        ));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_route: true,
            ..RequestFacts::default()
        };
        assert_eq!(
            attribute(&state, &config(), facts).await.codex_session(),
            None
        );
    }

    // --- the recent-session fallback ------------------------------------

    #[tokio::test(start_paused = true)]
    async fn unmarked_codex_falls_back_to_a_single_recent_session() {
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot.sessions.push(codex_file(
            "sole",
            "paper-openai",
            time::Duration::seconds(30),
        ));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_route: true,
            ..RequestFacts::default()
        };
        let got = attribute(&state, &config(), facts).await;
        assert_eq!(
            got.codex_session().map(|s| s.session_id.as_str()),
            Some("sole"),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ambiguous_recent_sessions_are_refused_rather_than_guessed() {
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot
            .sessions
            .push(codex_file("a", "paper-openai", time::Duration::seconds(30)));
        snapshot
            .sessions
            .push(codex_file("b", "paper-openai", time::Duration::seconds(10)));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_route: true,
            ..RequestFacts::default()
        };
        // Newest-wins would be wrong here: neither session presented evidence
        // tying it to *this* request.
        assert_eq!(
            attribute(&state, &config(), facts).await.codex_session(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_fallback_refuses_a_lone_session_that_is_not_the_named_rollout() {
        // The discovery-timeout shape: the child rollout the request names is
        // not on disk yet, and the only visible session is the parent. Handing
        // the child's turn to the parent would be permanent cross-thread
        // attribution — the exact hole the lanes already close.
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot.sessions.push(codex_file(
            "parent",
            "paper-openai",
            time::Duration::seconds(30),
        ));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_route: true,
            codex_rollout_id: Some("child-not-yet-on-disk"),
            ..RequestFacts::default()
        };
        assert_eq!(
            attribute(&state, &config(), facts).await.codex_session(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_fallback_selects_the_named_rollout_among_several() {
        // With rollout evidence the fallback no longer needs to refuse a
        // family: the named session is a decision, not a guess.
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot
            .sessions
            .push(codex_file("a", "paper-openai", time::Duration::seconds(30)));
        snapshot
            .sessions
            .push(codex_file("b", "paper-openai", time::Duration::seconds(10)));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_route: true,
            codex_rollout_id: Some("a"),
            ..RequestFacts::default()
        };
        let got = attribute(&state, &config(), facts).await;
        assert_eq!(
            got.codex_session().map(|s| s.session_id.as_str()),
            Some("a"),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stale_sessions_are_outside_the_recent_window() {
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot.sessions.push(codex_file(
            "stale",
            "paper-openai",
            time::Duration::hours(3),
        ));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_route: true,
            ..RequestFacts::default()
        };
        assert_eq!(
            attribute(&state, &config(), facts).await.codex_session(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_supplied_marker_never_falls_back_to_a_recent_session() {
        // One recent session exists, but the marker named a provider that is
        // not it. An explicit launch signal must not be overridden by a guess.
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot.sessions.push(codex_file(
            "sole",
            "paper-openai",
            time::Duration::seconds(30),
        ));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_marker: Some("paper-openai-somethingelse"),
            codex_route: true,
            ..RequestFacts::default()
        };
        assert_eq!(
            attribute(&state, &config(), facts).await.codex_session(),
            None
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_marker_selects_its_own_session_among_several() {
        let mut snapshot = CodexWatcherSnapshot::default();
        snapshot
            .sessions
            .push(codex_file("a", "paper-openai", time::Duration::seconds(30)));
        snapshot.sessions.push(codex_file(
            "wanted",
            "paper-openai-transparent",
            time::Duration::seconds(30),
        ));
        let state = state_with(WatcherSnapshot::default(), snapshot);

        let facts = RequestFacts {
            codex_marker: Some("paper-openai-transparent"),
            codex_route: true,
            ..RequestFacts::default()
        };
        let got = attribute(&state, &config(), facts).await;
        assert_eq!(
            got.codex_session().map(|s| s.session_id.as_str()),
            Some("wanted"),
        );
    }

    #[test]
    fn rollout_id_prefers_the_thread_over_the_root_session() {
        // On a subagent turn Codex sends BOTH: `session-id` stays pinned to the
        // root while `thread-id` is the child's own rollout. Reading
        // `session-id` first would attribute every subagent turn to the parent.
        let mut headers = http::HeaderMap::new();
        headers.insert("session-id", http::HeaderValue::from_static("sid-parent"));
        headers.insert("thread-id", http::HeaderValue::from_static("sid-child-a"));
        assert_eq!(codex_session::rollout_id(&headers), Some("sid-child-a"));
    }

    #[test]
    fn rollout_id_falls_back_to_the_session_when_no_thread_is_named() {
        // A main-thread turn on a Codex build that omits `thread-id`.
        let mut headers = http::HeaderMap::new();
        headers.insert("session-id", http::HeaderValue::from_static("sid-parent"));
        assert_eq!(codex_session::rollout_id(&headers), Some("sid-parent"));
    }

    #[test]
    fn rollout_id_treats_a_blank_header_as_absent() {
        // Absent must mean "no evidence", never "matches nothing" — a blank
        // value that reached the matcher would refuse every candidate.
        let mut headers = http::HeaderMap::new();
        headers.insert("thread-id", http::HeaderValue::from_static("  "));
        headers.insert("session-id", http::HeaderValue::from_static("sid-parent"));
        assert_eq!(codex_session::rollout_id(&headers), Some("sid-parent"));

        assert_eq!(codex_session::rollout_id(&http::HeaderMap::new()), None);
    }

    // A transient miss must not become permanent: the first requests of a
    // session race the transcript's appearance on disk. Within the give-up
    // window an elapsed retry interval re-opens discovery; once a parent is
    // found it sticks.
    #[tokio::test(start_paused = true)]
    async fn negative_fork_parent_cache_retries_then_hardens() {
        let cache = ForkParentCache::new();
        cache.insert("sid".into(), None);
        // Immediately after the miss: answered negative, no re-probe storm.
        assert_eq!(cache.get("sid"), Some(None));
        // Retry interval elapsed: the cache steps aside for a re-probe.
        tokio::time::advance(NEGATIVE_RETRY_INTERVAL + Duration::from_millis(10)).await;
        assert_eq!(cache.get("sid"), None);
        // The re-probe finds the parent late: permanent from here.
        cache.insert("sid".into(), Some("parent".into()));
        assert_eq!(cache.get("sid"), Some(Some("parent".into())));
    }

    // Two concurrent probes race: the winner finds the parent, the loser
    // misses. The miss must never erase the found parent — lineage is
    // immutable once discovered.
    #[tokio::test(start_paused = true)]
    async fn negative_insert_never_erases_a_found_parent() {
        let cache = ForkParentCache::new();
        cache.insert("sid".into(), Some("parent".into()));
        cache.insert("sid".into(), None);
        assert_eq!(cache.get("sid"), Some(Some("parent".into())));
    }

    #[tokio::test(start_paused = true)]
    async fn negative_fork_parent_cache_gives_up_only_after_a_final_probe() {
        let cache = ForkParentCache::new();
        cache.insert("sid".into(), None);
        tokio::time::advance(NEGATIVE_GIVE_UP + Duration::from_secs(1)).await;
        // The window lapsed between probes: the cache must grant ONE final
        // probe rather than hardening on expiry alone — a transcript that
        // appeared during the last interval is still discoverable here.
        assert_eq!(cache.get("sid"), None);
        // The final probe found it late: lineage recovered, permanent.
        cache.insert("sid".into(), Some("parent".into()));
        assert_eq!(cache.get("sid"), Some(Some("parent".into())));

        // And when the final probe also misses, THEN it hardens.
        let done = ForkParentCache::new();
        done.insert("done".into(), None);
        tokio::time::advance(NEGATIVE_GIVE_UP + Duration::from_secs(1)).await;
        assert_eq!(done.get("done"), None);
        done.insert("done".into(), None);
        assert_eq!(done.get("done"), Some(None));
        tokio::time::advance(NEGATIVE_RETRY_INTERVAL * 5).await;
        assert_eq!(
            done.get("done"),
            Some(None),
            "hardened after the final probe missed"
        );
    }

    // --- envelopes -------------------------------------------------------

    #[test]
    fn codex_envelope_carries_the_metadata_ingest_stores() {
        let mut session = codex_file("sid-1", "paper-openai", time::Duration::seconds(5));
        session.source = Some("cli".to_owned());
        session.thread_source = Some("main".to_owned());

        let envelope = crate::attribution::codex::request::codex_envelope(
            &session,
            &CodexRequestIdentity::default(),
        );
        assert_eq!(envelope.harness_id, HARNESS_ID_CODEX);
        assert_eq!(envelope.session_id.as_deref(), Some("sid-1"));
        assert_eq!(envelope.version.as_deref(), Some("0.9.0"));
        assert_eq!(envelope.cwd.as_deref(), Some("/tmp"));
        assert_eq!(envelope.metadata["originator"], "codex_cli_rs");
        assert_eq!(envelope.metadata["source"], "cli");
        assert_eq!(envelope.metadata["threadSource"], "main");
        assert_eq!(envelope.metadata["modelProvider"], "paper-openai");
        // The operator's join key back to the rollout file on disk.
        assert_eq!(
            envelope.metadata["transcriptPath"],
            "/tmp/rollout-sid-1.jsonl",
        );
    }

    #[test]
    fn absent_codex_metadata_fields_are_omitted_not_nulled() {
        let session = codex_file("sid-2", "paper-openai", time::Duration::seconds(5));
        let envelope = crate::attribution::codex::request::codex_envelope(
            &session,
            &CodexRequestIdentity::default(),
        );
        assert!(!envelope.metadata.contains_key("source"));
        assert!(!envelope.metadata.contains_key("threadSource"));
    }

    #[test]
    fn claude_envelope_carries_lineage_when_recovered() {
        let attributed = Attributed::Claude {
            session: claude_session("sid-claude"),
            parent_session_id: Some("sid-parent".to_owned()),
        };
        let envelope = attributed.envelope().unwrap();
        assert_eq!(envelope.harness_id, HARNESS_ID_CLAUDE);
        assert_eq!(envelope.session_id.as_deref(), Some("sid-claude"));
        assert_eq!(envelope.parent_sid.as_deref(), Some("sid-parent"));
    }

    // --- stamping --------------------------------------------------------

    fn headers_with(pairs: &[(&'static str, &'static str)]) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_static(value),
            );
        }
        headers
    }

    #[test]
    fn an_unattributed_request_preserves_a_complete_inbound_envelope() {
        // A harness that stamps its own envelope knows more than our failed
        // peer-PID lookup does. Overwriting it with `unknown` would discard a
        // correct attribution and re-file the session.
        let mut headers = headers_with(&[
            ("x-tapes-harness-id", "pi"),
            ("x-tapes-harness-session-id", "sid-from-harness"),
        ]);

        Attributed::UnknownHarness.stamp(&mut headers).unwrap();

        assert_eq!(headers["x-tapes-harness-id"], "pi");
        assert_eq!(headers["x-tapes-harness-session-id"], "sid-from-harness");
    }

    #[test]
    fn an_unattributed_request_without_an_inbound_envelope_is_marked_unknown() {
        let mut headers = http::HeaderMap::new();
        Attributed::UnknownHarness.stamp(&mut headers).unwrap();
        assert_eq!(headers["x-tapes-harness-id"], HARNESS_ID_UNKNOWN);
    }

    #[test]
    fn an_undecided_codex_request_is_left_entirely_unstamped() {
        let mut headers = http::HeaderMap::new();
        Attributed::Undecided.stamp(&mut headers).unwrap();
        assert!(headers.is_empty(), "got: {headers:?}");
    }

    #[test]
    fn an_attributed_claude_request_overrides_whatever_arrived() {
        // Here we *do* know better than the inbound headers.
        let mut headers = headers_with(&[("x-tapes-harness-id", "pi")]);
        Attributed::Claude {
            session: claude_session("sid-claude"),
            parent_session_id: None,
        }
        .stamp(&mut headers)
        .unwrap();

        assert_eq!(headers["x-tapes-harness-id"], HARNESS_ID_CLAUDE);
        assert_eq!(headers["x-tapes-harness-session-id"], "sid-claude");
    }

    // --- the harness-agnostic summary ------------------------------------

    #[test]
    fn summary_leaves_auth_subject_to_the_consumer() {
        let attributed = Attributed::Claude {
            session: claude_session("sid-claude"),
            parent_session_id: Some("sid-parent".to_owned()),
        };
        let summary = attributed.attribution();
        assert_eq!(summary.session_id.as_deref(), Some("sid-claude"));
        assert_eq!(summary.parent_session_id.as_deref(), Some("sid-parent"));
        assert_eq!(summary.cwd.as_deref(), Some("/tmp"));
        // The pipeline discovers harness facts; the subject is not one.
        assert_eq!(summary.auth_subject, None);
    }

    #[test]
    fn summary_of_a_miss_is_entirely_unknown() {
        assert_eq!(
            Attributed::UnknownHarness.attribution(),
            Attribution::default()
        );
        assert_eq!(Attributed::Undecided.attribution(), Attribution::default());
    }

    // --- the fork-parent cache -------------------------------------------

    #[tokio::test]
    async fn a_session_without_a_cwd_caches_its_none() {
        let state = empty_state();
        let mut session = claude_session("sid-nocwd");
        session.cwd = None;

        assert_eq!(discover_parent_cached(&state, &session).await, None);
        // Cached, so a second request in this session does not re-scan.
        assert_eq!(state.fork_parents.get("sid-nocwd"), Some(None));
    }

    #[tokio::test]
    async fn a_cached_parent_short_circuits_discovery() {
        let state = empty_state();
        state
            .fork_parents
            .insert("sid-x".to_owned(), Some("sid-parent".to_owned()));
        let session = claude_session("sid-x");
        assert_eq!(
            discover_parent_cached(&state, &session).await,
            Some("sid-parent".to_owned()),
        );
    }
}
