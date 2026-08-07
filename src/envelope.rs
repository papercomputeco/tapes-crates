//! The capture envelope.
//!
//! The `X-Tapes-*` request-header contract carries attribution and provenance
//! from a capture transport (the tapesctl JIT proxy, paperd, or tapes-extproc)
//! into the tapes ingest server. It is the narrow Rust↔Go waist: metadata, not
//! parsing, and it rarely changes.
//!
//! This module is the **producer** half. It turns a resolved session identity
//! into the on-wire header set: percent-encoding, the 256-byte session-name
//! cap, base64url metadata, and the 8 KiB total budget. The parsers on the
//! other side (tapes-extproc's `ParseSessionEnvelope`, the tapes ingest reader)
//! read that header set back into an envelope. Both halves table-test against
//! one shared fixture corpus, vendored here under
//! `vendor/tapes-envelope-fixtures/` — see that directory's `SOURCE.md` and the
//! oracle in `envelope_fixtures.rs`. Drift between the halves is otherwise
//! invisible until a captured session lands mis-attributed.
//!
//! Extracted from paperd's `proxy::headers`, which was the sole producer before
//! `tapesctl` existed. The behaviour is pinned by the corpus, so both capture
//! paths emit byte-identical envelopes by construction rather than by review.
//!
//! What is deliberately *not* here: `X-Paper-Auth` and its injection helper.
//! That header is paperd's private channel to the Paper cloud edge, not part of
//! the tapes envelope, and it stays in paperd. The generic RFC 7230 hop-by-hop
//! knowledge every capture proxy needs does live here — see
//! [`HOP_BY_HOP_HEADERS`] and [`is_hop_by_hop`].

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, HeaderName, HeaderValue};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use snafu::{ResultExt, Snafu};
use tapes_capture::HarnessSession;
use tracing::warn;

/// Failure modes the envelope helpers can surface.
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
#[non_exhaustive]
pub enum HeaderError {
    /// The supplied string value is not a valid HTTP header value —
    /// typically because it contains non-visible-ASCII bytes. Header
    /// values must be visible ASCII per RFC 7230 §3.2.6.
    #[snafu(display("header value is not valid HTTP-header bytes"))]
    InvalidValue {
        /// Underlying validation error from the `http` crate.
        source: http::header::InvalidHeaderValue,
    },
}

/// Hop-by-hop headers per RFC 7230 §6.1. These are scoped to a single
/// connection and must not be forwarded across a proxy boundary. Listed
/// in lower-case so case-insensitive comparison can use
/// `eq_ignore_ascii_case` directly without renormalising the input.
pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Common prefix for every capture-envelope request header.
pub const HEADER_PREFIX: &str = "x-tapes-";

// --- X-Tapes-* envelope headers -----------------------------------------
//
// A capture transport attaches these on every outbound LLM request. They
// are scoped to the transport's private channel to ingest and are expected
// to be stripped upstream before the request reaches the model provider.

/// Identifies the harness — `claude`, `unknown`, or a future
/// registered value. Required.
pub const X_TAPES_HARNESS_ID: &str = "x-tapes-harness-id";

/// Opaque harness-side session identifier. For Claude, the
/// `sessionId` UUID from `~/.claude/sessions/<pid>.json`. Required
/// when `harness_id != "unknown"`.
pub const X_TAPES_HARNESS_SESSION_ID: &str = "x-tapes-harness-session-id";

/// Harness version (e.g. claude `version` field). Optional.
pub const X_TAPES_HARNESS_VERSION: &str = "x-tapes-harness-version";

/// Harness working directory. Percent-encoded UTF-8 — common
/// filesystems (macOS, Linux) allow non-ASCII path components, which
/// RFC 7230 forbids in raw header values. Optional.
pub const X_TAPES_CWD: &str = "x-tapes-cwd";

/// User-given session name (`/name` in claude). Percent-encoded UTF-8,
/// capped at 256 bytes raw. Optional.
pub const X_TAPES_SESSION_NAME: &str = "x-tapes-session-name";

/// Fork-parent's `harness_session_id`, when the capture client has
/// recovered lineage from the transcript. Optional.
pub const X_TAPES_PARENT_HARNESS_SESSION_ID: &str = "x-tapes-parent-harness-session-id";

/// Base64url(no padding) of a JSON object holding the harness blob
/// destined for `sessions.harness_metadata`. Capped at 4 KiB raw JSON;
/// dropped first when the total `X-Tapes-*` byte budget (8 KiB) is
/// exceeded.
pub const X_TAPES_HARNESS_METADATA: &str = "x-tapes-harness-metadata";

/// Sentinel harness-id used when the capture client couldn't attribute
/// the request (cold race, unrecognised User-Agent, sandboxed harness).
/// No `X-Tapes-Harness-Session-Id` is attached in this case.
pub const HARNESS_ID_UNKNOWN: &str = "unknown";

/// Harness-id attached by pi's capture extension — the one harness that stamps
/// its own envelope from inside itself. The extension is
/// [`crate::plugin::PI_GATEWAY_EXTENSION`].
pub const HARNESS_ID_PI: &str = "pi";

/// Harness-id attached for Claude traffic (User-Agent `claude*`).
pub const HARNESS_ID_CLAUDE: &str = "claude";

/// Harness-id attached for Codex traffic.
pub const HARNESS_ID_CODEX: &str = "codex";

/// Harness-id for the Codex desktop app.
///
/// A distinct harness, not an alias of [`HARNESS_ID_CODEX`]: the app is a
/// long-lived Codex host a consumer configures rather than launches, and its
/// session identity arrives through lifecycle hook reports (see
/// [`crate::attribution::codex_app`]) rather than through the peer-PID lanes.
/// It shares Codex's wire protocol and rollout tree, so requests still carry
/// the `thread-id`/`session-id` pair and transcripts still land under
/// `$CODEX_HOME/sessions` — what differs is who answers "which session is
/// this?", and keying captured sessions by a distinct id keeps that
/// difference visible downstream.
pub const HARNESS_ID_CODEX_APP: &str = "codex-app";

/// Harness-id attached for opencode traffic.
///
/// opencode capture arrives with the standalone client; the constant lives
/// here with the others so [`crate::harness`] has one place to take every id
/// from, rather than the launch recipe spelling its own.
pub const HARNESS_ID_OPENCODE: &str = "opencode";

/// Maximum total budget across all `X-Tapes-*` headers.
/// Metadata is dropped first when the budget is exceeded; the other
/// headers are small (UUIDs and paths) and stay.
pub const X_TAPES_TOTAL_BUDGET: usize = 8 * 1024;

/// Maximum raw JSON size of the metadata blob before base64 encoding.
/// Larger blobs cause the entire metadata header to be dropped
/// silently — the producer enforces this locally rather than letting an
/// oversize blob travel upstream.
pub const X_TAPES_METADATA_RAW_CAP: usize = 4 * 1024;

/// Maximum raw byte length of `X-Tapes-Session-Name` before
/// percent-encoding. Names beyond this are truncated to the cap before
/// encoding (silently — the cap exists to keep the header inside the
/// total budget, not to validate user input).
pub const X_TAPES_SESSION_NAME_CAP: usize = 256;

/// Percent-encoding set for header values that may carry arbitrary
/// UTF-8 (session name, working directory). RFC 7230 header values are
/// visible ASCII; we escape everything outside that range and a small
/// set of structural ASCII characters that could confuse header
/// parsers.
const UTF8_VALUE_ESCAPE: &AsciiSet = &CONTROLS.add(b' ').add(b'%').add(b'"').add(b'\\').add(0x7f);

/// Claude Code's sub-thread header, in priority order — first present wins.
///
/// Claude Code stamps `x-claude-code-agent-id` on every call made from a
/// subagent context (including its security-monitor checks) and omits it on the
/// main thread, so presence alone is the signal.
pub const CLAUDE_THREAD_ID_HEADERS: &[&str] = &["x-claude-code-agent-id"];

/// Codex's thread id for one call: equal to [`CODEX_SESSION_ID_HEADER`] on a
/// root turn, a distinct id on a spawned sub-thread's turn.
///
/// Also read — as an ordered first-present list rather than as a pair — by
/// [`crate::attribution::codex::session::CODEX_ROLLOUT_ID_HEADERS`], which
/// answers a different question: *which rollout* a request belongs to. Both
/// take their spelling from here so the two readings cannot drift apart.
pub const CODEX_THREAD_ID_HEADER: &str = "thread-id";

/// Codex's root session id, present on every Codex call.
pub const CODEX_SESSION_ID_HEADER: &str = "session-id";

/// The thread that spawned this call's thread — **one hop**, not the root.
///
/// Present only on a sub-thread's calls. At depth 1 it equals
/// [`CODEX_SESSION_ID_HEADER`]; deeper, it names the immediate parent while the
/// session header stays pinned to the root. That pairing is what makes a
/// request self-describing enough to be joined against a rollout transcript's
/// own `parent_thread_id` — see
/// [`crate::attribution::codex::request::CodexRequestIdentity`].
pub const CODEX_PARENT_THREAD_ID_HEADER: &str = "x-codex-parent-thread-id";

/// A JSON restatement of the identity headers, plus the turn id.
///
/// Codex sends the same session/thread/parent/subagent-kind values here that it
/// sends as individual headers, so the blob is a *corroborating* source rather
/// than an authoritative one: where the two disagree the request's account of
/// itself is not trustworthy at all (see `conflicting_metadata`). It is also
/// the only carrier of the turn id.
///
/// The blob additionally carries the user's prompt and other conversation
/// content. Parsing is therefore an allowlist, exactly as it is for the
/// desktop app's lifecycle payloads: see
/// [`crate::attribution::codex_app`].
pub const CODEX_TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";

/// The legacy, unstructured spelling of a sub-thread's kind.
///
/// Codex names the collaboration *transport* here (`collab_spawn`) while its
/// structured metadata and the rollout transcript name the *thread source*
/// (`thread_spawn`). The two are canonicalised to one vocabulary before
/// comparison — see
/// [`crate::attribution::codex::request::canonical_subagent_kind`] — so a
/// request that says both things does not read as self-contradictory.
pub const OPENAI_SUBAGENT_HEADER: &str = "x-openai-subagent";

/// How one harness's request headers name the sub-thread a call was made from.
///
/// The two shapes exist because harnesses disagree about what a header's
/// *presence* means, and a single flat list of names cannot express both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HarnessThreadRule {
    /// The harness stamps a dedicated header **only** on sub-thread calls, so
    /// presence is the whole signal. Names are tried in order; the first one
    /// present with a non-empty value wins.
    FirstPresent(&'static [&'static str]),
    /// The harness stamps both headers on **every** call, and the sub-thread
    /// signal is their divergence rather than either one's presence.
    ///
    /// Both must be present and differ. Each guard earns its place:
    ///
    /// * An **equal** pair is a root turn. Codex sets `thread-id` ==
    ///   `session-id` there, so a flat first-present entry for `thread-id`
    ///   would stamp a thread id on every root turn — which is not cosmetic:
    ///   downstream, a non-empty thread id routes the root spine off the main
    ///   spine and silently degrades the session's derived status.
    /// * A **lone** thread id, with no session id beside it, is not a
    ///   recognised shape for this harness at all. Treating it as a sub-thread
    ///   would risk that same misrouting on evidence the rule cannot confirm,
    ///   so it resolves to nothing instead.
    DivergentPair {
        /// Header carrying this call's thread id.
        thread: &'static str,
        /// Header carrying the root session id the thread id is compared against.
        session: &'static str,
    },
}

impl HarnessThreadRule {
    /// Apply this rule to a request's headers.
    ///
    /// `None` means "this rule recognises nothing here" — a main-thread call,
    /// or a request belonging to a different harness.
    #[must_use]
    pub fn resolve<'h>(&self, headers: &'h HeaderMap) -> Option<&'h str> {
        match *self {
            Self::FirstPresent(names) => names.iter().find_map(|name| header_str(headers, name)),
            Self::DivergentPair { thread, session } => {
                let thread_id = header_str(headers, thread)?;
                let session_id = header_str(headers, session)?;
                (thread_id != session_id).then_some(thread_id)
            }
        }
    }
}

/// Every harness's sub-thread rule, in the order [`thread_id`] tries them.
///
/// This is harness knowledge, so it lives here rather than in each capture
/// client; the rest of a client's pipeline is harness-neutral and only ever
/// sees the resolved thread id. The table mirrors tapes-extproc's `ThreadID` —
/// the two must agree, since extproc reads these off the wire for exactly the
/// same purpose. Add other harnesses' rules to both as they are identified.
///
/// Order is precedence, and it is only observable when one request carries
/// evidence for two harnesses at once. Claude's dedicated header is the more
/// specific signal, so it is tried first, matching extproc.
pub const HARNESS_THREAD_ID_RULES: &[HarnessThreadRule] = &[
    HarnessThreadRule::FirstPresent(CLAUDE_THREAD_ID_HEADERS),
    HarnessThreadRule::DivergentPair {
        thread: CODEX_THREAD_ID_HEADER,
        session: CODEX_SESSION_ID_HEADER,
    },
];

/// A header's value as a string, treating absent, non-ASCII and empty alike as
/// "not stated". No trimming: the comparison in
/// [`HarnessThreadRule::DivergentPair`] is against another raw header value,
/// and extproc compares the bytes it received.
fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
}

/// Resolve the harness-native sub-thread id for a request.
///
/// Returns `None` for a main-thread call, or for a harness with no known
/// mapping. The value is destined for the ingest turn's `meta.thread_id`; it is
/// **not** an `X-Tapes-*` envelope header and is not stripped from the outbound
/// request — the harness set it, and upstream may legitimately see it.
#[must_use]
pub fn thread_id(headers: &HeaderMap) -> Option<&str> {
    HARNESS_THREAD_ID_RULES
        .iter()
        .find_map(|rule| rule.resolve(headers))
}

/// Returns true if `name` is in [`HOP_BY_HOP_HEADERS`] (case-insensitive).
#[must_use]
pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP_HEADERS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
}

/// Insert the full `X-Tapes-*` envelope for a resolved harness session.
///
/// Attaches `X-Tapes-Harness-Id` (the session's own
/// [`HarnessSession::harness_id`]), `X-Tapes-Harness-Session-Id`,
/// `X-Tapes-Harness-Version`, `X-Tapes-Cwd`, `X-Tapes-Session-Name`
/// (percent-encoded UTF-8), optional `X-Tapes-Parent-Harness-Session-Id` when
/// `parent_sid` is set, and `X-Tapes-Harness-Metadata` (base64url(JSON) of the
/// session's metadata object) when the JSON fits the 4 KiB raw cap.
///
/// Budget enforcement: the total `X-Tapes-*` byte budget is 8 KiB.
/// When the metadata header would push the running total over the cap,
/// the metadata header is dropped silently and the rest proceeds. The
/// session name is bounded to 256 bytes raw before percent-encoding so
/// it can't dominate the budget.
///
/// All non-ASCII content arrives either as percent-encoded UTF-8
/// (`X-Tapes-Session-Name`, `X-Tapes-Cwd`) or base64url
/// (`X-Tapes-Harness-Metadata`). Any other field that happens to contain
/// bytes invalid in an HTTP header value (CR/LF/NUL) is dropped — that
/// header is omitted from the envelope rather than returning an error.
/// The "mandatory headers" guarantee is satisfied by always emitting the
/// `X-Tapes-Harness-Id` header.
///
/// # Errors
///
/// Returns [`HeaderError::InvalidValue`] only if the required harness-id
/// value is not valid HTTP-header bytes. Unreachable in practice — the
/// fallback path substitutes the ASCII `unknown` constant — but the
/// signature keeps the failure visible to callers.
pub fn inject_session_envelope(
    headers: &mut HeaderMap,
    session: &impl HarnessSession,
    parent_sid: Option<&str>,
) -> Result<(), HeaderError> {
    inject_tapes_attribution(headers, TapesAttribution::from_session(session, parent_sid))
}

/// Insert the envelope for a request nobody could attribute.
///
/// Attaches only `X-Tapes-Harness-Id: unknown` — unless the inbound request
/// already carries a complete envelope, which is preserved as-is because a
/// harness that stamped its own identity from inside itself knows more than a
/// failed lookup does. Any stale *partial* envelope is cleared first, so a
/// half-stated identity never rides out alongside the sentinel.
///
/// The counterpart to [`inject_session_envelope`], and split from it rather
/// than expressed as its `None` case: the two arms share no input. This one
/// needs no session type at all, which is what lets a caller with nothing to
/// say reach the sentinel path without naming a harness's session shape.
///
/// # Errors
///
/// Returns [`HeaderError::InvalidValue`] if the ASCII `unknown` constant is
/// somehow not valid HTTP-header bytes. Unreachable; the signature keeps the
/// failure visible.
pub fn inject_unattributed_envelope(headers: &mut HeaderMap) -> Result<(), HeaderError> {
    if has_complete_inbound_envelope(headers) {
        return Ok(());
    }
    clear_tapes_headers(headers);
    inject_tapes_attribution(headers, TapesAttribution::unknown())
}

/// Session-attribution envelope to serialize into `X-Tapes-*` headers.
///
/// Fields are public so a consumer can construct an attribution the named
/// constructors below don't express — the fixture oracle does exactly that,
/// because the shared corpus spans harnesses (`pi`) and field combinations
/// production has no constructor for.
pub struct TapesAttribution {
    /// Harness identifier; [`HARNESS_ID_UNKNOWN`] selects the
    /// single-header path.
    pub harness_id: String,
    /// Opaque harness-side session id.
    pub session_id: Option<String>,
    /// Harness version string.
    pub version: Option<String>,
    /// Harness working directory; percent-encoded on the wire.
    pub cwd: Option<String>,
    /// User-given session name; capped and percent-encoded on the wire.
    pub name: Option<String>,
    /// Fork-parent's harness session id.
    pub parent_sid: Option<String>,
    /// Free-form harness metadata; base64url(JSON) on the wire.
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

impl TapesAttribution {
    /// The cold-race / unrecognised-harness fallback.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            harness_id: HARNESS_ID_UNKNOWN.to_owned(),
            session_id: None,
            version: None,
            cwd: None,
            name: None,
            parent_sid: None,
            metadata: serde_json::Map::new(),
        }
    }

    /// Codex traffic whose session identity has not been resolved.
    #[must_use]
    pub fn codex() -> Self {
        Self::codex_with_metadata(serde_json::Map::new())
    }

    /// Codex traffic with no resolved rollout, carrying request-derived
    /// metadata (a proxy that saw useful request headers but could not
    /// name a session still forwards what it learned).
    #[must_use]
    pub fn codex_with_metadata(metadata: serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            harness_id: HARNESS_ID_CODEX.to_owned(),
            session_id: None,
            version: None,
            cwd: None,
            name: None,
            parent_sid: None,
            metadata,
        }
    }

    /// Codex traffic attributed to a resolved rollout session.
    #[must_use]
    pub fn codex_session(
        session_id: &str,
        cwd: Option<&str>,
        cli_version: Option<&str>,
        metadata: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self::codex_session_with_parent(session_id, None, cwd, cli_version, metadata)
    }

    /// Codex traffic attributed to a session id with optional resume/fork
    /// parent lineage.
    ///
    /// `parent_sid` keeps the envelope's resume/fork meaning: it must name
    /// a harness SESSION, never a sub-thread. A thread-spawn transcript's
    /// `parent_thread_id` names a THREAD, and emitting it here would make a
    /// consumer placeholder-insert a bogus session keyed by that thread id
    /// — pass `None` for subagent rollouts.
    #[must_use]
    pub fn codex_session_with_parent(
        session_id: &str,
        parent_sid: Option<&str>,
        cwd: Option<&str>,
        cli_version: Option<&str>,
        metadata: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        Self {
            harness_id: HARNESS_ID_CODEX.to_owned(),
            session_id: Some(session_id.to_owned()),
            version: cli_version.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
            name: None,
            parent_sid: parent_sid.map(str::to_owned),
            metadata,
        }
    }

    /// Read an attribution back out of an envelope a harness stamped on
    /// itself, for harnesses whose session identity comes from a managed
    /// extension rather than from this crate's session watchers.
    ///
    /// `None` unless the headers carry a **complete** envelope: a harness id
    /// that is present and is not the [`HARNESS_ID_UNKNOWN`] sentinel, plus a
    /// non-blank session id. That completeness rule is the point of this
    /// constructor. The same rule decides two things in different processes —
    /// here, whether a capture client files a turn under an inbound envelope;
    /// and in [`inject_unattributed_envelope`], whether the producer *preserves* an
    /// inbound envelope instead of overwriting it with `unknown`. Two
    /// spellings of it drift into a request whose headers say `pi` and whose
    /// ingest row says `unknown`, so both callers come through here.
    ///
    /// Only the plain-text fields are read. `cwd`, session name, and metadata
    /// are percent-encoded or base64url on the wire, and this module is the
    /// envelope's *producer* half — the parsers live on the tapes side and
    /// table-test against the same corpus. Decoding here would stand up a
    /// second, drifting implementation of an encoder that is twenty lines
    /// above, so those fields come back empty rather than guessed. Nothing is
    /// lost today: the self-attributing harnesses stamp exactly the fields
    /// this reads. A harness that starts sending the encoded ones wants the
    /// decode half added here, once, not at each call site.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let harness_id =
            envelope_field(headers, X_TAPES_HARNESS_ID).filter(|id| id != HARNESS_ID_UNKNOWN)?;
        let session_id = envelope_field(headers, X_TAPES_HARNESS_SESSION_ID)?;
        Some(Self {
            harness_id,
            session_id: Some(session_id),
            version: envelope_field(headers, X_TAPES_HARNESS_VERSION),
            cwd: None,
            name: None,
            parent_sid: envelope_field(headers, X_TAPES_PARENT_HARNESS_SESSION_ID),
            metadata: serde_json::Map::new(),
        })
    }

    /// Traffic attributed to a resolved harness session, with optional
    /// recovered fork-parent lineage.
    ///
    /// Takes [`HarnessSession`] rather than any harness's own session type.
    /// That is the whole reason this module can live beside the wire format
    /// instead of beside the harness registry: the projection from "what the
    /// harness published" to "what the envelope carries" is stated once, as a
    /// requirement, and each harness satisfies it on its own side. A new
    /// harness reaches this constructor by implementing the trait — nothing
    /// here learns its name.
    #[must_use]
    pub fn from_session(session: &impl HarnessSession, parent_sid: Option<&str>) -> Self {
        Self {
            harness_id: session.harness_id().to_owned(),
            session_id: Some(session.session_id().to_owned()),
            version: session.version().map(str::to_owned),
            cwd: session.cwd().map(str::to_owned),
            name: session.name().map(str::to_owned),
            parent_sid: parent_sid.map(str::to_owned),
            metadata: session.metadata(),
        }
    }
}

/// Insert the `X-Tapes-*` envelope headers for an already-resolved
/// attribution.
///
/// # Errors
///
/// Returns [`HeaderError::InvalidValue`] if the required harness-id value
/// is not valid HTTP-header bytes. Unreachable in practice: the failure
/// path below wipes the partial envelope and substitutes the ASCII
/// `unknown` constant.
pub fn inject_tapes_attribution(
    headers: &mut HeaderMap,
    attribution: TapesAttribution,
) -> Result<(), HeaderError> {
    // Unknown-harness path: one header, no further work. Always
    // succeeds — the constant is ASCII.
    if attribution.harness_id == HARNESS_ID_UNKNOWN {
        let value = HeaderValue::from_static(HARNESS_ID_UNKNOWN);
        headers.insert(HeaderName::from_static(X_TAPES_HARNESS_ID), value);
        return Ok(());
    }

    let mut budget = X_TAPES_TOTAL_BUDGET;

    // 1. Harness-Id (REQUIRED). This is the one mandatory header on
    //    the known-harness arm. If insertion fails for any reason
    //    (unreachable today — the value comes from a fixed set of
    //    constants — but defensive against a future refactor that lets
    //    the harness-id be dynamic), wipe any other `X-Tapes-*` header
    //    we might have inserted and fall through to the unknown-harness
    //    path. Better to attribute as `unknown` than to ship an
    //    envelope that's missing the required header.
    if let Err(err) = insert_required_ascii(
        headers,
        X_TAPES_HARNESS_ID,
        &attribution.harness_id,
        &mut budget,
    ) {
        warn!(
            harness_id = %attribution.harness_id,
            error = ?err,
            "tapes-headers: required X-Tapes-Harness-Id insert failed; falling back to unknown",
        );
        clear_tapes_headers(headers);
        let value = HeaderValue::from_static(HARNESS_ID_UNKNOWN);
        headers.insert(HeaderName::from_static(X_TAPES_HARNESS_ID), value);
        return Ok(());
    }

    // 2. Non-metadata headers next. Each tries to fit inside the
    //    remaining budget and is silently dropped if the value is
    //    invalid (e.g. internal CR/LF) or oversize — optional fields
    //    drop rather than failing the request.
    if let Some(session_id) = attribution.session_id.as_deref() {
        try_insert_string(headers, X_TAPES_HARNESS_SESSION_ID, session_id, &mut budget);
    }
    if let Some(v) = attribution.version.as_deref() {
        try_insert_string(headers, X_TAPES_HARNESS_VERSION, v, &mut budget);
    }
    if let Some(cwd) = attribution.cwd.as_deref() {
        // Paths on macOS/Linux can contain non-ASCII bytes (Japanese
        // home dirs, accented characters); raw `HeaderValue::from_str`
        // would reject them and silently drop the header. Encode the
        // same way as the session name so the upstream sees a stable
        // ASCII form.
        let encoded = utf8_percent_encode(cwd, UTF8_VALUE_ESCAPE).to_string();
        try_insert_string(headers, X_TAPES_CWD, &encoded, &mut budget);
    }
    if let Some(name) = attribution.name.as_deref() {
        try_insert_session_name(headers, name, &mut budget);
    }
    if let Some(parent) = attribution.parent_sid.as_deref() {
        try_insert_string(
            headers,
            X_TAPES_PARENT_HARNESS_SESSION_ID,
            parent,
            &mut budget,
        );
    }

    // 3. Metadata blob — LOWEST priority and the first thing dropped
    //    when the envelope can't fit. By this point the non-metadata
    //    headers have already consumed their share of the budget.
    //    `try_insert_metadata` then checks the raw 4 KiB cap AND the
    //    remaining total budget BEFORE the insert. No "insert then
    //    remove" path: the encoded size is known up front from the
    //    base64url-encoded buffer length, so the drop semantics are
    //    stable regardless of any future reordering of the non-metadata
    //    headers above.
    try_insert_metadata(headers, attribution.metadata, &mut budget);

    Ok(())
}

/// Returns true when the inbound request already has the minimum
/// envelope tapes needs to group turns under a stable session. Used for
/// harnesses whose session identity is supplied by a managed extension
/// rather than by this crate's session watchers.
///
/// Delegates to [`TapesAttribution::from_headers`] so the rule has exactly one
/// implementation — see that constructor for why a second spelling is a bug
/// rather than a duplication.
#[must_use]
pub fn has_complete_inbound_envelope(headers: &HeaderMap) -> bool {
    TapesAttribution::from_headers(headers).is_some()
}

/// One `X-Tapes-*` header's value, trimmed, treating absent, non-ASCII,
/// and blank alike as "not stated".
///
/// Distinct from [`header_str`], which deliberately does not trim: that one
/// feeds [`HarnessThreadRule::DivergentPair`], where the comparison is against
/// another raw header value and must see the bytes that arrived.
fn envelope_field(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Remove every `X-Tapes-*` header in `headers` in-place. Called from
/// the required-header failure path so we don't ship a partial
/// envelope. Implemented as collect-then-remove because `HeaderMap`'s
/// iteration borrows immutably; the allocation is bounded by the
/// inserted-so-far count (at most ~7 entries).
fn clear_tapes_headers(headers: &mut HeaderMap) {
    let to_remove: Vec<HeaderName> = headers
        .keys()
        .filter(|n| n.as_str().to_ascii_lowercase().starts_with(HEADER_PREFIX))
        .cloned()
        .collect();
    for name in to_remove {
        headers.remove(&name);
    }
}

/// Insert an ASCII-only header and decrement the budget. Returns an
/// error only if the (impossible) `from_str` fails — used for the
/// `X-Tapes-Harness-Id` header where the value is a known constant.
fn insert_required_ascii(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
    budget: &mut usize,
) -> Result<(), HeaderError> {
    let val = HeaderValue::from_str(value).context(header_error::InvalidValueSnafu)?;
    let cost = name.len() + value.len();
    *budget = budget.saturating_sub(cost);
    headers.insert(HeaderName::from_static(name), val);
    Ok(())
}

/// Insert `value` under `name` if (a) it fits in the remaining
/// budget and (b) it is a valid HTTP header value. Failure on either
/// front silently drops the header — optional fields drop rather than
/// erroring so a malformed value never fails the whole request.
fn try_insert_string(headers: &mut HeaderMap, name: &'static str, value: &str, budget: &mut usize) {
    let cost = name.len() + value.len();
    if cost > *budget {
        return;
    }
    let Ok(val) = HeaderValue::from_str(value) else {
        return;
    };
    *budget -= cost;
    headers.insert(HeaderName::from_static(name), val);
}

/// Percent-encode `name` (UTF-8 → ASCII), bounded to 256 raw bytes,
/// and insert if it fits the budget. Encoding expansion is bounded
/// (worst case ~3× for all-multibyte input); the budget check is on
/// the encoded length so even a heavy expansion can't overrun.
fn try_insert_session_name(headers: &mut HeaderMap, name: &str, budget: &mut usize) {
    let raw = if name.len() > X_TAPES_SESSION_NAME_CAP {
        // Truncate at a UTF-8 boundary at or below the cap so the
        // percent-encoder never sees a split codepoint.
        let mut end = X_TAPES_SESSION_NAME_CAP;
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        &name[..end]
    } else {
        name
    };
    let encoded = utf8_percent_encode(raw, UTF8_VALUE_ESCAPE).to_string();
    try_insert_string(headers, X_TAPES_SESSION_NAME, &encoded, budget);
}

/// Build the metadata JSON object (structured fields the producer cares
/// about plus the harness's verbatim `extra` map), base64url-encode
/// it, and insert if both the raw JSON fits the 4 KiB cap and the
/// encoded header fits the remaining total budget. Silently dropped
/// otherwise.
fn try_insert_metadata(
    headers: &mut HeaderMap,
    obj: serde_json::Map<String, serde_json::Value>,
    budget: &mut usize,
) {
    if obj.is_empty() {
        return;
    }
    let Ok(raw) = serde_json::to_vec(&serde_json::Value::Object(obj)) else {
        return;
    };
    if raw.len() > X_TAPES_METADATA_RAW_CAP {
        return;
    }
    let encoded = URL_SAFE_NO_PAD.encode(&raw);
    try_insert_string(headers, X_TAPES_HARNESS_METADATA, &encoded, budget);
}

// The vendored shared envelope fixtures: a public reader (under the
// `envelope-fixtures` feature) plus this crate's own producer-side oracle over
// them. Declared as a child module of `envelope` rather than as an integration
// test so it can construct a `TapesAttribution` field-by-field — the corpus
// covers harnesses and field combinations the named constructors don't express.
//
// Compiled for this crate's own tests regardless of the feature, so the oracle
// runs on a bare `cargo test`.
#[cfg(any(test, feature = "envelope-fixtures"))]
#[path = "envelope_fixtures.rs"]
pub mod fixtures;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn hop_by_hop_list_matches_rfc7230() {
        assert!(
            is_hop_by_hop("Connection"),
            "case-insensitive match for canonical-cased header"
        );
        assert!(is_hop_by_hop("transfer-encoding"), "lower-case input");
        assert!(is_hop_by_hop("PROXY-AUTHENTICATE"), "upper-case input");
        assert!(is_hop_by_hop("Keep-Alive"));
        assert!(is_hop_by_hop("TE"));
        assert!(is_hop_by_hop("Trailers"));
        assert!(is_hop_by_hop("Upgrade"));

        assert!(
            !is_hop_by_hop("Content-Length"),
            "end-to-end header is not hop-by-hop"
        );
        assert!(!is_hop_by_hop("Content-Type"));
        assert!(!is_hop_by_hop("X-Paper-Auth"));
    }

    #[test]
    fn is_hop_by_hop_matches_every_listed_header_in_any_case() {
        // `hop_by_hop_list_matches_rfc7230` above spot-checks three
        // entries; this covers the whole list, so an entry added to
        // HOP_BY_HOP_HEADERS in a form that defeats the comparison
        // (stray whitespace, embedded upper-case) fails here rather
        // than leaking a connection-scoped header across the proxy
        // boundary at runtime.
        //
        // The match is `eq_ignore_ascii_case` against a lower-cased
        // table, so every case permutation of a listed name must hit.
        for name in HOP_BY_HOP_HEADERS {
            let upper = name.to_ascii_uppercase();
            // Title-Case-Each-Word, the form an HTTP stack most often
            // presents (`Transfer-Encoding`, `Proxy-Authenticate`).
            let title: String = name
                .split('-')
                .map(|seg| {
                    let mut c = seg.chars();
                    match c.next() {
                        Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join("-");

            assert!(is_hop_by_hop(name), "lower-case `{name}` must match");
            assert!(is_hop_by_hop(&upper), "upper-case `{upper}` must match");
            assert!(is_hop_by_hop(&title), "title-case `{title}` must match");
        }

        // The table itself must stay lower-case: the comparison is
        // case-insensitive, but a mixed-case entry would still be a
        // latent trap for any caller that compares against the
        // constant directly instead of going through `is_hop_by_hop`.
        for name in HOP_BY_HOP_HEADERS {
            assert_eq!(
                *name,
                name.to_ascii_lowercase(),
                "HOP_BY_HOP_HEADERS entries are listed lower-case",
            );
        }
    }

    /// A harness session for header tests, modelled on the shape Claude
    /// publishes — modelled fields that become metadata keys, plus a verbatim
    /// `extra` passthrough — without importing any harness's own type. The
    /// producer must work for whatever satisfies [`HarnessSession`]; testing
    /// it through one harness's struct would let a change to that struct
    /// masquerade as a change to the wire format.
    struct SampleSession {
        session_id: String,
        cwd: Option<String>,
        version: Option<String>,
        peer_protocol: Option<i64>,
        kind: Option<String>,
        entrypoint: Option<String>,
        name: Option<String>,
        extra: serde_json::Map<String, serde_json::Value>,
    }

    impl HarnessSession for SampleSession {
        fn harness_id(&self) -> &str {
            HARNESS_ID_CLAUDE
        }
        fn session_id(&self) -> &str {
            &self.session_id
        }
        fn version(&self) -> Option<&str> {
            self.version.as_deref()
        }
        fn cwd(&self) -> Option<&str> {
            self.cwd.as_deref()
        }
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }
        fn metadata(&self) -> serde_json::Map<String, serde_json::Value> {
            let mut metadata = serde_json::Map::new();
            if let Some(kind) = &self.kind {
                metadata.insert("kind".to_owned(), serde_json::Value::String(kind.clone()));
            }
            if let Some(entrypoint) = &self.entrypoint {
                metadata.insert(
                    "entrypoint".to_owned(),
                    serde_json::Value::String(entrypoint.clone()),
                );
            }
            if let Some(pp) = self.peer_protocol {
                metadata.insert(
                    "peerProtocol".to_owned(),
                    serde_json::Value::Number(pp.into()),
                );
            }
            for (k, v) in &self.extra {
                metadata.insert(k.clone(), v.clone());
            }
            metadata
        }
    }

    /// Build a session with sensible defaults for header tests. Override what
    /// the test cares about by mutation after the call.
    fn sample_session() -> SampleSession {
        SampleSession {
            session_id: "eae77e15-c7d2-4883-b82e-251161f8eeb3".to_owned(),
            cwd: Some("/Users/matt/code".to_owned()),
            version: Some("2.1.145".to_owned()),
            peer_protocol: Some(1),
            kind: Some("interactive".to_owned()),
            entrypoint: Some("cli".to_owned()),
            name: Some("woo-names".to_owned()),
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn unattributed_envelope_is_the_unknown_sentinel_alone() {
        // Cold-race / non-Claude callers (curl, health probes,
        // unparsed metadata) land on the unknown-harness path:
        // exactly one header, value `unknown`.
        let mut headers = HeaderMap::new();
        inject_unattributed_envelope(&mut headers).unwrap();

        assert_eq!(
            headers.get(X_TAPES_HARNESS_ID).unwrap().to_str().unwrap(),
            HARNESS_ID_UNKNOWN
        );
        // The unknown-harness path attaches nothing else.
        assert!(!headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
        assert!(!headers.contains_key(X_TAPES_CWD));
        assert!(!headers.contains_key(X_TAPES_SESSION_NAME));
        assert!(!headers.contains_key(X_TAPES_HARNESS_METADATA));
    }

    #[test]
    fn unattributed_envelope_names_no_fork_parent() {
        // The unknown path has no harness session to be a fork of, so it
        // emits no lineage. This used to be stated by passing a `parent_sid`
        // and requiring it to be ignored; the unattributed entry point no
        // longer accepts one, which makes the property structural. The
        // assertion stays because "structurally impossible" is a claim about
        // today's signature, and the header is the thing that must be absent.
        let mut headers = HeaderMap::new();
        inject_unattributed_envelope(&mut headers).unwrap();
        assert!(!headers.contains_key(X_TAPES_PARENT_HARNESS_SESSION_ID));
    }

    #[test]
    fn inject_tapes_attribution_codex_without_session_id() {
        let mut headers = HeaderMap::new();
        inject_tapes_attribution(&mut headers, TapesAttribution::codex()).unwrap();

        assert_eq!(
            headers.get(X_TAPES_HARNESS_ID).unwrap().to_str().unwrap(),
            HARNESS_ID_CODEX
        );
        assert!(!headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
        assert!(!headers.contains_key(X_TAPES_HARNESS_METADATA));
    }

    #[test]
    fn inject_tapes_attribution_codex_with_session_metadata() {
        let mut headers = HeaderMap::new();
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "originator".to_owned(),
            serde_json::Value::String("codex-tui".to_owned()),
        );
        inject_tapes_attribution(
            &mut headers,
            TapesAttribution::codex_session(
                "019ecd8e-4281-7353-8a00-09df678443b1",
                Some("/Users/matt/code"),
                Some("0.139.0"),
                metadata,
            ),
        )
        .unwrap();

        assert_eq!(
            headers.get(X_TAPES_HARNESS_ID).unwrap().to_str().unwrap(),
            HARNESS_ID_CODEX
        );
        assert_eq!(
            headers
                .get(X_TAPES_HARNESS_SESSION_ID)
                .unwrap()
                .to_str()
                .unwrap(),
            "019ecd8e-4281-7353-8a00-09df678443b1"
        );
        assert_eq!(
            headers
                .get(X_TAPES_HARNESS_VERSION)
                .unwrap()
                .to_str()
                .unwrap(),
            "0.139.0"
        );
        let raw = URL_SAFE_NO_PAD
            .decode(
                headers
                    .get(X_TAPES_HARNESS_METADATA)
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(json["originator"], "codex-tui");
    }

    /// Ported from paperd's `TapesAttributionExt` shim (PCC-1056): the
    /// metadata-only and parent-lineage constructors fill exactly the
    /// fields their arguments name and nothing else.
    #[test]
    fn codex_constructors_fill_only_the_fields_their_arguments_name() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("k".to_owned(), serde_json::Value::String("v".to_owned()));

        let bare = TapesAttribution::codex_with_metadata(metadata.clone());
        assert_eq!(bare.harness_id, HARNESS_ID_CODEX);
        assert!(bare.session_id.is_none());
        assert!(bare.parent_sid.is_none());
        assert_eq!(bare.metadata, metadata);

        let keyed = TapesAttribution::codex_session_with_parent(
            "sid-1",
            Some("parent-sid"),
            Some("/tmp/x"),
            Some("0.99.0"),
            metadata.clone(),
        );
        assert_eq!(keyed.harness_id, HARNESS_ID_CODEX);
        assert_eq!(keyed.session_id.as_deref(), Some("sid-1"));
        assert_eq!(keyed.parent_sid.as_deref(), Some("parent-sid"));
        assert_eq!(keyed.cwd.as_deref(), Some("/tmp/x"));
        assert_eq!(keyed.version.as_deref(), Some("0.99.0"));
        assert_eq!(keyed.metadata, metadata);

        // The pre-existing constructors are the no-argument special
        // cases of the new ones and must stay behaviorally identical.
        let plain = TapesAttribution::codex();
        assert!(plain.session_id.is_none() && plain.metadata.is_empty());
        let sessioned =
            TapesAttribution::codex_session("sid-1", Some("/tmp/x"), Some("0.99.0"), metadata);
        assert!(sessioned.parent_sid.is_none());
        assert_eq!(sessioned.session_id.as_deref(), Some("sid-1"));
    }

    #[test]
    fn unattributed_envelope_preserves_a_complete_inbound_one() {
        let mut headers = HeaderMap::new();
        headers.insert(X_TAPES_HARNESS_ID, HeaderValue::from_static(HARNESS_ID_PI));
        headers.insert(
            X_TAPES_HARNESS_SESSION_ID,
            HeaderValue::from_static("paper-pi-test-session"),
        );

        inject_unattributed_envelope(&mut headers).unwrap();

        assert_eq!(
            headers.get(X_TAPES_HARNESS_ID).unwrap().to_str().unwrap(),
            HARNESS_ID_PI
        );
        assert_eq!(
            headers
                .get(X_TAPES_HARNESS_SESSION_ID)
                .unwrap()
                .to_str()
                .unwrap(),
            "paper-pi-test-session"
        );
    }

    #[test]
    fn unattributed_envelope_replaces_a_partial_inbound_one() {
        let mut headers = HeaderMap::new();
        headers.insert(X_TAPES_HARNESS_ID, HeaderValue::from_static(HARNESS_ID_PI));

        inject_unattributed_envelope(&mut headers).unwrap();

        assert_eq!(
            headers.get(X_TAPES_HARNESS_ID).unwrap().to_str().unwrap(),
            HARNESS_ID_UNKNOWN
        );
        assert!(!headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
    }

    #[test]
    fn unattributed_envelope_clears_an_orphan_session_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            X_TAPES_HARNESS_SESSION_ID,
            HeaderValue::from_static("orphan-pi-session"),
        );

        inject_unattributed_envelope(&mut headers).unwrap();

        assert_eq!(
            headers.get(X_TAPES_HARNESS_ID).unwrap().to_str().unwrap(),
            HARNESS_ID_UNKNOWN
        );
        assert!(!headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
    }

    #[test]
    fn session_envelope_is_well_formed() {
        // Happy path: full session, no parent. All structured fields
        // populated → all corresponding headers present with the
        // verbatim values, plus base64url-encoded metadata.
        let mut headers = HeaderMap::new();
        let session = sample_session();
        inject_session_envelope(&mut headers, &session, None).unwrap();

        assert_eq!(
            headers.get(X_TAPES_HARNESS_ID).unwrap().to_str().unwrap(),
            HARNESS_ID_CLAUDE
        );
        assert_eq!(
            headers
                .get(X_TAPES_HARNESS_SESSION_ID)
                .unwrap()
                .to_str()
                .unwrap(),
            session.session_id
        );
        assert_eq!(
            headers
                .get(X_TAPES_HARNESS_VERSION)
                .unwrap()
                .to_str()
                .unwrap(),
            "2.1.145"
        );
        assert_eq!(
            headers.get(X_TAPES_CWD).unwrap().to_str().unwrap(),
            "/Users/matt/code"
        );
        assert_eq!(
            headers.get(X_TAPES_SESSION_NAME).unwrap().to_str().unwrap(),
            "woo-names"
        );
        assert!(!headers.contains_key(X_TAPES_PARENT_HARNESS_SESSION_ID));

        // Metadata is base64url(no-pad) of the JSON {kind,
        // entrypoint, peerProtocol}. Decode and check structure
        // instead of comparing exact bytes so the assertion isn't
        // brittle to JSON key ordering.
        let encoded = headers
            .get(X_TAPES_HARNESS_METADATA)
            .unwrap()
            .to_str()
            .unwrap();
        let raw = URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(json["kind"], "interactive");
        assert_eq!(json["entrypoint"], "cli");
        assert_eq!(json["peerProtocol"], 1);
    }

    #[test]
    fn session_envelope_attaches_parent_when_present() {
        let mut headers = HeaderMap::new();
        let session = sample_session();
        inject_session_envelope(&mut headers, &session, Some("parent-sid-uuid")).unwrap();
        assert_eq!(
            headers
                .get(X_TAPES_PARENT_HARNESS_SESSION_ID)
                .unwrap()
                .to_str()
                .unwrap(),
            "parent-sid-uuid"
        );
    }

    #[test]
    fn session_envelope_omits_unset_optionals() {
        // None for an optional field means "harness didn't write
        // it". We omit the header rather than emitting a sentinel
        // empty value so absent and empty stay distinguishable
        // downstream.
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        session.cwd = None;
        session.version = None;
        session.name = None;
        // Metadata blob still has kind/entrypoint/peerProtocol so it
        // stays — we're testing the structured-string omissions.
        inject_session_envelope(&mut headers, &session, None).unwrap();

        assert!(headers.contains_key(X_TAPES_HARNESS_ID));
        assert!(headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
        assert!(!headers.contains_key(X_TAPES_CWD));
        assert!(!headers.contains_key(X_TAPES_HARNESS_VERSION));
        assert!(!headers.contains_key(X_TAPES_SESSION_NAME));
    }

    #[test]
    fn session_envelope_percent_encodes_unicode_session_name() {
        // The session name is the only header value that may carry
        // arbitrary UTF-8 (slash-command `/name` accepts anything
        // the user types). Encoded form must be ASCII per RFC 7230.
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        // A non-ASCII codepoint and a structural ASCII byte, so we
        // exercise the encoder's UTF-8 and ASCII-escape paths in one
        // shot.
        session.name = Some("name with space \"quotes\" café".to_owned());
        inject_session_envelope(&mut headers, &session, None).unwrap();

        let v = headers
            .get(X_TAPES_SESSION_NAME)
            .unwrap()
            .to_str()
            .expect("encoded header is ASCII");
        // Space and " are percent-encoded; the e-acute encodes to
        // its UTF-8 bytes `%C3%A9`.
        assert!(v.contains("%20"), "space is percent-encoded: {v}");
        assert!(v.contains("%22"), "quote is percent-encoded: {v}");
        assert!(
            v.contains("%C3%A9"),
            "non-ASCII is UTF-8 percent-encoded: {v}"
        );
        assert!(v.is_ascii(), "encoded value must be pure ASCII");
    }

    #[test]
    fn session_envelope_truncates_session_name_at_utf8_boundary() {
        // Names beyond X_TAPES_SESSION_NAME_CAP (256 B raw) are
        // truncated to the cap before encoding. Truncation must
        // happen at a UTF-8 codepoint boundary so the encoder
        // doesn't see a split codepoint.
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        // 100 copies of a 3-byte codepoint (Thai `ก` = 0xE0 0xB8 0x81)
        // = 300 raw bytes, which exceeds the 256-byte cap. 256 mod 3
        // == 1, so byte 256 is mid-codepoint — the truncation logic
        // MUST walk back to byte 255 to land on a boundary (the start
        // of the 86th codepoint at offset 255, which we then drop).
        // After truncation: 85 codepoints survive (255 bytes raw),
        // each percent-encoded to `%E0%B8%81` (9 ASCII bytes), so
        // encoded length is 85 × 9 = 765.
        session.name = Some("ก".repeat(100));
        inject_session_envelope(&mut headers, &session, None).unwrap();

        // Header value is ASCII (percent-encoded). The function
        // must not have panicked on an invalid UTF-8 slice.
        let v = headers.get(X_TAPES_SESSION_NAME).unwrap().to_str().unwrap();
        assert!(v.is_ascii(), "encoded value is ASCII");
        assert_eq!(
            v.len(),
            85 * 9,
            "85 codepoints survive truncation (raw=255 ≤ cap=256)",
        );
    }

    #[test]
    fn session_envelope_drops_oversize_metadata() {
        // The metadata blob is dropped (silently) when the raw
        // JSON exceeds X_TAPES_METADATA_RAW_CAP (4 KiB). The other
        // X-Tapes-* headers stay; only the metadata is omitted.
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        // 5 KiB of opaque content via the `extra` blob — this is
        // exactly the failure mode the cap defends against: a
        // future harness key whose value is too large.
        let huge: String = "x".repeat(5 * 1024);
        session
            .extra
            .insert("hugeKnob".to_owned(), serde_json::Value::String(huge));

        inject_session_envelope(&mut headers, &session, None).unwrap();

        assert!(headers.contains_key(X_TAPES_HARNESS_ID));
        assert!(headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
        assert!(headers.contains_key(X_TAPES_CWD));
        assert!(
            !headers.contains_key(X_TAPES_HARNESS_METADATA),
            "metadata blob dropped when raw JSON exceeds 4 KiB cap",
        );
    }

    #[test]
    fn session_envelope_metadata_includes_extra_keys() {
        // Forward-compat: anything the harness writes that we don't
        // model explicitly flows through the `extra` map into the
        // metadata blob unchanged, so new keys travel upstream without
        // a capture-client release.
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        session.extra.insert(
            "futureKnob".to_owned(),
            serde_json::Value::String("preserved".to_owned()),
        );
        inject_session_envelope(&mut headers, &session, None).unwrap();

        let encoded = headers
            .get(X_TAPES_HARNESS_METADATA)
            .unwrap()
            .to_str()
            .unwrap();
        let raw = URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(json["futureKnob"], "preserved");
        assert_eq!(json["kind"], "interactive");
    }

    #[test]
    fn session_envelope_metadata_empty_when_no_blob_fields() {
        // No kind / entrypoint / peerProtocol / extra → the
        // metadata blob would be an empty JSON object. We omit the
        // header entirely instead of attaching `{}` (the extra
        // round-trip costs ~70 bytes for nothing).
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        session.kind = None;
        session.entrypoint = None;
        session.peer_protocol = None;
        session.extra.clear();
        inject_session_envelope(&mut headers, &session, None).unwrap();

        assert!(!headers.contains_key(X_TAPES_HARNESS_METADATA));
        // Other headers still present.
        assert!(headers.contains_key(X_TAPES_HARNESS_ID));
        assert!(headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
    }

    #[test]
    fn session_envelope_escapes_control_bytes_in_cwd() {
        // Cwd is percent-encoded UTF-8 on the wire, so CR/LF/NUL
        // bytes — which RFC 7230 forbids in a raw header value, and
        // which would let an attacker inject a second header — are
        // escaped to `%0A` / `%0D` / `%00`. The header lands rather
        // than being silently dropped, and the encoded form is safe
        // for any HTTP intermediary to forward verbatim.
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        session.cwd = Some("/Users/matt\nwith-injection: yes".to_owned());
        inject_session_envelope(&mut headers, &session, None).unwrap();

        let v = headers.get(X_TAPES_CWD).unwrap().to_str().unwrap();
        assert!(v.contains("%0A"), "newline is percent-encoded: {v}");
        assert!(
            !v.contains('\n'),
            "no raw CR/LF survives into the header value: {v}",
        );
        assert!(v.is_ascii(), "encoded value must be pure ASCII");
        assert!(headers.contains_key(X_TAPES_HARNESS_ID));
    }

    #[test]
    fn session_envelope_percent_encodes_unicode_cwd() {
        // Working directories on macOS/Linux can contain non-ASCII
        // bytes (Japanese home dirs, accented characters, emoji). A
        // raw `HeaderValue::from_str` only accepts visible ASCII, so
        // before this encoding was added the header was silently
        // dropped for these users. The encoded form is pure ASCII
        // and the upstream decoder percent-decodes it back.
        let mut headers = HeaderMap::new();
        let mut session = sample_session();
        session.cwd = Some("/Users/松本/code".to_owned());
        inject_session_envelope(&mut headers, &session, None).unwrap();

        let v = headers.get(X_TAPES_CWD).unwrap().to_str().unwrap();
        assert!(v.is_ascii(), "encoded value must be pure ASCII");
        // 松 = U+677E = 0xE6 0x9D 0xBE in UTF-8; 本 = U+672C = 0xE6 0x9C 0xAC.
        assert!(v.contains("%E6%9D%BE"), "first codepoint encoded: {v}");
        assert!(v.contains("%E6%9C%AC"), "second codepoint encoded: {v}");
        // ASCII path segments survive verbatim.
        assert!(v.starts_with("/Users/"), "ASCII prefix preserved: {v}");
        assert!(v.ends_with("/code"), "ASCII suffix preserved: {v}");
    }

    #[test]
    fn session_name_truncation_lands_at_or_below_cap_for_each_byte_offset() {
        // Pin the UTF-8 truncation behaviour across the cap
        // boundary. For each raw input length straddling the 256-byte
        // cap, the function must (a) not panic, (b) emit ASCII, and
        // (c) drop nothing more than needed to land on a codepoint
        // boundary at or below the cap.
        //
        // Two flavours of input:
        // * ASCII (every byte is a boundary): truncation lands
        //   exactly on the cap when raw > 256.
        // * 3-byte codepoint × n (Thai `ก` = 0xE0 0xB8 0x81): 256
        //   mod 3 == 1, so byte 256 is mid-codepoint. The truncator
        //   walks back to byte 255 (boundary), keeping 85
        //   codepoints when n_copies > 85.

        // ASCII case: cap is trivially a boundary.
        for raw_len in [254usize, 255, 256, 257, 258, 299, 300, 301] {
            let ascii = "a".repeat(raw_len);
            let mut h = HeaderMap::new();
            let mut budget = X_TAPES_TOTAL_BUDGET;
            try_insert_session_name(&mut h, &ascii, &mut budget);
            let v = h.get(X_TAPES_SESSION_NAME).unwrap().to_str().unwrap();
            assert!(v.is_ascii(), "ascii input @ {raw_len} yields ASCII");
            // No escapable bytes in [a-z], so encoded == truncated raw.
            let expected = raw_len.min(X_TAPES_SESSION_NAME_CAP);
            assert_eq!(
                v.len(),
                expected,
                "ascii input @ {raw_len}: encoded length must equal min(raw, cap)",
            );
        }

        // 3-byte codepoint case: walk-back kicks in once raw > cap.
        // n_copies × 3 raw bytes, then truncate to ≤ cap on a
        // boundary, then percent-encode (9 ASCII bytes per `ก`).
        for n_copies in [84usize, 85, 86, 87, 100] {
            let s = "ก".repeat(n_copies);
            let raw_len = s.len();
            assert_eq!(raw_len, n_copies * 3, "ก is 3 raw UTF-8 bytes");
            let mut h = HeaderMap::new();
            let mut budget = X_TAPES_TOTAL_BUDGET;
            try_insert_session_name(&mut h, &s, &mut budget);
            let v = h.get(X_TAPES_SESSION_NAME).unwrap().to_str().unwrap();
            assert!(v.is_ascii(), "utf8 input @ {raw_len} yields ASCII");
            // If raw ≤ cap: every codepoint survives.
            // If raw  > cap: walk back from 256 → 255 boundary →
            //   floor(255/3) = 85 codepoints survive.
            let kept = if raw_len <= X_TAPES_SESSION_NAME_CAP {
                n_copies
            } else {
                X_TAPES_SESSION_NAME_CAP / 3
            };
            assert_eq!(
                v.len(),
                kept * 9,
                "utf8 input ({n_copies} × ก, raw={raw_len}): \
                 encoded length matches kept codepoints",
            );
        }
    }

    #[test]
    fn clear_tapes_headers_removes_all_envelope_headers() {
        // When the required-header insert fails we MUST wipe the
        // partial envelope before falling back to `unknown`. This test
        // pins the helper's behaviour: every `X-Tapes-*` header (any
        // case) is removed; unrelated headers are kept.
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(X_TAPES_HARNESS_ID),
            HeaderValue::from_static("claude"),
        );
        headers.insert(
            HeaderName::from_static(X_TAPES_HARNESS_SESSION_ID),
            HeaderValue::from_static("sid"),
        );
        headers.insert(
            HeaderName::from_static(X_TAPES_CWD),
            HeaderValue::from_static("/tmp"),
        );
        headers.insert(
            HeaderName::from_static(X_TAPES_HARNESS_METADATA),
            HeaderValue::from_static("payload"),
        );
        // An unrelated header survives the wipe — clear_tapes_headers
        // is scoped to the X-Tapes-* prefix only.
        headers.insert("authorization", HeaderValue::from_static("Bearer foo"));

        clear_tapes_headers(&mut headers);

        assert!(!headers.contains_key(X_TAPES_HARNESS_ID));
        assert!(!headers.contains_key(X_TAPES_HARNESS_SESSION_ID));
        assert!(!headers.contains_key(X_TAPES_CWD));
        assert!(!headers.contains_key(X_TAPES_HARNESS_METADATA));
        assert!(
            headers.contains_key("authorization"),
            "non-tapes headers are preserved"
        );
    }

    // --- sub-thread resolution ------------------------------------------
    //
    // These mirror tapes-extproc's `TestThreadID` case for case. The ids are
    // the same captured wire evidence its table uses, so a divergence between
    // the two implementations shows up as one of these failing rather than as
    // a mis-shaped session weeks later.

    const CODEX_ROOT: &str = "019f863d-0cd6-7ce2-b481-20abd683a14e";
    const CODEX_CHILD: &str = "019f8713-2213-75e3-be33-36fd2f8dd384";
    const CLAUDE_AGENT: &str = "agent-0a1b2c3d";

    fn headers_from(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn thread_id_reads_the_claude_subagent_header() {
        let headers = headers_from(&[("x-claude-code-agent-id", CLAUDE_AGENT)]);
        assert_eq!(thread_id(&headers), Some(CLAUDE_AGENT));
    }

    #[test]
    fn thread_id_is_absent_on_a_main_thread_call() {
        // Claude Code omits the header entirely on the main thread, which is
        // what makes its presence a reliable subagent signal.
        let headers = headers_from(&[("content-type", "application/json")]);
        assert_eq!(thread_id(&headers), None);
    }

    #[test]
    fn a_blank_thread_id_counts_as_absent() {
        let headers = headers_from(&[("x-claude-code-agent-id", "")]);
        assert_eq!(thread_id(&headers), None);
    }

    #[test]
    fn a_codex_child_turn_resolves_to_its_thread_id() {
        let headers = headers_from(&[("session-id", CODEX_ROOT), ("thread-id", CODEX_CHILD)]);
        assert_eq!(thread_id(&headers), Some(CODEX_CHILD));
    }

    /// The root guard, and the reason Codex cannot be expressed as a flat
    /// first-present entry: it stamps `thread-id` on *every* call, equal to
    /// `session-id` on a root turn. A flat entry would stamp a thread id on
    /// every root turn and misroute the root spine.
    #[test]
    fn a_codex_root_turn_has_no_thread_id() {
        let headers = headers_from(&[("session-id", CODEX_ROOT), ("thread-id", CODEX_ROOT)]);
        assert_eq!(thread_id(&headers), None);
    }

    #[test]
    fn a_codex_session_id_alone_is_a_main_thread_call() {
        let headers = headers_from(&[("session-id", CODEX_ROOT)]);
        assert_eq!(thread_id(&headers), None);
    }

    /// The second guard: a `thread-id` with no `session-id` beside it is not a
    /// recognised Codex shape, so the pair rule declines rather than guessing
    /// on half the evidence.
    #[test]
    fn a_lone_thread_id_is_not_a_codex_shape() {
        let headers = headers_from(&[("thread-id", CODEX_CHILD)]);
        assert_eq!(thread_id(&headers), None);
    }

    /// Rule order is precedence. Only observable when one request carries
    /// evidence for two harnesses at once, which is exactly when a silent
    /// reordering would matter.
    #[test]
    fn the_claude_rule_wins_over_a_codex_shaped_pair() {
        let headers = headers_from(&[
            ("x-claude-code-agent-id", CLAUDE_AGENT),
            ("session-id", CODEX_ROOT),
            ("thread-id", CODEX_CHILD),
        ]);
        assert_eq!(thread_id(&headers), Some(CLAUDE_AGENT));
    }

    /// A complete inbound envelope reads back with its plain-text fields, and
    /// the encoded ones stay empty rather than half-parsed.
    #[test]
    fn from_headers_reads_a_complete_inbound_envelope() {
        let headers = headers_from(&[
            (X_TAPES_HARNESS_ID, HARNESS_ID_PI),
            (X_TAPES_HARNESS_SESSION_ID, "sess-1"),
            (X_TAPES_HARNESS_VERSION, "1.2.3"),
            (X_TAPES_PARENT_HARNESS_SESSION_ID, "sess-0"),
            // Wire-encoded fields the producer half deliberately does not
            // decode back; see `from_headers`.
            (X_TAPES_CWD, "%2Ftmp%2Fwork"),
            (X_TAPES_HARNESS_METADATA, "e30"),
        ]);

        let attribution = TapesAttribution::from_headers(&headers).expect("envelope is complete");
        assert_eq!(attribution.harness_id, HARNESS_ID_PI);
        assert_eq!(attribution.session_id.as_deref(), Some("sess-1"));
        assert_eq!(attribution.version.as_deref(), Some("1.2.3"));
        assert_eq!(attribution.parent_sid.as_deref(), Some("sess-0"));
        assert_eq!(attribution.cwd, None);
        assert_eq!(attribution.name, None);
        assert!(attribution.metadata.is_empty());
    }

    /// Every shape the completeness rule rejects, in one place. Each is a
    /// request a consumer must file as unattributed rather than under a
    /// half-stated identity.
    #[test]
    fn from_headers_rejects_incomplete_envelopes() {
        let cases: &[(&str, Vec<(&'static str, &str)>)] = &[
            ("no headers at all", vec![]),
            (
                "harness id but no session id",
                vec![(X_TAPES_HARNESS_ID, HARNESS_ID_PI)],
            ),
            (
                "session id but no harness id",
                vec![(X_TAPES_HARNESS_SESSION_ID, "sess-1")],
            ),
            (
                "the unknown sentinel is not an identity",
                vec![
                    (X_TAPES_HARNESS_ID, HARNESS_ID_UNKNOWN),
                    (X_TAPES_HARNESS_SESSION_ID, "sess-1"),
                ],
            ),
            (
                "a blank harness id",
                vec![
                    (X_TAPES_HARNESS_ID, "   "),
                    (X_TAPES_HARNESS_SESSION_ID, "sess-1"),
                ],
            ),
            (
                "a blank session id",
                vec![
                    (X_TAPES_HARNESS_ID, HARNESS_ID_PI),
                    (X_TAPES_HARNESS_SESSION_ID, ""),
                ],
            ),
        ];

        for (why, pairs) in cases {
            let headers = headers_from(pairs);
            assert!(
                TapesAttribution::from_headers(&headers).is_none(),
                "{why}: an incomplete envelope must not read back as an identity",
            );
        }
    }

    /// Values are trimmed, so whitespace padding neither defeats the
    /// completeness rule nor rides into the attribution.
    #[test]
    fn from_headers_trims_envelope_values() {
        let headers = headers_from(&[
            (X_TAPES_HARNESS_ID, "  pi  "),
            (X_TAPES_HARNESS_SESSION_ID, "  sess-1  "),
        ]);
        let attribution = TapesAttribution::from_headers(&headers).expect("padding is not absence");
        assert_eq!(attribution.harness_id, HARNESS_ID_PI);
        assert_eq!(attribution.session_id.as_deref(), Some("sess-1"));
    }

    /// The bug this hoist exists to prevent: the rule that decides whether the
    /// producer PRESERVES an inbound envelope and the rule that decides
    /// whether a consumer FILES a turn under one must be the same rule. If
    /// they ever diverge, a request's headers say `pi` while its ingest row
    /// says `unknown`.
    ///
    /// Asserted as agreement across the whole case table rather than by
    /// inspecting either implementation, so a future re-spelling of one side
    /// fails here.
    #[test]
    fn envelope_preservation_and_readback_apply_one_rule() {
        let cases: &[Vec<(&'static str, &str)>] = &[
            vec![],
            vec![(X_TAPES_HARNESS_ID, HARNESS_ID_PI)],
            vec![(X_TAPES_HARNESS_SESSION_ID, "sess-1")],
            vec![
                (X_TAPES_HARNESS_ID, HARNESS_ID_UNKNOWN),
                (X_TAPES_HARNESS_SESSION_ID, "sess-1"),
            ],
            vec![
                (X_TAPES_HARNESS_ID, HARNESS_ID_PI),
                (X_TAPES_HARNESS_SESSION_ID, ""),
            ],
            vec![
                (X_TAPES_HARNESS_ID, HARNESS_ID_PI),
                (X_TAPES_HARNESS_SESSION_ID, "sess-1"),
            ],
        ];

        for pairs in cases {
            let mut headers = headers_from(pairs);
            let readable = TapesAttribution::from_headers(&headers).is_some();
            assert_eq!(
                has_complete_inbound_envelope(&headers),
                readable,
                "the predicate and the reader disagree about {pairs:?}",
            );

            // And the producer must act on that same answer: a complete
            // envelope survives an unattributed injection untouched, an
            // incomplete one is replaced by the `unknown` sentinel.
            let before = headers.clone();
            inject_unattributed_envelope(&mut headers).unwrap();
            if readable {
                assert_eq!(
                    headers.get(X_TAPES_HARNESS_ID),
                    before.get(X_TAPES_HARNESS_ID),
                    "a complete envelope was overwritten: {pairs:?}",
                );
            } else {
                assert_eq!(
                    headers.get(X_TAPES_HARNESS_ID).unwrap(),
                    HARNESS_ID_UNKNOWN,
                    "an incomplete envelope was not replaced with the sentinel: {pairs:?}",
                );
            }
        }
    }

    /// The pair rule reads the same header spellings the rollout-id lookup
    /// does. They answer different questions and must not drift apart.
    #[test]
    fn the_codex_pair_names_the_rollout_id_headers() {
        let pair = HARNESS_THREAD_ID_RULES
            .iter()
            .find_map(|rule| match rule {
                HarnessThreadRule::DivergentPair { thread, session } => Some((*thread, *session)),
                HarnessThreadRule::FirstPresent(_) => None,
            })
            .expect("codex is declared as a divergent pair");
        assert_eq!(
            [pair.0, pair.1],
            [
                crate::attribution::codex::session::CODEX_ROLLOUT_ID_HEADERS[0],
                crate::attribution::codex::session::CODEX_ROLLOUT_ID_HEADERS[1],
            ],
        );
    }
}
