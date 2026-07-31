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
//! * **Claude lane.** Gate on a `claude*` User-Agent, then poll the watcher
//!   snapshot until the peer PID resolves to a parsed
//!   `~/.claude/sessions/<pid>.json`. On a hit, recover fork-parent lineage
//!   (cached per session id). A miss yields [`Attributed::UnknownHarness`].
//! * **Codex lane.** Codex writes no PID-indexed session file, so identity is
//!   recovered from the rollout JSONL a live `codex` process holds open: try
//!   the launch marker first, then the peer PID's open files, then — only when
//!   no marker was supplied — fall back to a single recent session. A miss
//!   yields [`Attributed::Undecided`].
//!
//! ## Why the two misses differ
//!
//! [`Attributed::UnknownHarness`] emits an envelope that says "harness
//! unknown"; [`Attributed::Undecided`] emits **no envelope at all**. This
//! asymmetry is inherited from paperd and is deliberate. A Claude-lane miss is
//! a *known* harness whose session id we failed to resolve, and ingest should
//! record the turn under a synthetic session. A Codex-lane miss is ambiguous —
//! the recent-session fallback refuses to guess between concurrent Codex
//! sessions rather than attach traffic to the wrong one — and stamping
//! `harness_id: codex` with no session id would assert an identity the
//! pipeline just declined to assert. Encoding the difference in the type,
//! rather than in each consumer's `if let`, is why [`Attributed::envelope`]
//! returns an `Option`.
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
//!   pointed at some other provider.
//!
//! That last one is why [`CodexProviderFilter`] exists rather than a hardcoded
//! `paper-openai` test: a standalone `tapesctl` names its provider something
//! else entirely, and a shared crate that only recognised Paper's spelling
//! would silently attribute nothing for every other consumer.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, PoisonError};

use tokio::time::{Duration, Instant};
use tracing::warn;

use super::{
    Attribution, ClaudeSessionFile, CodexSessionFile, CodexWatcherSnapshotHandle,
    WatcherSnapshotHandle, codex_session, fork_parent, open_jsonl_sessions_by_pid, peer_pid,
};
use crate::envelope::TapesAttribution;

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

/// Timeouts and per-consumer identifiers the pipeline needs.
///
/// The timeouts default to the values paperd runs in production. The provider
/// filter has no default on purpose — see [`CodexProviderFilter`].
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
}

impl AttributionConfig {
    /// Config with the production timeouts and the supplied provider filter.
    #[must_use]
    pub fn new(codex_provider: CodexProviderFilter) -> Self {
        Self {
            claude_timeout: DEFAULT_CLAUDE_TIMEOUT,
            claude_poll: DEFAULT_CLAUDE_POLL,
            codex_timeout: DEFAULT_CODEX_TIMEOUT,
            codex_poll: DEFAULT_CODEX_POLL,
            codex_recent_window: DEFAULT_CODEX_RECENT_WINDOW,
            codex_provider,
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
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
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
    /// Whether the consumer's routing says this request is Codex traffic.
    pub codex_route: bool,
}

/// The outcome of attributing one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attributed {
    /// Claude traffic resolved to a session file, with any recovered lineage.
    Claude {
        /// The resolved `~/.claude/sessions/<pid>.json`.
        session: ClaudeSessionFile,
        /// Fork parent's harness session id, when lineage was recovered.
        parent_session_id: Option<String>,
    },
    /// Codex traffic resolved to a rollout file.
    Codex {
        /// The resolved rollout `session_meta`.
        session: CodexSessionFile,
    },
    /// Claude-lane miss: a known-shaped request we could not attribute.
    /// Emits the `harness_id: unknown` envelope.
    UnknownHarness,
    /// Codex-lane miss: the pipeline declined to guess. Emits no envelope.
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
            } => Some(TapesAttribution::claude(
                session,
                parent_session_id.as_deref(),
            )),
            Self::Codex { session } => Some(codex_envelope(session)),
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
    pub fn stamp(&self, headers: &mut http::HeaderMap) -> Result<(), crate::envelope::HeaderError> {
        match self {
            // The pipeline declined to assert an identity, so it writes
            // nothing at all — see the module docs on the two misses.
            Self::Undecided => Ok(()),
            Self::UnknownHarness => crate::envelope::inject_tapes_headers(headers, None, None),
            Self::Claude {
                session,
                parent_session_id,
            } => crate::envelope::inject_tapes_attribution(
                headers,
                TapesAttribution::claude(session, parent_session_id.as_deref()),
            ),
            Self::Codex { session } => {
                crate::envelope::inject_tapes_attribution(headers, codex_envelope(session))
            }
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
            Self::Codex { session } => Attribution {
                session_id: Some(session.session_id.clone()),
                parent_session_id: None,
                cwd: session.cwd.clone(),
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

    /// The resolved Codex session, when the Codex lane hit.
    #[must_use]
    pub fn codex_session(&self) -> Option<&CodexSessionFile> {
        match self {
            Self::Codex { session } => Some(session),
            _ => None,
        }
    }
}

/// Case-insensitive `claude*` prefix check for the User-Agent gate.
///
/// `Claude-CLI`, `claude-cli`, `CLAUDE/2.1`, and any future Anthropic-side
/// casing all qualify; non-Claude callers (curl health probes, OpenAI SDKs) do
/// not. It is a **prefix** test, not a substring test — `some-claude-like` must
/// not match.
#[must_use]
pub fn ua_matches_claude(ua: &str) -> bool {
    // `eq_ignore_ascii_case` only matches whole strings, so compare a
    // lower-case copy. A UA is ~200 bytes in practice; the allocation is
    // negligible against the per-request work that follows.
    ua.to_ascii_lowercase().starts_with("claude")
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
    if facts.codex_route || facts.codex_marker.is_some() {
        return match attribute_codex(state, config, facts).await {
            Some(session) => Attributed::Codex { session },
            None => Attributed::Undecided,
        };
    }

    match attribute_claude(state, config, facts).await {
        Some(session) => {
            let parent_session_id = discover_parent_cached(state, &session).await;
            Attributed::Claude {
                session,
                parent_session_id,
            }
        }
        None => Attributed::UnknownHarness,
    }
}

/// Claude lane: User-Agent gate, then a bounded poll for the session file.
async fn attribute_claude(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
) -> Option<ClaudeSessionFile> {
    if !facts.user_agent.is_some_and(ua_matches_claude) {
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

/// Codex lane: bounded poll over marker/peer-PID matching, then — only when no
/// marker was supplied — a conservative recent-session fallback.
async fn attribute_codex(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
) -> Option<CodexSessionFile> {
    let deadline = Instant::now() + config.codex_timeout;
    loop {
        if let Some(session) = attribute_codex_once(state, config, facts) {
            return Some(session);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // A marker was supplied and never matched: the launch told us
            // exactly which provider to expect, so falling back to "some
            // recent session" would override explicit information with a
            // guess. Only the unmarked path may fall back.
            return facts
                .codex_marker
                .is_none()
                .then(|| attribute_recent_codex(state, config))
                .flatten();
        }
        tokio::time::sleep(config.codex_poll.min(remaining)).await;
    }
}

fn attribute_codex_once(
    state: &AttributionState,
    config: &AttributionConfig,
    facts: RequestFacts<'_>,
) -> Option<CodexSessionFile> {
    let cutoff = time::OffsetDateTime::now_utc() - config.codex_recent_window;
    let snapshot = state.codex_watcher.load_full();
    let recent: Vec<&CodexSessionFile> = snapshot
        .sessions
        .iter()
        .filter(|session| is_live_candidate(session, config, cutoff))
        .collect();

    if let Some(marker) = facts.codex_marker {
        let matches: Vec<&CodexSessionFile> = recent
            .iter()
            .copied()
            .filter(|session| session.has_model_provider(marker))
            .collect();
        if let Some(session) = marker_match(matches.into_iter().cloned().collect()) {
            return Some(session);
        }
    }

    let peer = facts.peer?;
    let pid = peer_pid::lookup_owner(peer).pid?;
    let matches: Vec<CodexSessionFile> = open_jsonl_sessions_by_pid(pid)
        .into_iter()
        .filter_map(|path| codex_session::read(&path))
        .filter(|session| is_live_candidate(session, config, cutoff))
        .collect();
    unique_or_newest("peer-open-file", matches)
}

/// Last resort: exactly one recent session of ours, or nothing.
///
/// Strictly single-candidate — with two concurrent Codex sessions there is no
/// evidence to choose between them, and attaching traffic to the wrong session
/// is worse than leaving it unattributed.
fn attribute_recent_codex(
    state: &AttributionState,
    config: &AttributionConfig,
) -> Option<CodexSessionFile> {
    let cutoff = time::OffsetDateTime::now_utc() - config.codex_recent_window;
    let snapshot = state.codex_watcher.load_full();
    let candidates: Vec<&CodexSessionFile> = snapshot
        .sessions
        .iter()
        .filter(|session| is_live_candidate(session, config, cutoff))
        .collect();
    match candidates.as_slice() {
        [] => None,
        [session] => Some((*session).clone()),
        _ => {
            warn!(
                count = candidates.len(),
                sample = ?sample_of(&candidates),
                "codex-session: multiple recent sessions; omitting session id",
            );
            None
        }
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

/// Resolve a candidate set that matched on positive evidence.
///
/// Unlike [`attribute_recent_codex`], ties here are broken rather than
/// refused: every candidate matched the marker or was held open by the calling
/// PID, so the most recently modified one is the live session and the others
/// are stale files that happen to share the evidence.
/// Resolve marker matches. The marker is fresh per launch BY CONTRACT, so
/// several live files may only legitimately share one when they belong to the
/// same session (rollout rotation) — newest wins there. Two DISTINCT live
/// sessions sharing a marker means a consumer reused a provider id across
/// concurrent processes; picking the newest would silently attach one
/// process's traffic to the other's session, so the marker lane refuses and
/// attribution falls through to peer-PID evidence (or stays undecided).
fn marker_match(candidates: Vec<CodexSessionFile>) -> Option<CodexSessionFile> {
    let mut ids: Vec<&str> = candidates
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() > 1 {
        let refs: Vec<&CodexSessionFile> = candidates.iter().collect();
        warn!(
            count = candidates.len(),
            sample = ?sample_of(&refs),
            "codex-session: marker shared by multiple LIVE sessions — a consumer \
             reused a provider id across concurrent processes; refusing to guess",
        );
        return None;
    }
    unique_or_newest("marker", candidates)
}

fn unique_or_newest(reason: &str, candidates: Vec<CodexSessionFile>) -> Option<CodexSessionFile> {
    match candidates.as_slice() {
        [] => None,
        [session] => Some(session.clone()),
        _ => {
            let refs: Vec<&CodexSessionFile> = candidates.iter().collect();
            warn!(
                reason,
                count = candidates.len(),
                sample = ?sample_of(&refs),
                "codex-session: multiple exact matches; using most recently modified session",
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

/// Build the Codex envelope, including the metadata blob ingest stores as
/// `sessions.harness_metadata`.
///
/// `transcriptPath` is always present: it is the operator's join key from a
/// captured session back to the rollout file on disk.
fn codex_envelope(session: &CodexSessionFile) -> TapesAttribution {
    let mut metadata = serde_json::Map::new();
    let mut put = |key: &str, value: &Option<String>| {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), serde_json::Value::String(value.clone()));
        }
    };
    put("originator", &session.originator);
    put("source", &session.source);
    put("threadSource", &session.thread_source);
    put("modelProvider", &session.model_provider);
    metadata.insert(
        "transcriptPath".to_owned(),
        serde_json::Value::String(session.path.display().to_string()),
    );

    TapesAttribution::codex_session(
        &session.session_id,
        session.cwd.as_deref(),
        session.cli_version.as_deref(),
        metadata,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::attribution::{CodexWatcherSnapshot, WatcherSnapshot};
    use crate::envelope::{HARNESS_ID_CLAUDE, HARNESS_ID_CODEX, HARNESS_ID_UNKNOWN};
    use arc_swap::ArcSwap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn filter() -> CodexProviderFilter {
        CodexProviderFilter::new("paper-openai")
    }

    fn config() -> AttributionConfig {
        AttributionConfig::new(filter())
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

    #[test]
    fn ua_gate_matches_any_casing_but_only_as_a_prefix() {
        assert!(ua_matches_claude("claude-cli/2.1.145"));
        // The exact spelling Anthropic shipped on at least one beta build.
        assert!(ua_matches_claude("Claude-CLI/2.1.145"));
        assert!(ua_matches_claude("CLAUDE/0.0"));

        assert!(!ua_matches_claude("curl/8.0"));
        assert!(!ua_matches_claude("OpenAI/python"));
        assert!(!ua_matches_claude(""));
        // Substring matching would be wrong — we want a prefix.
        assert!(!ua_matches_claude("some-claude-like"));
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
    fn codex_lane_miss_emits_no_envelope_at_all() {
        // Not "an envelope with no session id" — no envelope. Stamping
        // `harness_id: codex` here would assert an identity the pipeline just
        // declined to assert.
        assert!(Attributed::Undecided.envelope().is_none());
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
            attribute(&state, &config(), facts).await,
            Attributed::Undecided,
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
            attribute(&state, &config(), facts).await,
            Attributed::Undecided,
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
            attribute(&state, &config(), facts).await,
            Attributed::Undecided,
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
            attribute(&state, &config(), facts).await,
            Attributed::Undecided,
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
    fn marker_ties_break_to_the_most_recently_modified() {
        // Positive evidence on both, unlike the recent-session fallback: the
        // stale duplicate is a leftover file with the same provider.
        let older = codex_file("older", "paper-openai", time::Duration::minutes(5));
        let newer = codex_file("newer", "paper-openai", time::Duration::seconds(5));
        let got = unique_or_newest("marker", vec![older, newer]);
        assert_eq!(got.map(|s| s.session_id), Some("newer".to_owned()));
    }

    // A marker is fresh per launch by contract; two DISTINCT live sessions
    // sharing one means a consumer reused a provider id across concurrent
    // processes. Guessing (newest) would attach one process's traffic to the
    // other's session — the marker lane must refuse instead.
    #[test]
    fn marker_shared_by_distinct_live_sessions_refuses() {
        let a = codex_file("sid-a", "tapesctl-openai-x", time::Duration::minutes(1));
        let b = codex_file("sid-b", "tapesctl-openai-x", time::Duration::minutes(2));
        assert!(marker_match(vec![a, b]).is_none());
    }

    // Same session, several rollout files (rotation): newest wins as before.
    #[test]
    fn marker_same_session_rotation_picks_newest() {
        let older = codex_file("sid-a", "tapesctl-openai-x", time::Duration::minutes(9));
        let newer = codex_file("sid-a", "tapesctl-openai-x", time::Duration::minutes(1));
        let got = marker_match(vec![older, newer]).expect("same-session rotation resolves");
        assert_eq!(got.session_id, "sid-a");
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

        let envelope = codex_envelope(&session);
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
        let envelope = codex_envelope(&session);
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
