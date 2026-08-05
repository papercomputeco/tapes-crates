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
/// [`crate::envelope::HARNESS_THREAD_ID_RULES`], to answer a different
/// question: not *which rollout* a request belongs to, but whether it was made
/// from a sub-thread. That reading needs the pair, because on a root turn the
/// two are equal. The spellings come from there so the two cannot drift apart.
pub const CODEX_ROLLOUT_ID_HEADERS: &[&str] = &[
    crate::envelope::CODEX_THREAD_ID_HEADER,
    crate::envelope::CODEX_SESSION_ID_HEADER,
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
        return Some(CodexSessionFile {
            session_id: payload.id,
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
