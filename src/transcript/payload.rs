//! The transcript-lane wire payload.
//!
//! Extracted from paperd's `transcript_upload::client`, minus the HTTP call. The
//! shape is a cross-language contract with tapes' Go server
//! (`ingest.TranscriptPayload` / `pkg/sessions.IngestEnvelope`) and the Go
//! reference client (`pkg/backfill/transcript_upload.go`):
//!
//! ```json
//! {
//!   "session": {
//!     "org_id": "",
//!     "auth_subject": "",
//!     "harness_id": "claude",
//!     "harness_session_id": "<sid>",
//!     "harness_version": "2.1.161",
//!     "cwd": "/Users/me/src/repo"
//!   },
//!   "agent_id": "<id>",          // subagent files only
//!   "agent_type": "...",         // from agent-<id>.meta.json
//!   "description": "...",
//!   "tool_use_id": "toolu_...",  // the fork edge
//!   "kind": "interacted",        // anchor re-entry rows only
//!   "records": [ ...verbatim JSONL lines... ]
//! }
//! ```
//!
//! `org_id` and `auth_subject` serialize as empty strings rather than being
//! omitted, matching Go's non-`omitempty` fields — so they are non-optional
//! `&str` here and a caller with nothing to say passes `""`.
//!
//! The endpoint is idempotent: the server keys raw rows on
//! `transcript:<sid>:<agent|main>:<sha256(records)[..8]>`, so re-pushing
//! unchanged files answers `{"deduped": true}` and grown files append a new
//! version. That is what makes the eager trigger in [`super::trigger`] and
//! sweep-on-start in [`super::sweep`] safe.
//!
//! # What stays with the client
//!
//! Delivery: the HTTP client, the request timeout, the response and dedup-flag
//! parsing, and above all **auth**. paperd rides its own `X-Paper-Auth` channel
//! so the Paper cloud edge admits the request; a standalone client authenticates
//! differently or not at all. None of that is harness knowledge, and the
//! `X-Paper-Auth` header in particular is explicitly not part of the tapes
//! contract.

use serde::Serialize;
use serde_json::value::RawValue;

use super::files::TranscriptFile;

/// Path of the transcript-ingest endpoint, joined onto a client's base URL.
pub const INGEST_PATH: &str = "/v1/ingest/transcript";

/// The session a transcript belongs to, as the ingest lane needs it.
///
/// A client's own session registry will carry more than this (a pid to watch,
/// bookkeeping for backoff); this is the subset that reaches the wire. Kept
/// owned rather than borrowed so a client can build one from a swept transcript
/// (see [`super::sweep`]) as easily as from a live registry entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSession {
    /// Which harness produced it — `claude` today. Should match the
    /// `X-Tapes-Harness-Id` the same session's wire traffic carries, so the
    /// deriver fuses both sources under one session row.
    pub harness_id: String,
    /// The harness session id. **Required** by the server for transcripts; it is
    /// what names `<sid>.jsonl` and keys the upload.
    pub harness_session_id: String,
    /// Harness version, when known.
    pub harness_version: Option<String>,
    /// Working directory the harness ran in, when known.
    pub cwd: Option<String>,
    /// Organization id, or empty. paperd leaves this empty deliberately: its
    /// `paper_org_id` is a WorkOS id (`org_…`) that the server's envelope
    /// validation rejects (it requires a UUID), and the wire-capture path derives
    /// org identity from the JWT at the edge rather than from this envelope.
    pub org_id: String,
    /// Acting subject, or empty. A standalone client conventionally sets
    /// `local:<os-username>`; on the platform the cloud edge stamps it from
    /// validated JWT claims. Nothing parses the prefix — see
    /// [`crate::attribution::Attribution::auth_subject`].
    pub auth_subject: String,
}

impl TranscriptSession {
    /// A session envelope with only the required fields set.
    pub fn new(harness_id: impl Into<String>, harness_session_id: impl Into<String>) -> Self {
        Self {
            harness_id: harness_id.into(),
            harness_session_id: harness_session_id.into(),
            harness_version: None,
            cwd: None,
            org_id: String::new(),
            auth_subject: String::new(),
        }
    }

    /// Set the harness version.
    #[must_use]
    pub fn with_harness_version(mut self, version: Option<String>) -> Self {
        self.harness_version = version;
        self
    }

    /// Set the working directory.
    #[must_use]
    pub fn with_cwd(mut self, cwd: Option<String>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Set the acting subject.
    #[must_use]
    pub fn with_auth_subject(mut self, auth_subject: impl Into<String>) -> Self {
        self.auth_subject = auth_subject.into();
        self
    }
}

/// Session envelope, field-for-field with tapes' `pkg/sessions.IngestEnvelope`
/// (the subset the transcript lane populates).
#[derive(Debug, Serialize)]
pub struct IngestEnvelope<'a> {
    /// Empty string when unknown — never omitted; see the module docs.
    pub org_id: &'a str,
    /// Empty string when unknown — never omitted; see the module docs.
    pub auth_subject: &'a str,
    /// Which harness produced the transcript.
    pub harness_id: &'a str,
    /// The harness session id; REQUIRED by the server for transcripts.
    pub harness_session_id: &'a str,
    /// Harness version, omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness_version: Option<&'a str>,
    /// Working directory, omitted when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<&'a str>,
}

/// Ingest body for one transcript file, mirroring tapes'
/// `ingest.TranscriptPayload`.
#[derive(Debug, Serialize)]
pub struct TranscriptPayload<'a> {
    /// Session the transcript belongs to.
    pub session: IngestEnvelope<'a>,
    /// Absent for the main transcript; the subagent id otherwise. Matches Go's
    /// `omitempty` by omitting `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<&'a str>,
    /// Subagent type from `agent-<id>.meta.json`, omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<&'a str>,
    /// Task description from `agent-<id>.meta.json`, omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    /// The `Task` tool_use that forked this subagent — the causal edge the tapes
    /// deriver attaches. Omitted when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<&'a str>,
    /// Lifecycle qualifier for a Codex `sub_agent_activity` anchor row.
    ///
    /// `Some("interacted")` marks a re-entry record — a `send_message` or
    /// `followup_task` aimed at an already-spawned thread, with `agent_id` the
    /// target thread and `tool_use_id` the triggering call. `None` means spawn
    /// evidence, which is the legacy default and the only thing a transcript
    /// file ever is: [`build_payload`] therefore always leaves it unset, and an
    /// anchor builder sets it through this field directly.
    ///
    /// The server keys a raw row's dedup on the payload bytes and reads the
    /// latest version per (session, agent, lifecycle kind), so an `interacted`
    /// row versions separately from the `started` anchor it shares an
    /// `agent_id` with rather than superseding it. That is also why the field
    /// is `Option` with `skip_serializing_if` rather than an empty-string
    /// sentinel, and why it sits *after* `tool_use_id`: a spawn row's bytes
    /// must stay identical to what earlier builds already ingested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'a str>,
    /// The transcript's JSONL content as a JSON array, verbatim.
    pub records: &'a RawValue,
}

/// The [`TranscriptPayload::kind`] value marking a re-entry anchor row.
pub const KIND_INTERACTED: &str = "interacted";

/// Assemble the payload for one transcript file.
///
/// `records` is the output of [`super::files::jsonl_to_records`] wrapped in a
/// [`RawValue`] so the bytes embed verbatim — re-serializing them would change
/// the server's dedup hash and register identical content as a new version.
#[must_use]
pub fn build_payload<'a>(
    session: &'a TranscriptSession,
    file: &'a TranscriptFile,
    records: &'a RawValue,
) -> TranscriptPayload<'a> {
    // Empty meta fields are omitted, matching Go's `omitempty`.
    let some_nonempty = |s: &'a str| (!s.is_empty()).then_some(s);
    TranscriptPayload {
        session: IngestEnvelope {
            org_id: &session.org_id,
            auth_subject: &session.auth_subject,
            harness_id: &session.harness_id,
            harness_session_id: &session.harness_session_id,
            harness_version: session.harness_version.as_deref(),
            cwd: session.cwd.as_deref(),
        },
        agent_id: file.agent_id.as_deref(),
        agent_type: some_nonempty(&file.meta.agent_type),
        description: some_nonempty(&file.meta.description),
        tool_use_id: some_nonempty(&file.meta.tool_use_id),
        // A transcript file is always spawn evidence; only an anchor row
        // qualifies its lifecycle. See [`TranscriptPayload::kind`].
        kind: None,
        records,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::transcript::files::SubagentMeta;

    fn session() -> TranscriptSession {
        TranscriptSession::new(
            crate::envelope::HARNESS_ID_CLAUDE,
            "0ea3c2cc-fe9d-41ff-aab1-4134ad00c350",
        )
        .with_harness_version(Some("2.1.161".to_owned()))
        .with_cwd(Some("/Users/me/src/repo".to_owned()))
    }

    fn main_file() -> TranscriptFile {
        TranscriptFile {
            path: "/tmp/x.jsonl".into(),
            agent_id: None,
            meta: SubagentMeta::default(),
        }
    }

    fn subagent_file() -> TranscriptFile {
        TranscriptFile {
            path: "/tmp/agent-abc.jsonl".into(),
            agent_id: Some("abc".to_owned()),
            meta: SubagentMeta {
                tool_use_id: "toolu_01".to_owned(),
                agent_type: "general-purpose".to_owned(),
                description: "dig".to_owned(),
            },
        }
    }

    /// Golden-pin the exact JSON the Go reference client would produce for the
    /// same inputs: empty-string identity fields present, agent fields absent,
    /// records verbatim.
    ///
    /// Carried over from paperd's `main_payload_matches_go_client_shape` — same
    /// expected bytes.
    #[test]
    fn main_payload_matches_go_client_shape() {
        let session = session();
        let file = main_file();
        let records = RawValue::from_string(r#"[{"b":1,"a":2}]"#.to_owned()).unwrap();
        let payload = build_payload(&session, &file, &records);
        let got = serde_json::to_string(&payload).unwrap();
        assert_eq!(
            got,
            r#"{"session":{"org_id":"","auth_subject":"","harness_id":"claude","harness_session_id":"0ea3c2cc-fe9d-41ff-aab1-4134ad00c350","harness_version":"2.1.161","cwd":"/Users/me/src/repo"},"records":[{"b":1,"a":2}]}"#,
        );
    }

    /// Carried over from paperd's `subagent_payload_carries_fork_metadata`.
    #[test]
    fn subagent_payload_carries_fork_metadata() {
        let session = session();
        let file = subagent_file();
        let records = RawValue::from_string("[]".to_owned()).unwrap();
        let payload = build_payload(&session, &file, &records);
        let got: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&payload).unwrap()).unwrap();
        assert_eq!(got["agent_id"], "abc");
        assert_eq!(got["agent_type"], "general-purpose");
        assert_eq!(got["description"], "dig");
        assert_eq!(got["tool_use_id"], "toolu_01");
    }

    /// Missing `meta.json` degrades to empty strings; Go's `omitempty` drops
    /// them and so must we.
    ///
    /// Carried over from paperd's `subagent_payload_omits_empty_meta_fields`.
    #[test]
    fn subagent_payload_omits_empty_meta_fields() {
        let session = session();
        let mut file = subagent_file();
        file.meta = SubagentMeta::default();
        let records = RawValue::from_string("[]".to_owned()).unwrap();
        let payload = build_payload(&session, &file, &records);
        let got = serde_json::to_string(&payload).unwrap();
        assert!(!got.contains("agent_type"), "got: {got}");
        assert!(!got.contains("tool_use_id"), "got: {got}");
        assert!(!got.contains("description"), "got: {got}");
        assert!(got.contains(r#""agent_id":"abc""#), "got: {got}");
    }

    /// A caller that *does* have identity to declare gets it on the wire — the
    /// standalone-client case paperd never exercised. The fields stay present
    /// either way, which is what the Go server's non-`omitempty` decode expects.
    #[test]
    fn identity_fields_are_always_present_and_carry_a_subject_when_set() {
        let session = TranscriptSession::new("claude", "sid").with_auth_subject("local:alice");
        let file = main_file();
        let records = RawValue::from_string("[]".to_owned()).unwrap();
        let got = serde_json::to_string(&build_payload(&session, &file, &records)).unwrap();
        assert!(
            got.contains(r#""auth_subject":"local:alice""#),
            "got: {got}"
        );
        assert!(got.contains(r#""org_id":"""#), "got: {got}");
        // Unknown optionals stay omitted rather than becoming null.
        assert!(!got.contains("harness_version"), "got: {got}");
        assert!(!got.contains("cwd"), "got: {got}");
    }

    /// An unset `kind` must not appear on the wire *at all*. This is the
    /// stability constraint the whole field is shaped around: spawn-evidence
    /// rows were ingested by builds that predate `kind`, and the server's raw
    /// dedup keys on the payload bytes, so a `"kind":null` — or any reordering
    /// that moved an existing field — would re-ingest every already-stored row
    /// as a new version.
    #[test]
    fn an_unset_kind_leaves_the_payload_bytes_unchanged() {
        let session = session();
        let file = subagent_file();
        let records = RawValue::from_string("[]".to_owned()).unwrap();
        let payload = build_payload(&session, &file, &records);
        assert!(payload.kind.is_none(), "build_payload never sets kind");
        let got = serde_json::to_string(&payload).unwrap();
        assert!(!got.contains("kind"), "got: {got}");
        assert_eq!(
            got,
            r#"{"session":{"org_id":"","auth_subject":"","harness_id":"claude","harness_session_id":"0ea3c2cc-fe9d-41ff-aab1-4134ad00c350","harness_version":"2.1.161","cwd":"/Users/me/src/repo"},"agent_id":"abc","agent_type":"general-purpose","description":"dig","tool_use_id":"toolu_01","records":[]}"#,
        );
    }

    /// A caller that *does* qualify the row — the Codex anchor lane — gets
    /// `kind` between `tool_use_id` and `records`. Field order is part of the
    /// contract, not an accident: it keeps a spawn row's bytes a strict prefix
    /// of the shape an interacted row extends.
    #[test]
    fn an_anchor_kind_serializes_after_tool_use_id() {
        let session = session();
        let file = subagent_file();
        let records = RawValue::from_string("[]".to_owned()).unwrap();
        let payload = TranscriptPayload {
            kind: Some(KIND_INTERACTED),
            ..build_payload(&session, &file, &records)
        };
        let got = serde_json::to_string(&payload).unwrap();
        assert!(
            got.contains(r#""tool_use_id":"toolu_01","kind":"interacted","records":[]"#),
            "got: {got}",
        );
    }

    /// The records bytes embed verbatim, including key order and interior
    /// spacing — the server's dedup hash is computed over exactly these bytes.
    #[test]
    fn records_embed_verbatim() {
        let session = session();
        let file = main_file();
        let raw = r#"[{"z":1,"a": 2},{"b":[3,  4]}]"#;
        let records = RawValue::from_string(raw.to_owned()).unwrap();
        let got = serde_json::to_string(&build_payload(&session, &file, &records)).unwrap();
        assert!(
            got.ends_with(&format!(r#","records":{raw}}}"#)),
            "got: {got}",
        );
    }
}
