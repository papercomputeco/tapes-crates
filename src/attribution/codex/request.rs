//! What a Codex request says about its own identity, and what that implies
//! for the envelope.
//!
//! Every other file in this module reads *the machine*: a rollout file on
//! disk, the files a PID holds open, a watcher snapshot. This one reads **the
//! request**. Codex stamps its session, its thread, that thread's immediate
//! parent, the turn, and the sub-thread's kind on every inference call, and
//! that is the only evidence available before anything has been written to
//! disk — which makes it the evidence that survives the cold-start race the
//! rest of the lane spends a bounded wait on.
//!
//! # Two sources, deliberately compared
//!
//! Codex sends the identity twice: as individual headers, and again inside the
//! [`crate::envelope::CODEX_TURN_METADATA_HEADER`] JSON blob. Parsing both and
//! *comparing* them is the point. Agreement is corroboration. Disagreement —
//! [`CodexRequestIdentity::conflicting_metadata`] — means the request is not a
//! trustworthy account of itself, and every downstream rule that would act on
//! request identity (the child-shape join, hook-exact selection, narrowing a
//! candidate set to the named rollout) stands down and lets the
//! machine-observed lanes decide instead. A request cannot be allowed to talk
//! a capture client into filing a turn under a session it does not belong to.
//!
//! Only allowlisted fields survive the blob. It also carries the user's prompt
//! and assistant output; none of that may reach a consumer's logs or evidence
//! journal, exactly as for the desktop app's lifecycle payloads
//! ([`crate::attribution::codex_app`]).
//!
//! # The child re-key
//!
//! A Codex sub-thread ("subagent") is not a session of its own. Its turns must
//! land on the SAME captured session row as the root, the way Claude subagents
//! do by sharing the root's session file. [`codex_envelope`] therefore re-keys
//! every child-shaped request onto the root named by the request itself,
//! regardless of which rollout the selection lanes happened to resolve — see
//! [`child_envelope`] for the full contract, including why no parent header and
//! no per-child metadata are emitted.

use http::HeaderMap;
use serde::Deserialize;

use super::CodexSessionFile;
use crate::envelope::{
    CODEX_PARENT_THREAD_ID_HEADER, CODEX_SESSION_ID_HEADER, CODEX_THREAD_ID_HEADER,
    CODEX_TURN_METADATA_HEADER, OPENAI_SUBAGENT_HEADER, TapesAttribution, X_TAPES_METADATA_RAW_CAP,
};

/// The metadata key carrying the consumer's per-request correlation id.
///
/// The spelling is frozen: tapes' raw-turn attribution-repair query joins a
/// stored `harness_metadata` blob back to a proxy observation on exactly this
/// key. It reads `paper` because paperd minted it, and renaming it here would
/// silently orphan every turn a consumer could otherwise still repair.
pub const REQUEST_CORRELATION_METADATA_KEY: &str = "paperProxyRequestId";

/// The value a rollout transcript records in `thread_source` for a sub-thread.
///
/// The child-shape join requires it: a transcript whose lineage fields happen
/// to match a request but which does not declare itself a subagent is a
/// resumed/forked session, not a spawned thread, and attaching a child turn to
/// it would merge two distinct sessions.
const THREAD_SOURCE_SUBAGENT: &str = "subagent";

/// One Codex request's account of its own identity.
///
/// Constructed by the consumer with [`Self::from_headers`] and handed to the
/// pipeline through [`crate::attribution::RequestFacts::codex_identity`]. All
/// identifiers are opaque strings compared only for equality — nothing here is
/// parsed, interpreted, or used for anything but matching against evidence
/// recovered from the other lanes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexRequestIdentity {
    /// The consumer's per-request correlation id, emitted as
    /// [`REQUEST_CORRELATION_METADATA_KEY`].
    ///
    /// Empty means "the consumer minted none", and the key is then omitted
    /// rather than emitted blank. A consumer that wants an unattributed turn
    /// to remain repairable later must supply one — see
    /// [`Self::with_correlation_id`]. It is not minted here because a
    /// correlation id is only useful if the *same* value also reaches the
    /// consumer's own diagnostics, which the crate cannot write to.
    pub correlation_id: String,
    /// The ROOT session, pinned on every call including a sub-thread's.
    pub session_id: Option<String>,
    /// This call's own thread. Equal to [`Self::session_id`] on a root turn.
    pub thread_id: Option<String>,
    /// The immediate parent thread — one hop, so it equals the root only at
    /// depth 1.
    pub parent_thread_id: Option<String>,
    /// The turn this call belongs to, available only from the metadata blob.
    pub turn_id: Option<String>,
    /// The sub-thread's kind, canonicalised by [`canonical_subagent_kind`].
    pub subagent_kind: Option<String>,
    /// The header and metadata accounts of the identity disagree, so nothing
    /// here may be acted on. See the module docs.
    pub conflicting_metadata: bool,
}

/// The allowlisted subset of the turn-metadata blob. Everything else Codex
/// sends in it — prompt, output, extensions — is discarded by construction.
#[derive(Debug, Deserialize)]
struct CodexTurnMetadata {
    session_id: Option<String>,
    thread_id: Option<String>,
    parent_thread_id: Option<String>,
    turn_id: Option<String>,
    subagent_kind: Option<String>,
}

impl CodexRequestIdentity {
    /// Read the identity a Codex request states about itself.
    ///
    /// The header value wins where both sources carry a field, but a
    /// *disagreement* between them sets [`Self::conflicting_metadata`], which
    /// disables every rule that acts on this identity.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let metadata = header_string(headers, CODEX_TURN_METADATA_HEADER)
            .and_then(|raw| serde_json::from_str::<CodexTurnMetadata>(&raw).ok());
        let header_session_id = header_string(headers, CODEX_SESSION_ID_HEADER);
        let header_thread_id = header_string(headers, CODEX_THREAD_ID_HEADER);
        let header_parent_thread_id = header_string(headers, CODEX_PARENT_THREAD_ID_HEADER);
        let header_subagent_kind = header_string(headers, OPENAI_SUBAGENT_HEADER)
            .map(|kind| canonical_subagent_kind(&kind).to_owned());
        let metadata_subagent_kind = metadata
            .as_ref()
            .and_then(|value| value.subagent_kind.as_deref())
            .map(|kind| canonical_subagent_kind(kind).to_owned());
        let conflicting_metadata = [
            (
                header_session_id.as_ref(),
                metadata
                    .as_ref()
                    .and_then(|value| value.session_id.as_ref()),
            ),
            (
                header_thread_id.as_ref(),
                metadata.as_ref().and_then(|value| value.thread_id.as_ref()),
            ),
            (
                header_parent_thread_id.as_ref(),
                metadata
                    .as_ref()
                    .and_then(|value| value.parent_thread_id.as_ref()),
            ),
            (
                header_subagent_kind.as_ref(),
                metadata_subagent_kind.as_ref(),
            ),
        ]
        .into_iter()
        .any(|(header, metadata)| matches!((header, metadata), (Some(a), Some(b)) if a != b));
        Self {
            correlation_id: String::new(),
            session_id: header_session_id
                .or_else(|| metadata.as_ref().and_then(|value| value.session_id.clone())),
            thread_id: header_thread_id
                .or_else(|| metadata.as_ref().and_then(|value| value.thread_id.clone())),
            parent_thread_id: header_parent_thread_id.or_else(|| {
                metadata
                    .as_ref()
                    .and_then(|value| value.parent_thread_id.clone())
            }),
            turn_id: metadata.as_ref().and_then(|value| value.turn_id.clone()),
            subagent_kind: metadata_subagent_kind.or(header_subagent_kind),
            conflicting_metadata,
        }
    }

    /// Attach the consumer's per-request correlation id.
    #[must_use]
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = correlation_id.into();
        self
    }

    /// Does this request describe a turn made from a spawned sub-thread?
    ///
    /// Requires all three ids, a thread that differs from the root, and a
    /// thread that differs from its own parent. At depth 1 the direct parent
    /// equals the root; nested sub-threads keep the same root while naming
    /// their immediate parent, so parent and root are intentionally allowed to
    /// differ. A conflicted identity is never child-shaped.
    #[must_use]
    pub fn is_child_shaped(&self) -> bool {
        !self.conflicting_metadata
            && matches!(
            (
                self.session_id.as_deref(),
                self.thread_id.as_deref(),
                self.parent_thread_id.as_deref(),
            ),
            (Some(root), Some(thread), Some(parent)) if thread != root && thread != parent
            )
    }

    /// The rollout this request names, for narrowing a candidate set — or
    /// `None` when the request's account of itself is not trustworthy.
    ///
    /// Same value [`super::session::rollout_id`] resolves from the raw
    /// headers, minus the requests that contradict themselves. The gate is why
    /// this exists rather than each lane calling `rollout_id` again: narrowing
    /// on a contradicted thread id would let a forged blob steer a turn onto
    /// another session, which is the whole reason the conflict flag is
    /// computed.
    #[must_use]
    pub fn rollout_id(&self) -> Option<&str> {
        if self.conflicting_metadata {
            return None;
        }
        self.thread_id
            .as_deref()
            .or(self.session_id.as_deref())
            .filter(|value| !value.is_empty())
    }
}

/// Collapse Codex's two spellings for one sub-thread kind.
///
/// The legacy [`OPENAI_SUBAGENT_HEADER`] names the collaboration transport
/// while the structured metadata and the rollout transcript name the thread
/// source. Without this, a request that (correctly) says both would read as
/// self-contradictory and lose its identity evidence entirely.
#[must_use]
pub fn canonical_subagent_kind(kind: &str) -> &str {
    match kind {
        "collab_spawn" => "thread_spawn",
        kind => kind,
    }
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Does this rollout transcript describe exactly the sub-thread the request
/// claims to be?
///
/// Every lineage field must agree — the transcript's own id is the request's
/// thread, its root is the request's session, its direct parent is the
/// request's parent — and the transcript must declare itself a subagent.
/// Kinds are compared only when both sides name one, since older Codex builds
/// omit the kind on one side or the other and a missing kind is not a
/// contradiction.
#[must_use]
pub fn transcript_matches_child(
    session: &CodexSessionFile,
    identity: &CodexRequestIdentity,
) -> bool {
    let (Some(root), Some(thread), Some(parent)) = (
        identity.session_id.as_deref(),
        identity.thread_id.as_deref(),
        identity.parent_thread_id.as_deref(),
    ) else {
        return false;
    };
    session.session_id == thread
        && session.root_session_id.as_deref() == Some(root)
        && session.parent_thread_id.as_deref() == Some(parent)
        && session.thread_source.as_deref() == Some(THREAD_SOURCE_SUBAGENT)
        && match (
            identity.subagent_kind.as_deref(),
            session.subagent_kind.as_deref(),
        ) {
            (Some(request), Some(transcript)) => request == transcript,
            _ => true,
        }
}

/// The envelope for a Codex request whose rollout was resolved.
///
/// A child-shaped request re-keys to the root regardless of which rollout the
/// selection lanes returned — see [`child_envelope`]. Otherwise the resolved
/// rollout supplies identity and enrichment, and the request supplies the
/// correlation id and its own allowlisted identifiers.
#[must_use]
pub fn codex_envelope(
    session: &CodexSessionFile,
    identity: &CodexRequestIdentity,
) -> TapesAttribution {
    if let Some(envelope) = child_envelope(identity, Some(session)) {
        return envelope;
    }
    let mut metadata = request_metadata(identity);
    for (key, value) in [
        ("originator", session.originator.as_deref()),
        ("source", session.source.as_deref()),
        ("threadSource", session.thread_source.as_deref()),
        ("modelProvider", session.model_provider.as_deref()),
    ] {
        if let Some(value) = value {
            try_insert_metadata(&mut metadata, key, value);
        }
    }
    // The operator's join key from a captured session back to the rollout file.
    try_insert_metadata(
        &mut metadata,
        "transcriptPath",
        &session.path.display().to_string(),
    );
    // The parent header means resume/fork lineage. A thread-spawn transcript's
    // `parent_thread_id` names a THREAD, not a session — emitting it would
    // placeholder-insert a bogus session keyed by that thread id, so suppress
    // it even when the request identity was too incomplete to re-key to the
    // root above.
    let parent_sid = if session.thread_source.as_deref() == Some(THREAD_SOURCE_SUBAGENT) {
        None
    } else {
        session.parent_thread_id.as_deref()
    };
    TapesAttribution::codex_session_with_parent(
        &session.session_id,
        parent_sid,
        session.cwd.as_deref(),
        session.cli_version.as_deref(),
        metadata,
    )
}

/// The envelope for a Codex request whose rollout could not be resolved.
///
/// Not an empty envelope: a child-shaped request carries its own root session
/// id in its headers, so identity survives a cold watcher entirely. Where even
/// that is absent, the request-derived metadata still goes out — it carries
/// the correlation id an attribution-repair pass needs to find this turn
/// again.
#[must_use]
pub fn request_envelope(identity: &CodexRequestIdentity) -> TapesAttribution {
    child_envelope(identity, None)
        .unwrap_or_else(|| TapesAttribution::codex_with_metadata(request_metadata(identity)))
}

/// Attribution for a sub-thread's turn, keyed to the ROOT session.
///
/// Returns `None` for a request that is not child-shaped.
///
/// Codex sub-threads must land on the same captured session row as their root,
/// exactly like Claude subagents, which share the root's session file. The
/// identity comes from the REQUEST: the session header names the root and the
/// thread header names the child on every child-shaped call, so the emitted
/// session id is correct even when the child's rollout has not reached the
/// watcher yet — the race that would otherwise attribute child turns to
/// whatever rollout the fallback chain selected.
///
/// Two omissions are load-bearing:
///
/// * **No parent session header.** The child shares the root's session rather
///   than being a session of its own, and a parent header naming a THREAD id
///   would make a consumer placeholder-insert a bogus session keyed by that
///   thread id. The parent header keeps its resume/fork-lineage meaning only.
/// * **No per-child metadata** (thread id, parent thread id, turn id, subagent
///   kind, originator, source, thread source, transcript path). All child
///   turns merge into the ROOT session row's harness metadata, which is
///   last-write-wins per key — per-child values would churn root-owned fields,
///   and the last child would win forever. This mirrors Claude, whose subagent
///   requests carry only the single session file's metadata. The per-turn
///   child identity travels instead on the native `thread-id` header, which is
///   forwarded untouched and lands in the raw turn's own thread id.
///
/// `session` is enrichment only — cwd, CLI version, provider, all root-stable
/// values identical between root and child rollouts. Identity never depends on
/// which rollout the watcher selected.
#[must_use]
pub fn child_envelope(
    identity: &CodexRequestIdentity,
    session: Option<&CodexSessionFile>,
) -> Option<TapesAttribution> {
    if !identity.is_child_shaped() {
        return None;
    }
    // `is_child_shaped` guarantees the root is present; the `?` keeps this
    // fallible rather than panicking on a future refactor.
    let root = identity.session_id.as_deref()?;
    let mut metadata = serde_json::Map::new();
    insert_correlation_id(&mut metadata, identity);
    // Root-stable keys only — the same values every root turn writes.
    try_insert_metadata(&mut metadata, "codexSessionId", root);
    if let Some(provider) = session.and_then(|session| session.model_provider.as_deref()) {
        try_insert_metadata(&mut metadata, "modelProvider", provider);
    }
    Some(TapesAttribution::codex_session_with_parent(
        root,
        None,
        session.and_then(|session| session.cwd.as_deref()),
        session.and_then(|session| session.cli_version.as_deref()),
        metadata,
    ))
}

/// The session id the envelope for this request will carry, before the
/// envelope itself is built.
///
/// The root for a child-shaped request, the selected rollout otherwise —
/// exactly the rule [`codex_envelope`] and [`request_envelope`] apply, factored
/// out because a consumer's harness-selection evidence is keyed on the id that
/// will actually be emitted, not on the rollout that happened to be resolved.
#[must_use]
pub fn envelope_session_id<'a>(
    identity: &'a CodexRequestIdentity,
    session: Option<&'a CodexSessionFile>,
) -> Option<&'a str> {
    if identity.is_child_shaped() {
        return identity.session_id.as_deref();
    }
    session.map(|session| session.session_id.as_str())
}

/// The request-derived half of a Codex envelope's metadata.
#[must_use]
pub fn request_metadata(
    identity: &CodexRequestIdentity,
) -> serde_json::Map<String, serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    insert_correlation_id(&mut metadata, identity);
    for (key, value) in [
        ("codexSessionId", identity.session_id.as_ref()),
        ("codexThreadId", identity.thread_id.as_ref()),
        ("codexParentThreadId", identity.parent_thread_id.as_ref()),
        ("codexTurnId", identity.turn_id.as_ref()),
        ("codexSubagentKind", identity.subagent_kind.as_ref()),
    ] {
        let Some(value) = value else {
            continue;
        };
        try_insert_metadata(&mut metadata, key, value);
    }
    metadata
}

/// The correlation id goes in first and unconditionally.
///
/// It is fixed-size and it is what makes an unattributed turn repairable, so
/// it must never be the field a later oversized value evicts — every other
/// insertion goes through [`try_insert_metadata`], which drops the value it is
/// adding rather than anything already present.
fn insert_correlation_id(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    identity: &CodexRequestIdentity,
) {
    if identity.correlation_id.is_empty() {
        return;
    }
    metadata.insert(
        REQUEST_CORRELATION_METADATA_KEY.to_owned(),
        serde_json::Value::String(identity.correlation_id.clone()),
    );
}

/// Insert a metadata key only if the blob still fits the envelope's raw cap.
///
/// Exact-or-absent, per key: an oversized value is dropped on its own rather
/// than truncated (a truncated opaque id is worse than a missing one — it
/// looks like a real id and matches nothing) and rather than failing the whole
/// blob (which would take the correlation id down with it).
pub fn try_insert_metadata(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: &str,
) {
    if value.len() >= X_TAPES_METADATA_RAW_CAP {
        return;
    }
    metadata.insert(key.to_owned(), serde_json::Value::String(value.to_owned()));
    let fits = serde_json::to_vec(&metadata).is_ok_and(|raw| raw.len() <= X_TAPES_METADATA_RAW_CAP);
    if !fits {
        metadata.remove(key);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    fn root_session(id: &str) -> CodexSessionFile {
        CodexSessionFile {
            session_id: id.to_owned(),
            root_session_id: Some(id.to_owned()),
            parent_thread_id: None,
            subagent_kind: None,
            timestamp: time::OffsetDateTime::now_utc(),
            modified_at: Some(time::OffsetDateTime::now_utc()),
            cwd: Some("/tmp/work".to_owned()),
            originator: Some("codex-tui".to_owned()),
            cli_version: Some("0.139.0".to_owned()),
            source: Some("cli".to_owned()),
            thread_source: Some("user".to_owned()),
            model_provider: Some("paper-openai".to_owned()),
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
        }
    }

    fn child_session(id: &str, root: &str, parent: &str, kind: &str) -> CodexSessionFile {
        let mut session = root_session(id);
        session.root_session_id = Some(root.to_owned());
        session.parent_thread_id = Some(parent.to_owned());
        session.thread_source = Some("subagent".to_owned());
        session.subagent_kind = Some(kind.to_owned());
        session
    }

    fn child_identity(root: &str, parent: &str, child: &str, kind: &str) -> CodexRequestIdentity {
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

    // --- reading the request --------------------------------------------

    #[test]
    fn parses_simple_headers_and_allowlisted_turn_metadata() {
        let identity = CodexRequestIdentity::from_headers(&headers(&[
            ("session-id", "parent"),
            ("thread-id", "child"),
            ("x-codex-parent-thread-id", "parent"),
            ("x-openai-subagent", "guardian"),
            (
                "x-codex-turn-metadata",
                r#"{"turn_id":"turn-child","session_id":"parent","thread_id":"child","parent_thread_id":"parent","subagent_kind":"guardian","prompt":"must not survive"}"#,
            ),
        ]));

        assert_eq!(identity.session_id.as_deref(), Some("parent"));
        assert_eq!(identity.thread_id.as_deref(), Some("child"));
        assert_eq!(identity.parent_thread_id.as_deref(), Some("parent"));
        assert_eq!(identity.turn_id.as_deref(), Some("turn-child"));
        assert_eq!(identity.subagent_kind.as_deref(), Some("guardian"));
        assert!(identity.is_child_shaped());
        assert!(!identity.conflicting_metadata);
    }

    #[test]
    fn nested_child_allows_direct_parent_to_differ_from_root() {
        let identity = CodexRequestIdentity::from_headers(&headers(&[
            ("session-id", "root"),
            ("thread-id", "child"),
            ("x-codex-parent-thread-id", "parent"),
            (
                "x-codex-turn-metadata",
                r#"{"session_id":"root","thread_id":"child","parent_thread_id":"parent"}"#,
            ),
        ]));

        assert_eq!(identity.session_id.as_deref(), Some("root"));
        assert_eq!(identity.thread_id.as_deref(), Some("child"));
        assert_eq!(identity.parent_thread_id.as_deref(), Some("parent"));
        assert!(identity.is_child_shaped());
    }

    #[test]
    fn canonicalizes_collab_spawn_to_the_structured_thread_spawn() {
        let identity = CodexRequestIdentity::from_headers(&headers(&[
            ("session-id", "root"),
            ("thread-id", "child"),
            ("x-codex-parent-thread-id", "parent"),
            ("x-openai-subagent", "collab_spawn"),
            (
                "x-codex-turn-metadata",
                r#"{"session_id":"root","thread_id":"child","parent_thread_id":"parent","subagent_kind":"thread_spawn"}"#,
            ),
        ]));

        assert_eq!(identity.subagent_kind.as_deref(), Some("thread_spawn"));
        assert!(!identity.conflicting_metadata);
        assert!(identity.is_child_shaped());
    }

    #[test]
    fn genuinely_different_subagent_kinds_still_conflict() {
        let identity = CodexRequestIdentity::from_headers(&headers(&[
            ("session-id", "root"),
            ("thread-id", "child"),
            ("x-codex-parent-thread-id", "parent"),
            ("x-openai-subagent", "guardian"),
            (
                "x-codex-turn-metadata",
                r#"{"session_id":"root","thread_id":"child","parent_thread_id":"parent","subagent_kind":"thread_spawn"}"#,
            ),
        ]));

        assert_eq!(identity.subagent_kind.as_deref(), Some("thread_spawn"));
        assert!(identity.conflicting_metadata);
        assert!(!identity.is_child_shaped());
    }

    #[test]
    fn contradictory_sources_disable_the_child_shape() {
        let identity = CodexRequestIdentity::from_headers(&headers(&[
            ("session-id", "parent"),
            ("thread-id", "child"),
            ("x-codex-parent-thread-id", "parent"),
            (
                "x-codex-turn-metadata",
                r#"{"session_id":"other-parent","thread_id":"child","parent_thread_id":"parent"}"#,
            ),
        ]));

        assert!(identity.conflicting_metadata);
        assert!(!identity.is_child_shaped());
    }

    #[test]
    fn a_contradicted_request_names_no_rollout_to_narrow_by() {
        // The forgery guard: narrowing on a thread id the request itself
        // contradicts would let the blob steer the turn onto another session.
        let identity = CodexRequestIdentity::from_headers(&headers(&[
            ("thread-id", "claimed"),
            ("x-codex-turn-metadata", r#"{"thread_id":"contradictory"}"#),
        ]));
        assert!(identity.conflicting_metadata);
        assert_eq!(identity.rollout_id(), None);
    }

    #[test]
    fn rollout_id_prefers_the_thread_over_the_root() {
        let identity = CodexRequestIdentity::from_headers(&headers(&[
            ("session-id", "root"),
            ("thread-id", "child"),
        ]));
        assert_eq!(identity.rollout_id(), Some("child"));

        let root_turn = CodexRequestIdentity::from_headers(&headers(&[("session-id", "root")]));
        assert_eq!(root_turn.rollout_id(), Some("root"));
    }

    // --- joining a request to a transcript -------------------------------

    #[test]
    fn a_matching_subagent_transcript_joins_the_request() {
        let identity = child_identity("root", "root", "child", "guardian");
        assert!(transcript_matches_child(
            &child_session("child", "root", "root", "guardian"),
            &identity,
        ));
    }

    #[test]
    fn a_transcript_that_is_not_a_subagent_never_joins() {
        // A resumed session can carry lineage fields that look right; the
        // `thread_source` declaration is what separates the two shapes.
        let identity = child_identity("root", "root", "child", "guardian");
        let mut session = child_session("child", "root", "root", "guardian");
        session.thread_source = Some("user".to_owned());
        assert!(!transcript_matches_child(&session, &identity));
    }

    #[test]
    fn a_missing_kind_on_either_side_is_not_a_contradiction() {
        let mut identity = child_identity("root", "root", "child", "guardian");
        identity.subagent_kind = None;
        assert!(transcript_matches_child(
            &child_session("child", "root", "root", "guardian"),
            &identity,
        ));
    }

    // --- the envelope ----------------------------------------------------

    #[test]
    fn a_child_request_rekeys_to_the_root_and_drops_per_child_metadata() {
        let session = child_session("child", "parent", "parent", "guardian");
        let identity = child_identity("parent", "parent", "child", "guardian");
        let envelope = codex_envelope(&session, &identity);

        assert_eq!(envelope.session_id.as_deref(), Some("parent"));
        assert_eq!(envelope.parent_sid, None);
        assert_eq!(
            envelope.metadata[REQUEST_CORRELATION_METADATA_KEY],
            "correlation-child",
        );
        assert_eq!(envelope.metadata["codexSessionId"], "parent");
        assert_eq!(envelope.metadata["modelProvider"], "paper-openai");
        for key in [
            "codexThreadId",
            "codexParentThreadId",
            "codexTurnId",
            "codexSubagentKind",
            "originator",
            "source",
            "threadSource",
            "transcriptPath",
        ] {
            assert!(
                !envelope.metadata.contains_key(key),
                "unexpected per-child {key}",
            );
        }
    }

    #[test]
    fn a_child_request_with_no_rollout_still_names_its_root() {
        let identity = child_identity("parent", "parent", "child", "guardian");
        let envelope = request_envelope(&identity);
        assert_eq!(envelope.session_id.as_deref(), Some("parent"));
        assert_eq!(envelope.cwd, None);
        assert_eq!(envelope.metadata["codexSessionId"], "parent");
        assert!(!envelope.metadata.contains_key("modelProvider"));
    }

    #[test]
    fn a_subagent_rollout_never_emits_a_parent_session_header() {
        // Reached when the request identity was too incomplete to re-key but
        // the selected rollout is a subagent transcript anyway.
        let session = child_session("child", "root", "parent", "guardian");
        let envelope = codex_envelope(&session, &CodexRequestIdentity::default());
        assert_eq!(envelope.session_id.as_deref(), Some("child"));
        assert_eq!(envelope.parent_sid, None);
    }

    #[test]
    fn a_resumed_rollouts_parent_is_still_lineage() {
        let mut session = root_session("resumed");
        session.parent_thread_id = Some("original".to_owned());
        let envelope = codex_envelope(&session, &CodexRequestIdentity::default());
        assert_eq!(envelope.parent_sid.as_deref(), Some("original"));
    }

    #[test]
    fn an_absent_correlation_id_is_omitted_rather_than_emitted_blank() {
        let metadata = request_metadata(&CodexRequestIdentity::default());
        assert!(!metadata.contains_key(REQUEST_CORRELATION_METADATA_KEY));
    }

    #[test]
    fn oversized_identifiers_drop_individually_and_spare_the_correlation_id() {
        let oversized = "x".repeat(X_TAPES_METADATA_RAW_CAP + 1);
        let identity = CodexRequestIdentity {
            correlation_id: "correlation-bounded".to_owned(),
            session_id: Some(oversized.clone()),
            thread_id: Some("safe-thread".to_owned()),
            parent_thread_id: Some(oversized.clone()),
            turn_id: Some(oversized.clone()),
            subagent_kind: Some(oversized),
            conflicting_metadata: false,
        };
        let metadata = request_metadata(&identity);

        assert_eq!(
            metadata[REQUEST_CORRELATION_METADATA_KEY],
            "correlation-bounded",
        );
        assert_eq!(metadata["codexThreadId"], "safe-thread");
        for key in [
            "codexSessionId",
            "codexParentThreadId",
            "codexTurnId",
            "codexSubagentKind",
        ] {
            assert!(
                !metadata.contains_key(key),
                "{key} should have been dropped"
            );
        }
    }

    #[test]
    fn oversized_rollout_enrichment_cannot_evict_the_correlation_id() {
        let oversized = "x".repeat(X_TAPES_METADATA_RAW_CAP + 1);
        let mut session = root_session("root");
        session.originator = Some("safe-originator".to_owned());
        session.source = Some(oversized.clone());
        session.path = PathBuf::from(oversized);
        let identity = CodexRequestIdentity {
            correlation_id: "correlation-root".to_owned(),
            session_id: Some("root".to_owned()),
            thread_id: Some("root".to_owned()),
            ..CodexRequestIdentity::default()
        };

        let envelope = codex_envelope(&session, &identity);
        assert_eq!(
            envelope.metadata[REQUEST_CORRELATION_METADATA_KEY],
            "correlation-root",
        );
        assert_eq!(envelope.metadata["codexThreadId"], "root");
        assert_eq!(envelope.metadata["originator"], "safe-originator");
        assert!(!envelope.metadata.contains_key("source"));
        assert!(!envelope.metadata.contains_key("transcriptPath"));
    }
}
