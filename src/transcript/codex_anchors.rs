//! Codex sub-agent-activity anchors: the spawn edge that never reaches the
//! wire.
//!
//! Codex never puts the (spawn call_id ↔ child thread id) join on the wire:
//! `spawn_agent` tool arguments are an encrypted blob and the tool result names
//! only the task. The exact join exists solely in the PARENT rollout file as an
//! `event_msg` / `sub_agent_activity` record:
//!
//! ```json
//! {"timestamp":"…","type":"event_msg","payload":{
//!   "type":"sub_agent_activity","event_id":"call_…",
//!   "occurred_at_ms":…,"agent_thread_id":"<child thread id>",
//!   "agent_path":"/root/…","kind":"started"}}
//! ```
//!
//! tapes needs that join to stamp `ParentToolUseID` on the child thread's chain
//! root, which is what parents the child's `agent` span under the exact
//! `spawn_agent` tool span (otherwise root-keyed child turns flatten under the
//! trace root). The evidence ships the same way Claude's fork edge ships — as a
//! transcript-source raw row whose meta carries `{transcript: true, agent_id:
//! <child thread id>, tool_use_id: <spawn call_id>}` — so tapes' identity-first
//! `ReconcileTranscripts` join applies verbatim.
//!
//! Rollouts also carry `kind:"interacted"` records — one per directed re-entry
//! (`send_message` / `followup_task`), in the SENDER's rollout, with `event_id`
//! = the triggering function_call's call_id and `agent_thread_id` = the TARGET
//! thread (which may be the sender's parent or the root itself: upward/sideways
//! messaging is real). Those are anchors too, marked
//! [`crate::transcript::KIND_INTERACTED`] in payload/meta. They are NOT spawn
//! evidence — tapes' deriver ignores them, only `started` rows anchor threads —
//! but banking them means future re-entry rendering needs no rollout-file
//! backfill.
//!
//! # Why this is here rather than in a client
//!
//! Every line above is a statement about Codex's own on-disk format, and it was
//! true of exactly one client for as long as only paperd read rollouts. A
//! standalone capture of the same Codex session then reconstructed into a
//! *flatter* tree than the platform capture of it — the open-source path
//! observably capturing less than the commercial one for the same harness. The
//! derivation belongs wherever both clients can reach it, which is here.
//!
//! # Where it sits
//!
//! Under [`crate::transcript`] rather than [`crate::attribution::codex`]
//! because what it produces is a transcript-lane payload: it composes the
//! sibling modules here ([`super::files::fingerprint`],
//! [`super::files::jsonl_to_records`], [`super::payload::TranscriptPayload`]),
//! and attribution answers a different question — who sent *this request* —
//! with no dependency on the ingest lane. The harness-named module inside a
//! lane-named parent follows [`crate::attribution`]'s rule that harness
//! specifics carry their harness's name; if Codex ever grows a second
//! transcript-lane concern, this becomes `transcript/codex/anchors.rs`.
//!
//! # The anchor-row wire contract (shared with tapes)
//!
//! One `POST {base}/v1/ingest/transcript` per anchor, body:
//!
//! ```json
//! {
//!   "session": {
//!     "org_id": "",
//!     "auth_subject": "",
//!     "harness_id": "codex",
//!     "harness_session_id": "<ROOT session id>",
//!     "harness_version": "<parent rollout cli_version>",
//!     "cwd": "<parent rollout cwd>"
//!   },
//!   "agent_id": "<child thread id>",
//!   "agent_type": "<last agent_path segment>",
//!   "description": "<agent_path>",
//!   "tool_use_id": "<spawn call_id (event_id)>",
//!   "records": [ <the verbatim kind:"started" rollout line> ]
//! }
//! ```
//!
//! `harness_session_id` is always the ROOT session id
//! (`session_meta.session_id`, falling back to the rollout's own id for root
//! rollouts) — even when the spawning parent is itself a subagent — because
//! tapes groups reconcile inputs by session key and every re-keyed child turn
//! lives under the root. `records` holds the single rollout line verbatim,
//! which keeps the server-side content hash
//! (`transcript:<sid>:<agent>:<sha256[..8]>`) stable so re-pushes dedup instead
//! of appending versions.
//!
//! Interacted rows reuse the same body shape with `agent_id` = the TARGET
//! thread id, `tool_use_id` = the triggering call id, the verbatim
//! `kind:"interacted"` line as the single record, and one extra top-level
//! field: `"kind":"interacted"` (started rows omit it — absent means spawn
//! evidence, the legacy default).
//!
//! # What stays with each client
//!
//! The same split the rest of [`crate::transcript`] makes: **delivery, auth,
//! retry, and scope**. Which rollouts are *ours* is a per-client question — each
//! capture client declares its own Codex provider id and filters the watcher
//! snapshot with [`crate::attribution::CodexProviderFilter`] — and so are the
//! HTTP call, the credential, the tick cadence, and the failure backoff. What
//! [`CodexAnchorScanner`] owns is only the part that must not fork: which
//! anchors a rollout currently offers that have not been delivered yet, and
//! when a rollout is worth re-reading at all.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::attribution::CodexSessionFile;

use super::files::{self, FileFingerprint};
use super::payload::{IngestEnvelope, KIND_INTERACTED, TranscriptPayload};

/// The `sub_agent_activity` lifecycle kinds a rollout states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// A spawn: `agent_thread_id` is a freshly created child thread and
    /// `event_id` is the `spawn_agent` call that created it. The anchor tapes'
    /// deriver joins on.
    Started,
    /// A directed re-entry (`send_message` / `followup_task`):
    /// `agent_thread_id` is the TARGET thread — possibly the sender's parent or
    /// the root — and `event_id` is the triggering call. Carried for
    /// durability; inert in derivation.
    Interacted,
}

/// One `sub_agent_activity` record extracted from a rollout.
///
/// `line` is the rollout line **verbatim** — the ingest server content-hashes
/// the records array for idempotency, so the bytes must be stable across
/// pushes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAgentAnchor {
    /// Which lifecycle record this anchor carries.
    pub kind: AnchorKind,
    /// The subject thread (`payload.agent_thread_id`): the spawned child for
    /// `started`, the message TARGET for `interacted`.
    pub thread_id: String,
    /// The triggering call id (`payload.event_id`): the `spawn_agent` call for
    /// `started`, the `send_message`/`followup_task` call for `interacted`.
    pub call_id: String,
    /// Slash-separated task path (`payload.agent_path`), e.g.
    /// `/root/depth2_cli_child`.
    pub agent_path: Option<String>,
    /// The full rollout line, verbatim.
    pub line: String,
}

impl SubAgentAnchor {
    /// Label for the subagent kind slot on the upload: the last `agent_path`
    /// segment (the task name Codex assigned).
    #[must_use]
    pub fn agent_type(&self) -> Option<&str> {
        self.agent_path
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .filter(|segment| !segment.is_empty())
    }

    /// The payload/meta `kind` slot ([`TranscriptPayload::kind`]).
    ///
    /// Started rows omit it so their payload bytes — and the rows earlier
    /// builds already ingested — stay unchanged; absent means spawn evidence.
    /// The wire value is [`KIND_INTERACTED`], which matches the rollout
    /// record's own `kind` spelling by design.
    #[must_use]
    pub fn payload_kind(&self) -> Option<&'static str> {
        match self.kind {
            AnchorKind::Started => None,
            AnchorKind::Interacted => Some(KIND_INTERACTED),
        }
    }

    /// Once-per-anchor identity, used both to drop duplicate parses and to
    /// remember what a client already delivered.
    ///
    /// A thread starts exactly once, so started keys on the thread alone; a
    /// thread is interacted with many times, so interacted includes the
    /// triggering call id.
    #[must_use]
    pub fn dedup_key(&self) -> String {
        match self.kind {
            AnchorKind::Started => format!("started:{}", self.thread_id),
            AnchorKind::Interacted => format!("interacted:{}:{}", self.call_id, self.thread_id),
        }
    }
}

#[derive(Deserialize)]
struct RolloutRow {
    #[serde(rename = "type")]
    row_type: String,
    payload: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct SubAgentActivity {
    #[serde(rename = "type")]
    activity_type: String,
    event_id: Option<String>,
    agent_thread_id: Option<String>,
    agent_path: Option<String>,
    kind: Option<String>,
}

/// The `payload.kind` spelling of a spawn record.
const ROLLOUT_KIND_STARTED: &str = "started";

/// Extract the sub-agent anchors (`kind == "started"` spawns and
/// `kind == "interacted"` re-entries) from raw rollout bytes.
///
/// Blank / malformed / non-matching lines are skipped, matching the transcript
/// reader's tolerance for a truncated final line — a rollout is append-only and
/// is read while the harness is still writing it.
///
/// Started anchors are one per child: a thread starts exactly once, so a
/// duplicate `started` record for an already-seen `agent_thread_id` is dropped
/// (first wins — the first start is the spawn edge). Interacted anchors are one
/// per triggering call: the same target thread legitimately appears once per
/// `send_message` / `followup_task`, so only an exact `(call_id, thread)` repeat
/// drops.
#[must_use]
pub fn parse_subagent_anchors(raw: &[u8]) -> Vec<SubAgentAnchor> {
    let mut out: Vec<SubAgentAnchor> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in raw.split(|&byte| byte == b'\n') {
        // Cheap reject before JSON-parsing multi-KB rollout rows.
        if !line
            .windows(b"sub_agent_activity".len())
            .any(|window| window == b"sub_agent_activity")
        {
            continue;
        }
        let Ok(text) = std::str::from_utf8(line) else {
            continue;
        };
        let text = text.trim();
        let Ok(row) = serde_json::from_str::<RolloutRow>(text) else {
            continue;
        };
        if row.row_type != "event_msg" {
            continue;
        }
        let Some(activity) = row
            .payload
            .and_then(|payload| serde_json::from_value::<SubAgentActivity>(payload).ok())
        else {
            continue;
        };
        if activity.activity_type != "sub_agent_activity" {
            continue;
        }
        let kind = match activity.kind.as_deref() {
            Some(ROLLOUT_KIND_STARTED) => AnchorKind::Started,
            Some(KIND_INTERACTED) => AnchorKind::Interacted,
            // Unknown lifecycle kinds are not evidence we understand; skip
            // rather than mislabel.
            _ => continue,
        };
        let (Some(call_id), Some(thread)) = (activity.event_id, activity.agent_thread_id) else {
            continue;
        };
        if call_id.is_empty() || thread.is_empty() {
            continue;
        }
        let anchor = SubAgentAnchor {
            kind,
            thread_id: thread,
            call_id,
            agent_path: activity.agent_path,
            line: text.to_owned(),
        };
        if !seen.insert(anchor.dedup_key()) {
            if anchor.kind == AnchorKind::Started {
                tracing::warn!(
                    child_thread_id = %anchor.thread_id,
                    call_id = %anchor.call_id,
                    "codex-anchors: duplicate started record for one thread; keeping the first",
                );
            }
            continue;
        }
        out.push(anchor);
    }
    out
}

/// Assemble the ingest payload for one anchor — see the module docs for the
/// exact wire contract.
///
/// The body is a [`TranscriptPayload`] constructed literally rather than
/// through [`super::payload::build_payload`] because an anchor is not a
/// transcript file: `agent_id` carries the anchor's subject thread,
/// `tool_use_id` the triggering call, and `kind` the lifecycle qualifier only
/// interacted rows set.
///
/// `harness_id` is a parameter rather than a constant because one rollout tree
/// serves two harnesses: a `codex` CLI session and a Codex desktop-app session
/// write the same records, and the row must name the same harness its own wire
/// traffic does or the deriver files the two under different sessions. Callers
/// pass [`crate::envelope::HARNESS_ID_CODEX`] or
/// [`crate::envelope::HARNESS_ID_CODEX_APP`].
///
/// `records` must be [`files::jsonl_to_records`] over `anchor.line`, wrapped in
/// a [`RawValue`] so the bytes embed verbatim.
#[must_use]
pub fn build_anchor_payload<'a>(
    rollout: &'a CodexSessionFile,
    anchor: &'a SubAgentAnchor,
    harness_id: &'a str,
    records: &'a RawValue,
) -> TranscriptPayload<'a> {
    TranscriptPayload {
        session: IngestEnvelope {
            org_id: "",
            auth_subject: "",
            harness_id,
            // Always the ROOT session id: a depth-1 launcher's rollout carries
            // the root in session_meta.session_id; a root rollout falls back to
            // its own id.
            harness_session_id: rollout
                .root_session_id
                .as_deref()
                .unwrap_or(&rollout.session_id),
            harness_version: rollout.cli_version.as_deref(),
            cwd: rollout.cwd.as_deref(),
        },
        agent_id: Some(&anchor.thread_id),
        agent_type: anchor.agent_type(),
        description: anchor.agent_path.as_deref(),
        tool_use_id: Some(&anchor.call_id),
        kind: anchor.payload_kind(),
        records,
    }
}

/// Convenience: the `records` array for one anchor, ready for
/// [`build_anchor_payload`].
///
/// `None` only if [`files::jsonl_to_records`] produced something that is not
/// valid JSON, which it does not — the wrapper exists so a caller does not have
/// to decide what that impossible case means twice.
#[must_use]
pub fn anchor_records(anchor: &SubAgentAnchor) -> Option<Box<RawValue>> {
    RawValue::from_string(files::jsonl_to_records(anchor.line.as_bytes())).ok()
}

/// Per-rollout scan bookkeeping.
#[derive(Debug, Default)]
struct RolloutScan {
    /// Fingerprint at the last scan that left nothing undelivered; a matching
    /// fingerprint skips the file read entirely.
    scanned: Option<FileFingerprint>,
    /// [`SubAgentAnchor::dedup_key`]s the client reported delivered.
    delivered: HashSet<String>,
}

/// What a client has already taken from each rollout it watches.
///
/// This is the shared half of the anchor lane's state: *which anchors are still
/// owed*, and *whether a rollout is worth re-reading*. Both answers are
/// statements about Codex's file format — rollouts are append-only, so a stable
/// size+mtime fingerprint means no new anchors, and each anchor's identity is
/// its [`SubAgentAnchor::dedup_key`] — and a client that re-derived them would
/// be re-deriving how much of a session's causal skeleton gets uploaded.
///
/// Everything around it stays with the client: the tick, the HTTP call, the
/// credential, the failure backoff, and the scope rule that decides which
/// rollouts are handed here at all.
///
/// A scanner is only ever as large as the live rollout set — call
/// [`Self::retain_live`] each tick with the current snapshot.
#[derive(Debug, Default)]
pub struct CodexAnchorScanner {
    states: HashMap<PathBuf, RolloutScan>,
}

impl CodexAnchorScanner {
    /// An empty scanner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop bookkeeping for every rollout not in `live`.
    ///
    /// Codex's rollout directory is retention-bounded and a watcher snapshot
    /// only reports recent files, so without this the map grows for the life of
    /// a long-running client.
    pub fn retain_live<'a, I>(&mut self, live: I)
    where
        I: IntoIterator<Item = &'a Path>,
    {
        let live: HashSet<&Path> = live.into_iter().collect();
        self.states.retain(|path, _| live.contains(path.as_path()));
    }

    /// Whether `rollout` is worth reading, given its current `fingerprint`.
    ///
    /// `false` when the file could not be fingerprinted (vanished, or `stat`
    /// failed — skip this tick) or when it has not moved since the last clean
    /// scan. Rollouts are append-only, so an unchanged fingerprint cannot hide
    /// a new anchor.
    #[must_use]
    pub fn needs_read(&self, rollout: &Path, fingerprint: Option<FileFingerprint>) -> bool {
        fingerprint.is_some() && self.states.get(rollout).and_then(|s| s.scanned) != fingerprint
    }

    /// The anchors in `raw` that have not been reported delivered for
    /// `rollout`, in file order.
    #[must_use]
    pub fn undelivered(&self, rollout: &Path, raw: &[u8]) -> Vec<SubAgentAnchor> {
        let state = self.states.get(rollout);
        parse_subagent_anchors(raw)
            .into_iter()
            .filter(|anchor| {
                state.is_none_or(|state| !state.delivered.contains(&anchor.dedup_key()))
            })
            .collect()
    }

    /// Record that the server accepted `anchor`'s row (including a dedup — the
    /// bytes are stored either way).
    pub fn record_delivered(&mut self, rollout: &Path, anchor: &SubAgentAnchor) {
        self.states
            .entry(rollout.to_path_buf())
            .or_default()
            .delivered
            .insert(anchor.dedup_key());
    }

    /// Record a scan that left nothing owed, so `rollout` is not re-read until
    /// it grows.
    ///
    /// A client must NOT call this after a partial failure: leaving the
    /// fingerprint behind is what makes the next tick re-read and retry.
    pub fn record_clean_scan(&mut self, rollout: &Path, fingerprint: Option<FileFingerprint>) {
        self.states
            .entry(rollout.to_path_buf())
            .or_default()
            .scanned = fingerprint;
    }

    /// How many anchors have been reported delivered for `rollout`.
    #[must_use]
    pub fn delivered_count(&self, rollout: &Path) -> usize {
        self.states
            .get(rollout)
            .map_or(0, |state| state.delivered.len())
    }
}

/// The rollout lines and payload bytes both capture clients assert against.
///
/// Not feature-gated, unlike [`crate::envelope_fixtures`]: these are inert
/// `&'static str`s with no I/O and no panics, and gating them would put a Cargo
/// feature between two repositories and the one artifact that proves their
/// anchor rows are byte-identical. The corpus is small on purpose — it pins the
/// contract, not the implementation.
pub mod fixtures {
    /// The exact `sub_agent_activity` spawn line captured in the 2026-07-23
    /// codex_skills clearing (root `019f8d46-beb1`, line 18) — the real-world
    /// shape the parser exists for.
    pub const STARTED_LINE: &str = r#"{"timestamp":"2026-07-23T04:41:01.858Z","type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_J7B6r7ZdtqkECtSJV8YDQaL7","occurred_at_ms":1784781661858,"agent_thread_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_path":"/root/depth2_cli_child","kind":"started"}}"#;

    /// The `kind:"interacted"` line from the same clearing (grandchild rollout
    /// `019f8d47-0473`, line 31): a `send_message` in the SENDER's rollout
    /// targeting the sender's PARENT thread. Upward messaging is the real-world
    /// shape these rows must survive.
    pub const INTERACTED_LINE: &str = r#"{"timestamp":"2026-07-23T04:41:18.008Z","type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_cqusEjhomv5zKjZ7vodiY7Og","occurred_at_ms":1784781678008,"agent_thread_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_path":"/root/depth2_cli_child","kind":"interacted"}}"#;

    /// Root session id of [`ROLLOUT`], as [`SESSION_ID`]'s rollout reports it.
    pub const ROOT_SESSION_ID: &str = "019f8d46-beb1-7c40-9a1f-2e8b1c0d5a33";

    /// The rollout's own thread id. Distinct from [`ROOT_SESSION_ID`] on
    /// purpose: the parent doing the spawning is itself a subagent here, which
    /// is the case that proves anchor rows key to the ROOT.
    pub const SESSION_ID: &str = "019f8d46-c0de-7000-8000-000000000001";

    /// `cwd` of [`ROLLOUT`]'s session.
    pub const CWD: &str = "/w/repo";

    /// Codex CLI version of [`ROLLOUT`]'s session.
    pub const CLI_VERSION: &str = "0.145.0";

    /// A rollout carrying one spawn and one re-entry, plus rows an anchor
    /// scanner must ignore: a `session_meta` header, an unknown lifecycle kind,
    /// and an `agent_message` that merely mentions the marker in its text.
    pub const ROLLOUT: &str = concat!(
        r#"{"timestamp":"2026-07-23T04:41:00.000Z","type":"session_meta","payload":{"id":"019f8d46-c0de-7000-8000-000000000001"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-23T04:41:01.858Z","type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_J7B6r7ZdtqkECtSJV8YDQaL7","occurred_at_ms":1784781661858,"agent_thread_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_path":"/root/depth2_cli_child","kind":"started"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-23T04:41:10.000Z","type":"event_msg","payload":{"type":"agent_message","message":"spawned via sub_agent_activity"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-23T04:41:18.008Z","type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_cqusEjhomv5zKjZ7vodiY7Og","occurred_at_ms":1784781678008,"agent_thread_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_path":"/root/depth2_cli_child","kind":"interacted"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-23T04:41:20.000Z","type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_J7B6r7ZdtqkECtSJV8YDQaL7","agent_thread_id":"019f8d46-e663-74e1-940c-f82e34c07618","kind":"finished"}}"#,
        "\n",
    );

    /// The exact ingest body a `codex` capture must POST for [`ROLLOUT`]'s
    /// spawn record.
    ///
    /// Two independently written capture clients agreeing on these bytes is the
    /// whole claim: same session key, same anchor identity, same records, same
    /// field order. Any client whose lane differs — a re-serialized records
    /// array, an omitted `agent_type`, the rollout's own thread id in place of
    /// the root — fails against this constant rather than at derivation time in
    /// production.
    pub const STARTED_BODY: &str = concat!(
        r#"{"session":{"org_id":"","auth_subject":"","harness_id":"codex","#,
        r#""harness_session_id":"019f8d46-beb1-7c40-9a1f-2e8b1c0d5a33","harness_version":"0.145.0","cwd":"/w/repo"},"#,
        r#""agent_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_type":"depth2_cli_child","#,
        r#""description":"/root/depth2_cli_child","tool_use_id":"call_J7B6r7ZdtqkECtSJV8YDQaL7","#,
        r#""records":[{"timestamp":"2026-07-23T04:41:01.858Z","type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_J7B6r7ZdtqkECtSJV8YDQaL7","occurred_at_ms":1784781661858,"agent_thread_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_path":"/root/depth2_cli_child","kind":"started"}}]}"#,
    );

    /// The exact ingest body a `codex` capture must POST for [`ROLLOUT`]'s
    /// re-entry record — the same shape plus the `kind` marker, which sits
    /// after `tool_use_id` so a spawn row's bytes stay a strict prefix.
    pub const INTERACTED_BODY: &str = concat!(
        r#"{"session":{"org_id":"","auth_subject":"","harness_id":"codex","#,
        r#""harness_session_id":"019f8d46-beb1-7c40-9a1f-2e8b1c0d5a33","harness_version":"0.145.0","cwd":"/w/repo"},"#,
        r#""agent_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_type":"depth2_cli_child","#,
        r#""description":"/root/depth2_cli_child","tool_use_id":"call_cqusEjhomv5zKjZ7vodiY7Og","kind":"interacted","#,
        r#""records":[{"timestamp":"2026-07-23T04:41:18.008Z","type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_cqusEjhomv5zKjZ7vodiY7Og","occurred_at_ms":1784781678008,"agent_thread_id":"019f8d46-e663-74e1-940c-f82e34c07618","agent_path":"/root/depth2_cli_child","kind":"interacted"}}]}"#,
    );

    /// Both bodies in the order [`ROLLOUT`] states them, which is the order a
    /// client's pushes must arrive in.
    pub const BODIES: [&str; 2] = [STARTED_BODY, INTERACTED_BODY];

    /// The session facts of [`ROLLOUT`], as a rollout at `path` would be
    /// reported by the Codex watcher.
    ///
    /// `model_provider` is left `None`: whether a rollout is *ours* is the
    /// consumer's scope rule, and each client stamps a provider id of its own.
    #[must_use]
    pub fn session_file(path: std::path::PathBuf) -> crate::attribution::CodexSessionFile {
        crate::attribution::CodexSessionFile {
            session_id: SESSION_ID.to_owned(),
            root_session_id: Some(ROOT_SESSION_ID.to_owned()),
            parent_thread_id: Some(ROOT_SESSION_ID.to_owned()),
            subagent_kind: None,
            timestamp: time::OffsetDateTime::UNIX_EPOCH,
            modified_at: Some(time::OffsetDateTime::UNIX_EPOCH),
            cwd: Some(CWD.to_owned()),
            originator: Some("codex_exec".to_owned()),
            cli_version: Some(CLI_VERSION.to_owned()),
            source: Some("exec".to_owned()),
            thread_source: Some("subagent".to_owned()),
            model_provider: None,
            path,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::fixtures::{INTERACTED_LINE, STARTED_LINE};
    use super::*;
    use crate::envelope::HARNESS_ID_CODEX;

    fn rollout_at(path: &Path) -> CodexSessionFile {
        fixtures::session_file(path.to_path_buf())
    }

    fn body(harness_id: &str, rollout: &CodexSessionFile, anchor: &SubAgentAnchor) -> String {
        let records = anchor_records(anchor).unwrap();
        serde_json::to_string(&build_anchor_payload(rollout, anchor, harness_id, &records)).unwrap()
    }

    // --- the derivation ---------------------------------------------------

    #[test]
    fn parse_extracts_started_and_interacted_records_verbatim() {
        let anchors = parse_subagent_anchors(fixtures::ROLLOUT.as_bytes());
        assert_eq!(anchors.len(), 2, "the ignorable rows must stay ignored");

        let started = &anchors[0];
        assert_eq!(started.kind, AnchorKind::Started);
        assert_eq!(started.thread_id, "019f8d46-e663-74e1-940c-f82e34c07618");
        assert_eq!(started.call_id, "call_J7B6r7ZdtqkECtSJV8YDQaL7");
        assert_eq!(
            started.agent_path.as_deref(),
            Some("/root/depth2_cli_child")
        );
        assert_eq!(started.agent_type(), Some("depth2_cli_child"));
        assert_eq!(
            started.line, STARTED_LINE,
            "the rollout line must survive verbatim — the server dedups on its hash",
        );

        let interacted = &anchors[1];
        assert_eq!(interacted.kind, AnchorKind::Interacted);
        assert_eq!(
            interacted.thread_id, "019f8d46-e663-74e1-940c-f82e34c07618",
            "interacted rows carry the TARGET thread",
        );
        assert_eq!(interacted.call_id, "call_cqusEjhomv5zKjZ7vodiY7Og");
        assert_eq!(interacted.line, INTERACTED_LINE);
    }

    #[test]
    fn parse_tolerates_a_truncated_final_line() {
        // The file is read while the harness is still appending to it.
        let raw = format!("{STARTED_LINE}\n{{\"type\":\"event_msg\",\"payl");
        assert_eq!(parse_subagent_anchors(raw.as_bytes()).len(), 1);
    }

    #[test]
    fn parse_keeps_first_started_record_per_child() {
        let dup = STARTED_LINE.replace("call_J7B6r7ZdtqkECtSJV8YDQaL7", "call_second");
        let raw = format!("{STARTED_LINE}\n{dup}\n");
        let anchors = parse_subagent_anchors(raw.as_bytes());
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].call_id, "call_J7B6r7ZdtqkECtSJV8YDQaL7");
    }

    #[test]
    fn parse_keeps_one_interacted_anchor_per_triggering_call() {
        // Two sends to the SAME target are two anchors (distinct call_ids); an
        // exact byte-repeat of one record is not.
        let second_send = INTERACTED_LINE.replace("call_cqusEjhomv5zKjZ7vodiY7Og", "call_2nd");
        let raw = format!("{INTERACTED_LINE}\n{second_send}\n{INTERACTED_LINE}\n");
        let anchors = parse_subagent_anchors(raw.as_bytes());
        assert_eq!(anchors.len(), 2);
        assert_eq!(anchors[0].call_id, "call_cqusEjhomv5zKjZ7vodiY7Og");
        assert_eq!(anchors[1].call_id, "call_2nd");

        // A started record and an interacted record for the SAME thread never
        // collide — kind is part of the anchor identity.
        let raw = format!("{STARTED_LINE}\n{INTERACTED_LINE}\n");
        assert_eq!(parse_subagent_anchors(raw.as_bytes()).len(), 2);
    }

    #[test]
    fn parse_requires_call_and_thread_ids() {
        let missing_thread = r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_x","kind":"started"}}"#;
        let missing_call = r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_thread_id":"child","kind":"started"}}"#;
        let missing_thread_interacted = r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_x","kind":"interacted"}}"#;
        let missing_call_interacted = r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_thread_id":"child","kind":"interacted"}}"#;
        let raw = format!(
            "{missing_thread}\n{missing_call}\n{missing_thread_interacted}\n{missing_call_interacted}\n"
        );
        assert!(parse_subagent_anchors(raw.as_bytes()).is_empty());
    }

    // --- the payload ------------------------------------------------------

    /// The fixture bodies are the cross-repo contract: if this fails, every
    /// capture client's parity test fails with it, which is the point.
    #[test]
    fn the_fixture_bodies_are_what_the_derivation_produces() {
        let rollout = rollout_at(Path::new("/tmp/rollout.jsonl"));
        let anchors = parse_subagent_anchors(fixtures::ROLLOUT.as_bytes());
        let bodies: Vec<String> = anchors
            .iter()
            .map(|anchor| body(HARNESS_ID_CODEX, &rollout, anchor))
            .collect();
        assert_eq!(bodies, fixtures::BODIES.to_vec());
    }

    #[test]
    fn anchor_rows_key_to_the_root_session_not_the_spawning_thread() {
        // The fixture's parent is itself a subagent. Its anchor rows must still
        // key to the ROOT session, because tapes groups reconcile inputs by
        // session key and every re-keyed child turn lives under the root.
        let rollout = rollout_at(Path::new("/tmp/launcher.jsonl"));
        assert_ne!(rollout.session_id, fixtures::ROOT_SESSION_ID);
        let anchor = &parse_subagent_anchors(fixtures::ROLLOUT.as_bytes())[0];
        let got: serde_json::Value =
            serde_json::from_str(&body(HARNESS_ID_CODEX, &rollout, anchor)).unwrap();
        assert_eq!(
            got["session"]["harness_session_id"],
            fixtures::ROOT_SESSION_ID
        );

        // A root rollout names no root of its own and falls back to its own id.
        let mut root = rollout;
        root.root_session_id = None;
        let got: serde_json::Value =
            serde_json::from_str(&body(HARNESS_ID_CODEX, &root, anchor)).unwrap();
        assert_eq!(got["session"]["harness_session_id"], fixtures::SESSION_ID);
    }

    #[test]
    fn a_started_row_carries_no_kind_field_at_all() {
        // Spawn rows were ingested by builds that predate `kind`, and the
        // server's raw dedup keys on the payload bytes: a `"kind":null` would
        // re-ingest every stored row as a new version.
        let rollout = rollout_at(Path::new("/tmp/rollout.jsonl"));
        let anchors = parse_subagent_anchors(fixtures::ROLLOUT.as_bytes());
        // The records array quotes the rollout's own `"kind":"started"`, so the
        // claim is about the top-level object, not the bytes.
        let started: serde_json::Value =
            serde_json::from_str(&body(HARNESS_ID_CODEX, &rollout, &anchors[0])).unwrap();
        assert!(started.get("kind").is_none());
        let interacted: serde_json::Value =
            serde_json::from_str(&body(HARNESS_ID_CODEX, &rollout, &anchors[1])).unwrap();
        assert_eq!(interacted["kind"], KIND_INTERACTED);
    }

    #[test]
    fn the_row_names_the_harness_the_caller_declares() {
        // One rollout tree, two harnesses: the CLI and the desktop app write
        // identical records, and the row must name the harness its own wire
        // traffic does.
        let rollout = rollout_at(Path::new("/tmp/rollout.jsonl"));
        let anchor = &parse_subagent_anchors(fixtures::ROLLOUT.as_bytes())[0];
        let got = body(crate::envelope::HARNESS_ID_CODEX_APP, &rollout, anchor);
        assert!(got.contains(r#""harness_id":"codex-app""#), "got: {got}");
    }

    #[test]
    fn an_anchor_with_no_agent_path_omits_the_optional_slots() {
        let rollout = rollout_at(Path::new("/tmp/rollout.jsonl"));
        let anchor = SubAgentAnchor {
            kind: AnchorKind::Started,
            thread_id: "child".to_owned(),
            call_id: "call_x".to_owned(),
            agent_path: None,
            line: "{}".to_owned(),
        };
        let got = body(HARNESS_ID_CODEX, &rollout, &anchor);
        assert!(!got.contains("agent_type"), "got: {got}");
        assert!(!got.contains("description"), "got: {got}");
        assert!(got.contains(r#""agent_id":"child""#), "got: {got}");
    }

    // --- the scanner ------------------------------------------------------

    fn write_rollout(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("rollout.jsonl");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_fresh_rollout_offers_every_anchor_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_rollout(dir.path(), fixtures::ROLLOUT);
        let mut scanner = CodexAnchorScanner::new();

        let fingerprint = files::fingerprint(&path);
        assert!(scanner.needs_read(&path, fingerprint));
        let raw = std::fs::read(&path).unwrap();
        let anchors = scanner.undelivered(&path, &raw);
        assert_eq!(anchors.len(), 2);

        for anchor in &anchors {
            scanner.record_delivered(&path, anchor);
        }
        scanner.record_clean_scan(&path, fingerprint);

        assert_eq!(scanner.delivered_count(&path), 2);
        assert!(
            !scanner.needs_read(&path, files::fingerprint(&path)),
            "an unchanged append-only file cannot hide a new anchor",
        );
        assert!(scanner.undelivered(&path, &raw).is_empty());
    }

    #[test]
    fn a_grown_rollout_offers_only_what_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_rollout(dir.path(), fixtures::ROLLOUT);
        let mut scanner = CodexAnchorScanner::new();

        let raw = std::fs::read(&path).unwrap();
        for anchor in scanner.undelivered(&path, &raw) {
            scanner.record_delivered(&path, &anchor);
        }
        scanner.record_clean_scan(&path, files::fingerprint(&path));

        // A second spawn, appended later in the same session.
        let second = STARTED_LINE
            .replace("call_J7B6r7ZdtqkECtSJV8YDQaL7", "call_second")
            .replace(
                "019f8d46-e663-74e1-940c-f82e34c07618",
                "019f8d47-0473-7743-a1ed-9e4c0ae92ad8",
            );
        std::fs::write(&path, format!("{}{second}\n", fixtures::ROLLOUT)).unwrap();

        assert!(scanner.needs_read(&path, files::fingerprint(&path)));
        let raw = std::fs::read(&path).unwrap();
        let pending = scanner.undelivered(&path, &raw);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].call_id, "call_second");
    }

    #[test]
    fn an_undelivered_anchor_is_offered_again_next_read() {
        // The client's failure path: nothing was recorded, so the next read
        // must still owe the same rows.
        let dir = tempfile::tempdir().unwrap();
        let path = write_rollout(dir.path(), fixtures::ROLLOUT);
        let scanner = CodexAnchorScanner::new();
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(scanner.undelivered(&path, &raw).len(), 2);
        assert_eq!(scanner.undelivered(&path, &raw).len(), 2);
        assert!(
            scanner.needs_read(&path, files::fingerprint(&path)),
            "a scan that was never marked clean must re-run",
        );
    }

    #[test]
    fn a_vanished_rollout_is_skipped_rather_than_read() {
        let scanner = CodexAnchorScanner::new();
        assert!(!scanner.needs_read(Path::new("/nonexistent/rollout.jsonl"), None));
    }

    #[test]
    fn retain_live_drops_rollouts_that_aged_out_of_the_snapshot() {
        let mut scanner = CodexAnchorScanner::new();
        let kept = PathBuf::from("/tmp/kept.jsonl");
        let gone = PathBuf::from("/tmp/gone.jsonl");
        let anchor = &parse_subagent_anchors(fixtures::ROLLOUT.as_bytes())[0];
        scanner.record_delivered(&kept, anchor);
        scanner.record_delivered(&gone, anchor);

        scanner.retain_live([kept.as_path()]);

        assert_eq!(scanner.delivered_count(&kept), 1);
        assert_eq!(scanner.delivered_count(&gone), 0);
    }
}
