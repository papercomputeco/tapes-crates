//! Verbatim shape of `~/.claude/sessions/<pid>.json`.
//!
//! The Claude harness writes one of these files per active `claude`
//! process. A capture client reads them to map a peer PID (from
//! [`tapes_capture::peer_pid::lookup`]) into the session-identifying metadata
//! that goes onto the outbound `X-Tapes-*` envelope.
//!
//! Schema captured 2026-05-20 from a live `claude` install. Fields
//! beyond the documented set are preserved via `extra` so a future
//! harness release that adds a key surfaces in the
//! `X-Tapes-Harness-Metadata` blob instead of being silently dropped.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tapes_capture::HarnessSession;
use tracing::warn;

use tapes_capture::envelope::HARNESS_ID_CLAUDE;

/// Decoded `~/.claude/sessions/<pid>.json`. The on-disk keys are
/// camelCase; `rename_all` handles the conversion so the Rust field
/// names stay snake_case and a future shape change can be diffed
/// against this struct without per-field annotation churn.
// `PartialEq`/`Eq` mirror `CodexSessionFile` and let callers compare
// attribution outcomes structurally; every field is a string, an integer, or
// a `serde_json` value, all of which are `Eq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSessionFile {
    /// OS pid of the `claude` process that owns this session file.
    /// Cross-checked against the filename (`<pid>.json`); a mismatch
    /// would indicate a stale file from a crashed harness.
    pub pid: i64,
    /// Stable session identifier; for Claude, a UUID. This is the value a
    /// capture client attaches as `X-Tapes-Harness-Session-Id`.
    pub session_id: String,
    /// Working directory of the `claude` process at start. Attached as
    /// `X-Tapes-Cwd` and used to find the transcript file for
    /// fork-parent discovery.
    pub cwd: Option<String>,
    /// Harness version (e.g. `"2.1.145"`). Attached as
    /// `X-Tapes-Harness-Version`.
    pub version: Option<String>,
    /// Peer-protocol version the harness advertises. Captured for the
    /// metadata blob.
    pub peer_protocol: Option<i64>,
    /// `"interactive"` | `"resume"` | etc. — what mode the harness is
    /// running in. Goes into the metadata blob.
    pub kind: Option<String>,
    /// Entry point the harness was launched through (`"cli"`,
    /// `"mcp"`, …). Metadata blob.
    pub entrypoint: Option<String>,
    /// User-chosen session name (`/name` slash-command in claude).
    /// Attached as `X-Tapes-Session-Name`. Mutable across requests for
    /// the same session.
    pub name: Option<String>,
    /// Harness-reported liveness (`"idle"` / `"active"`). Metadata only;
    /// not used for routing decisions.
    pub status: Option<String>,
    /// Wall-clock string of process start (RFC-ish; the harness writes
    /// `Wed May 20 11:11:00 2026`). Metadata only.
    pub proc_start: Option<String>,
    /// Unix-ms timestamp of session start. Metadata.
    pub started_at: Option<i64>,
    /// Unix-ms timestamp of last harness-side update. Metadata.
    pub updated_at: Option<i64>,
    /// Anything the harness writes that we don't model explicitly.
    /// Kept verbatim so a schema drift surfaces in the metadata blob
    /// instead of being silently dropped.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// How a Claude session file becomes an `X-Tapes-*` envelope.
///
/// The projection used to live in the envelope producer, which is what forced
/// that module to import this one — and, through it, to sit in the crate that
/// declares harnesses. Implementing the trait here inverts the edge: the
/// producer states what it needs, and the knowledge of *which* Claude fields
/// answer it stays beside the struct that parses them. A foreign trait on a
/// local type, so the direction is the one the compiler permits.
///
/// Everything not carried in a dedicated header lands in the metadata blob.
/// `kind`, `entrypoint`, and `peerProtocol` are the modelled ones, spelled in
/// the harness's own camelCase; `extra` follows verbatim, so a Claude release
/// that adds a key travels upstream without a capture-client release. `extra`
/// is applied last and therefore wins a collision — a harness that starts
/// writing its own `kind` is describing itself more accurately than our model
/// of it.
impl HarnessSession for ClaudeSessionFile {
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

/// Read and parse `~/.claude/sessions/<pid>.json`. Returns `None` on
/// missing/unreadable file or parse failure (logged at `warn`). The
/// caller treats the absence as a cold-race or stale-file condition
/// and falls back to `harness_id: unknown`.
pub fn read(dir: &Path, pid: i32) -> Option<ClaudeSessionFile> {
    let path = dir.join(format!("{pid}.json"));
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice::<ClaudeSessionFile>(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "session: could not parse session file",
            );
            None
        }
    }
}

/// Default `~/.claude/sessions/` directory. `None` when the home dir
/// is unavailable (extremely unusual; logged by the caller).
#[must_use]
pub fn default_sessions_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("sessions"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn read_round_trips_verbatim_fields() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
            "pid": 26716,
            "sessionId": "eae77e15-c7d2-4883-b82e-251161f8eeb3",
            "cwd": "/Users/matt",
            "version": "2.1.145",
            "peerProtocol": 1,
            "kind": "interactive",
            "entrypoint": "cli",
            "name": "woo-names-are-fun",
            "status": "idle",
            "procStart": "Wed May 20 11:11:00 2026",
            "startedAt": 1779300649802,
            "updatedAt": 1779300681350,
            "futureKnob": "preserved-in-extra"
        }"#;
        std::fs::write(dir.path().join("26716.json"), body).unwrap();
        let s = read(dir.path(), 26716).unwrap();
        assert_eq!(s.session_id, "eae77e15-c7d2-4883-b82e-251161f8eeb3");
        assert_eq!(s.cwd.as_deref(), Some("/Users/matt"));
        assert_eq!(s.name.as_deref(), Some("woo-names-are-fun"));
        assert!(s.extra.contains_key("futureKnob"));
    }

    /// The envelope projection, pinned on this side of the boundary. The
    /// producer is generic over [`HarnessSession`] and so can no longer assert
    /// which Claude fields reach which header — if that mapping is only
    /// checked through a test double, a rename here changes the wire silently.
    #[test]
    fn the_envelope_projection_reads_the_fields_it_names() {
        let raw = r#"{
            "pid": 26716,
            "sessionId": "sid-1",
            "cwd": "/Users/matt",
            "version": "2.1.145",
            "peerProtocol": 1,
            "kind": "interactive",
            "entrypoint": "cli",
            "name": "woo-names",
            "futureKnob": "preserved-in-extra"
        }"#;
        let session: ClaudeSessionFile = serde_json::from_str(raw).unwrap();

        assert_eq!(session.harness_id(), HARNESS_ID_CLAUDE);
        assert_eq!(session.session_id(), "sid-1");
        assert_eq!(session.version(), Some("2.1.145"));
        assert_eq!(session.cwd(), Some("/Users/matt"));
        assert_eq!(session.name(), Some("woo-names"));

        let metadata = session.metadata();
        assert_eq!(metadata["kind"], "interactive");
        assert_eq!(metadata["entrypoint"], "cli");
        assert_eq!(metadata["peerProtocol"], 1);
        // Unmodelled keys travel verbatim, which is what lets a harness
        // release add one without a capture-client release.
        assert_eq!(metadata["futureKnob"], "preserved-in-extra");
        // Fields that have their own header are not duplicated into the blob.
        assert!(!metadata.contains_key("sessionId"));
        assert!(!metadata.contains_key("cwd"));
    }

    /// A session file with only the required keys projects to two required
    /// fields and nothing else — absence stays absence rather than becoming an
    /// empty string or a `{}` blob.
    #[test]
    fn a_sparse_session_file_projects_no_optional_fields() {
        let session: ClaudeSessionFile =
            serde_json::from_str(r#"{"pid": 1, "sessionId": "sid-2"}"#).unwrap();
        assert_eq!(session.session_id(), "sid-2");
        assert_eq!(session.version(), None);
        assert_eq!(session.cwd(), None);
        assert_eq!(session.name(), None);
        assert!(session.metadata().is_empty());
    }

    #[test]
    fn read_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path(), 9999).is_none());
    }

    #[test]
    fn read_invalid_json_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("123.json"), "not json").unwrap();
        assert!(read(dir.path(), 123).is_none());
    }
}
