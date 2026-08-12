//! Verbatim session metadata from Codex JSONL transcripts.
//!
//! Codex writes transcript files under
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<session-id>.jsonl`
//! (or `$CODEX_HOME/sessions/...`). The first JSONL row is
//! `type=session_meta` and carries the stable Codex session id plus
//! launch metadata. Unlike Claude's `~/.claude/sessions/<pid>.json`,
//! this file is not PID-indexed, so callers must use a conservative
//! disambiguation policy before attaching it to traffic.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use http::HeaderMap;
use serde::Deserialize;
use time::OffsetDateTime;
use tracing::warn;

/// Request headers naming the rollout a Codex request belongs to, in priority
/// order — first present wins.
///
/// Codex stamps both on every inference request. On a main-thread turn they
/// are equal. On a turn made by a spawned subagent they are **not**:
/// `thread-id` is the child thread's own rollout id, while `session-id` stays
/// pinned to the root session (the child additionally carries
/// `x-codex-parent-thread-id` equal to that root). The order therefore matters
/// — reading `session-id` first would attribute every subagent turn to the
/// parent, which is precisely the misattribution this list exists to prevent.
///
/// This is harness knowledge, so it lives here rather than in each capture
/// client. Both names are unprefixed and so are in principle claimable by
/// another harness; that is harmless here because the value is only ever used
/// as an *exact* match against a live rollout's own session id, and a non-match
/// refuses rather than guesses.
///
/// The same two headers are read a second way, by
/// [`tapes_capture::envelope::HARNESS_THREAD_ID_RULES`], to answer a different
/// question: not *which rollout* a request belongs to, but whether it was made
/// from a sub-thread. That reading needs the pair, because on a root turn the
/// two are equal. The spellings come from there so the two cannot drift apart.
pub const CODEX_ROLLOUT_ID_HEADERS: &[&str] = &[
    tapes_capture::envelope::CODEX_THREAD_ID_HEADER,
    tapes_capture::envelope::CODEX_SESSION_ID_HEADER,
];

/// Resolve the id of the rollout a Codex request belongs to.
///
/// Returns `None` for a request carrying neither header — an older Codex, or a
/// different client speaking the same protocol. Callers must treat that as
/// "no evidence", not as "no match": see
/// [`crate::attribution::RequestFacts::codex_rollout_id`].
#[must_use]
pub fn rollout_id(headers: &HeaderMap) -> Option<&str> {
    CODEX_ROLLOUT_ID_HEADERS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSessionFile {
    pub session_id: String,
    /// Root Codex session that owns this thread, when the rollout names one.
    ///
    /// A subagent rollout records the ROOT session in
    /// `session_meta.payload.session_id` while its own thread id stays in
    /// `payload.id` (which is what [`Self::session_id`] carries). Every
    /// descendant retains the same root even when its direct parent is another
    /// subagent, so this is the id to key a captured session on. A root rollout
    /// names no root of its own and leaves this `None`.
    pub root_session_id: Option<String>,
    /// Direct parent thread reported by Codex for a subagent transcript.
    ///
    /// One hop, unlike [`Self::root_session_id`]: for a depth-2 subagent this
    /// is the depth-1 thread, not the root.
    pub parent_thread_id: Option<String>,
    /// What kind of subagent this rollout is, recovered from
    /// `payload.source.subagent`. `None` for a root rollout, and also for a
    /// subagent whose source names no kind in a shape we recognise.
    pub subagent_kind: Option<String>,
    pub timestamp: OffsetDateTime,
    pub modified_at: Option<OffsetDateTime>,
    pub cwd: Option<String>,
    pub originator: Option<String>,
    pub cli_version: Option<String>,
    pub source: Option<String>,
    pub thread_source: Option<String>,
    pub model_provider: Option<String>,
    pub path: PathBuf,
}

impl CodexSessionFile {
    /// Does this session name exactly `provider` as its model provider?
    ///
    /// Whether a given provider id is *ours* is a per-consumer question — see
    /// [`crate::attribution::CodexProviderFilter`], which the attribution
    /// pipeline uses for that decision. This method only answers the literal
    /// equality the marker-matching path needs.
    #[must_use]
    pub fn has_model_provider(&self, provider: &str) -> bool {
        self.model_provider.as_deref() == Some(provider)
    }
}

#[derive(Deserialize)]
struct JsonlRow {
    #[serde(rename = "type")]
    row_type: String,
    payload: Option<SessionMetaPayload>,
}

#[derive(Deserialize)]
struct SessionMetaPayload {
    id: String,
    /// The ROOT session id, not this thread's — `id` above is the thread's own
    /// and is what becomes `session_id`. Named by Codex, not by us.
    session_id: Option<String>,
    parent_thread_id: Option<String>,
    timestamp: String,
    cwd: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    source: Option<serde_json::Value>,
    thread_source: Option<String>,
    model_provider: Option<String>,
}

/// Read the first `session_meta` row from a Codex JSONL transcript.
pub fn read(path: &Path) -> Option<CodexSessionFile> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let row = match serde_json::from_str::<JsonlRow>(&line) {
            Ok(row) => row,
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "codex-session: could not parse jsonl row",
                );
                continue;
            }
        };
        if row.row_type != "session_meta" {
            continue;
        }
        let payload = row.payload?;
        let timestamp = match OffsetDateTime::parse(
            &payload.timestamp,
            &time::format_description::well_known::Rfc3339,
        ) {
            Ok(ts) => ts,
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "codex-session: could not parse session timestamp",
                );
                return None;
            }
        };
        // Read before `source` is consumed into its string form below: the
        // kind is recovered from the structured value, not from the flattened
        // text the field ends up holding.
        let subagent_kind = payload.source.as_ref().and_then(subagent_kind_from_source);
        return Some(CodexSessionFile {
            session_id: payload.id,
            root_session_id: payload.session_id,
            parent_thread_id: payload.parent_thread_id,
            subagent_kind,
            timestamp,
            modified_at: modified_at(path),
            cwd: payload.cwd,
            originator: payload.originator,
            cli_version: payload.cli_version,
            source: payload.source.and_then(metadata_value_to_string),
            thread_source: payload.thread_source,
            model_provider: payload.model_provider,
            path: path.to_path_buf(),
        });
    }
    None
}

/// Recover the subagent kind from a `session_meta` payload's `source` value.
///
/// Codex has spelled this three ways, and a rollout written by any Codex build
/// still on disk may use any of them, so all three are probed:
///
/// * `source.subagent` as a bare string — the kind itself;
/// * `source.subagent.thread_spawn` as an object — a spawn whose kind is the
///   spawn mechanism, reported as `"thread_spawn"`;
/// * otherwise the first of `other` / `agent_type` / `agent_role` / `type` that
///   holds a string.
///
/// That last probe order is carried over **as observed** from the capture-side
/// implementation this was lifted from, not designed here. It matters only when
/// one `subagent` object carries more than one of those keys with different
/// values, which no captured rollout has been seen to do; if Codex ever emits
/// such a shape, the order is the open question to settle rather than a rule to
/// preserve.
fn subagent_kind_from_source(source: &serde_json::Value) -> Option<String> {
    let subagent = source.get("subagent")?;
    if let Some(kind) = subagent.as_str() {
        return Some(kind.to_owned());
    }
    if subagent
        .get("thread_spawn")
        .is_some_and(serde_json::Value::is_object)
    {
        return Some("thread_spawn".to_owned());
    }
    ["other", "agent_type", "agent_role", "type"]
        .into_iter()
        .find_map(|key| subagent.get(key).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

fn modified_at(path: &Path) -> Option<OffsetDateTime> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(system_time_to_offset(modified))
}

fn system_time_to_offset(t: SystemTime) -> OffsetDateTime {
    t.into()
}

/// Default Codex session directory. `$CODEX_HOME` wins when set, matching
/// Codex's own home-directory override; otherwise use `~/.codex/sessions`.
#[must_use]
pub fn default_sessions_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join("sessions"));
    }
    dirs::home_dir().map(|h| h.join(".codex").join("sessions"))
}

fn metadata_value_to_string(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(value) => Some(value),
        value => serde_json::to_string(&value).ok(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The rollout-id lookup and the envelope's sub-thread pair rule read the
    /// same two headers to answer different questions — which rollout a
    /// request belongs to, versus whether it came from a sub-thread — so they
    /// must not drift apart. The assertion lives on this side because this is
    /// the side that may name a harness: the envelope cannot reach back into
    /// an attribution lane to check.
    #[test]
    fn the_rollout_id_headers_are_the_envelope_pair_rule_headers() {
        use tapes_capture::envelope::{HARNESS_THREAD_ID_RULES, HarnessThreadRule};

        let pair = HARNESS_THREAD_ID_RULES
            .iter()
            .find_map(|rule| match rule {
                HarnessThreadRule::DivergentPair { thread, session } => Some((*thread, *session)),
                // `HarnessThreadRule` is `#[non_exhaustive]`, so a rule shape
                // added on the envelope side does not break this build — it
                // simply is not the pair rule this assertion is about.
                _ => None,
            })
            .expect("codex is declared as a divergent pair");
        assert_eq!(
            [pair.0, pair.1],
            [CODEX_ROLLOUT_ID_HEADERS[0], CODEX_ROLLOUT_ID_HEADERS[1]],
        );
    }

    #[test]
    fn read_parses_session_meta_first_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-test.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-06-15T23:11:58.261Z","type":"session_meta","payload":{"id":"019ecd8e-4281-7353-8a00-09df678443b1","timestamp":"2026-06-15T23:11:52.984Z","cwd":"/tmp/work","originator":"codex-tui","cli_version":"0.139.0","source":"cli","thread_source":"user","model_provider":"paper-openai"}}"#,
        )
        .unwrap();

        let got = read(&path).unwrap();
        assert_eq!(got.session_id, "019ecd8e-4281-7353-8a00-09df678443b1");
        assert_eq!(got.cwd.as_deref(), Some("/tmp/work"));
        assert_eq!(got.cli_version.as_deref(), Some("0.139.0"));
        assert!(got.has_model_provider("paper-openai"));
    }

    #[test]
    fn read_accepts_structured_source_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-test.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-06-15T23:11:58.261Z","type":"session_meta","payload":{"id":"019ecd8e-4281-7353-8a00-09df678443b1","timestamp":"2026-06-15T23:11:52.984Z","cwd":"/tmp/work","source":{"subagent":{"agent_nickname":"Kant"}},"thread_source":"subagent","model_provider":"paper-openai"}}"#,
        )
        .unwrap();

        let got = read(&path).unwrap();
        assert_eq!(
            got.source.as_deref(),
            Some(r#"{"subagent":{"agent_nickname":"Kant"}}"#)
        );
    }

    /// A subagent rollout names its root and its direct parent, and the kind
    /// comes off the structured `source` — all three from the one read, not a
    /// second pass over the file.
    #[test]
    fn read_recovers_subagent_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-test.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-06-15T23:11:58.261Z","type":"session_meta","payload":{"id":"019ecd8e-4281-7353-8a00-09df678443b1","session_id":"root-session","parent_thread_id":"parent-thread","timestamp":"2026-06-15T23:11:52.984Z","cwd":"/tmp/work","source":{"subagent":{"other":"guardian","agent_nickname":"Kant"}},"thread_source":"subagent","model_provider":"paper-openai"}}"#,
        )
        .unwrap();

        let got = read(&path).unwrap();

        // `session_id` stays the thread's own id, from `payload.id`...
        assert_eq!(got.session_id, "019ecd8e-4281-7353-8a00-09df678443b1");
        assert_eq!(got.cwd.as_deref(), Some("/tmp/work"));
        // ...and the lineage fields come from the same row.
        assert_eq!(got.root_session_id.as_deref(), Some("root-session"));
        assert_eq!(got.parent_thread_id.as_deref(), Some("parent-thread"));
        assert_eq!(got.subagent_kind.as_deref(), Some("guardian"));
        // The flattened `source` text is unaffected by the kind probe.
        assert_eq!(
            got.source.as_deref(),
            Some(r#"{"subagent":{"agent_nickname":"Kant","other":"guardian"}}"#)
        );
    }

    /// The nested spawn shape: the kind is the spawn mechanism itself.
    #[test]
    fn read_accepts_thread_spawn_source_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-test.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-07-22T23:11:58.261Z","type":"session_meta","payload":{"id":"child-thread","session_id":"root-session","parent_thread_id":"parent-thread","timestamp":"2026-07-22T23:11:52.984Z","cwd":"/tmp/work","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-thread","depth":2,"agent_path":"/root/child/grandchild","agent_nickname":"Euler","agent_role":null}}},"thread_source":"subagent","model_provider":"paper-openai"}}"#,
        )
        .unwrap();

        let got = read(&path).unwrap();

        assert_eq!(got.subagent_kind.as_deref(), Some("thread_spawn"));
        assert_eq!(got.root_session_id.as_deref(), Some("root-session"));
        assert_eq!(got.parent_thread_id.as_deref(), Some("parent-thread"));
    }

    /// A root rollout carries no lineage at all. This is the common case and
    /// the one that must not invent a self-referential root: `session_id` is
    /// the thread's own id, and `root_session_id` stays `None` so a consumer
    /// can tell "I am the root" from "my root is elsewhere".
    #[test]
    fn read_leaves_lineage_empty_for_root_rollouts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-root.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-06-15T23:11:58.261Z","type":"session_meta","payload":{"id":"root-thread","timestamp":"2026-06-15T23:11:52.984Z","cwd":"/tmp/work","thread_source":"user","model_provider":"paper-openai"}}"#,
        )
        .unwrap();

        let got = read(&path).unwrap();

        assert_eq!(got.session_id, "root-thread");
        assert!(got.root_session_id.is_none());
        assert!(got.parent_thread_id.is_none());
        assert!(got.subagent_kind.is_none());
    }

    /// A `source` that is a bare string (`"cli"`, the ordinary root spelling)
    /// has no `subagent` key to probe, and a subagent object naming none of the
    /// recognised keys yields nothing rather than guessing.
    #[test]
    fn subagent_kind_is_absent_when_the_source_names_none() {
        assert_eq!(
            subagent_kind_from_source(&serde_json::json!("cli")),
            None,
            "a bare-string source has no subagent object"
        );
        assert_eq!(
            subagent_kind_from_source(&serde_json::json!({"subagent": {"agent_nickname": "Kant"}})),
            None,
            "no recognised kind key means no kind, not a guess",
        );
        assert_eq!(
            subagent_kind_from_source(&serde_json::json!({"subagent": "guardian"})),
            Some("guardian".to_owned()),
            "a bare-string subagent is the kind itself",
        );
    }

    #[test]
    fn read_skips_malformed_rows_before_session_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-test.jsonl");
        std::fs::write(
            &path,
            r#"not json
{"timestamp":"2026-06-15T23:11:58.261Z","type":"session_meta","payload":{"id":"019ecd8e-4281-7353-8a00-09df678443b1","timestamp":"2026-06-15T23:11:52.984Z","cwd":"/tmp/work","model_provider":"paper-openai"}}"#,
        )
        .unwrap();

        let got = read(&path).unwrap();
        assert_eq!(got.session_id, "019ecd8e-4281-7353-8a00-09df678443b1");
    }
}
