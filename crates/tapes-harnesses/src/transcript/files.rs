//! Transcript-file discovery and JSONL handling.
//!
//! Extracted verbatim from paperd's `transcript_upload::files`, which in turn
//! mirrors the Go reference client
//! (`telemetry/tapes/pkg/backfill/transcript_upload.go`) — so every client that
//! feeds the transcript lane (paperd, `tapesctl`, the manual
//! `tapes backfill transcripts` CLI) produces byte-identical upload sets for the
//! same on-disk state:
//!
//! * main transcript: `<projects_dir>/<sid>.jsonl`
//! * subagents:       `<projects_dir>/<sid>/subagents/agent-<id>.jsonl`
//! * fork metadata:   `<projects_dir>/<sid>/subagents/agent-<id>.meta.json`
//!
//! and JSONL→records conversion that skips blank or malformed lines
//! (the harness occasionally truncates the final line mid-write) while
//! keeping every valid line **verbatim** — the ingest server
//! content-hashes the records array for idempotency, so the bytes must
//! be stable across pushes.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Fork metadata the harness writes alongside each subagent
/// transcript (`agent-<id>.meta.json`). `tool_use_id` is the Task
/// tool_use that forked the agent — the causal edge the tapes deriver
/// attaches.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SubagentMeta {
    /// Id of the `Task` tool_use block that spawned this subagent.
    pub tool_use_id: String,
    /// Subagent type (e.g. `general-purpose`).
    pub agent_type: String,
    /// Harness-supplied description of the delegated task.
    pub description: String,
}

/// One transcript file in a session's upload set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    /// Absolute path to the `.jsonl` file.
    pub path: PathBuf,
    /// `None` for the main transcript; `Some(<id>)` for
    /// `subagents/agent-<id>.jsonl`.
    pub agent_id: Option<String>,
    /// Parsed `agent-<id>.meta.json` when present (subagents only).
    /// Missing or malformed meta degrades to the default (empty)
    /// fields, matching the Go client's best-effort decode.
    pub meta: SubagentMeta,
}

impl TranscriptFile {
    /// Human-readable label for logs: `<sid>/main` or
    /// `<sid>/agent-<id>`.
    #[must_use]
    pub fn label(&self, sid: &str) -> String {
        match &self.agent_id {
            None => format!("{sid}/main"),
            Some(id) => format!("{sid}/agent-{id}"),
        }
    }
}

/// Discover the upload set for one session: the main transcript plus
/// every subagent transcript (with its fork metadata). Returns an
/// empty vec when the main transcript does not exist yet — a session
/// can be attributed on the wire before its first transcript flush.
///
/// Subagent discovery failures (`subagents/` missing, unreadable) are
/// not errors: most sessions never fork.
#[must_use]
pub fn session_files(projects_dir: &Path, sid: &str) -> Vec<TranscriptFile> {
    let mut out = Vec::new();
    let main = projects_dir.join(format!("{sid}.jsonl"));
    if !main.is_file() {
        return out;
    }
    out.push(TranscriptFile {
        path: main,
        agent_id: None,
        meta: SubagentMeta::default(),
    });

    let sub_dir = projects_dir.join(sid).join("subagents");
    let Ok(entries) = std::fs::read_dir(&sub_dir) else {
        return out; // no subagents
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(agent_id) = name
            .strip_prefix("agent-")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let meta_path = sub_dir.join(format!("agent-{agent_id}.meta.json"));
        let meta = std::fs::read(&meta_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<SubagentMeta>(&raw).ok())
            .unwrap_or_default();
        out.push(TranscriptFile {
            path: entry.path(),
            agent_id: Some(agent_id.to_owned()),
            meta,
        });
    }
    // Deterministic order (read_dir order is platform-dependent):
    // main first, then subagents sorted by id.
    out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    out
}

/// Convert raw JSONL bytes into a JSON array string, keeping each
/// valid line **verbatim** and skipping blank or malformed lines.
/// Verbatim matters: the ingest server's dedup key is a content hash
/// of the records array, so any re-serialization (key reordering,
/// whitespace changes) would register identical content as a new
/// version.
#[must_use]
pub fn jsonl_to_records(raw: &[u8]) -> String {
    let mut records = String::from("[");
    let mut first = true;
    for line in raw.split(|&b| b == b'\n') {
        let line = trim_ascii(line);
        if line.is_empty() || !is_valid_json(line) {
            continue;
        }
        if !first {
            records.push(',');
        }
        first = false;
        // Valid JSON is valid UTF-8 by construction; `is_valid_json`
        // already proved it parses, so the lossy conversion never
        // actually replaces bytes here.
        records.push_str(&String::from_utf8_lossy(line));
    }
    records.push(']');
    records
}

/// `true` when `bytes` parse as a single JSON value — the same check
/// as Go's `json.Valid`. `IgnoredAny` validates without building a
/// tree, so multi-MB transcripts don't allocate per line.
fn is_valid_json(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde::de::IgnoredAny>(bytes).is_ok()
}

/// `slice::trim_ascii` equivalent (stable since 1.80, spelled out here
/// to keep intent obvious): strip leading/trailing ASCII whitespace.
fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = bytes {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

/// Size + mtime fingerprint used by the trigger state machine to
/// decide whether a session's upload set changed since the last
/// successful push. Cheap (one `stat` per file) and conservative: any
/// fingerprint drift re-pushes, and the server dedups identical
/// content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFingerprint {
    /// File length in bytes.
    pub len: u64,
    /// Last-modified time.
    pub mtime: std::time::SystemTime,
}

/// Fingerprint one file. `None` when the file vanished or `stat`
/// failed — callers treat that as "skip this tick".
#[must_use]
pub fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileFingerprint {
        len: meta.len(),
        mtime: meta.modified().ok()?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_to_records_keeps_valid_lines_verbatim() {
        // Key order and spacing inside each line must survive — the
        // server's dedup hash is computed over these exact bytes.
        let raw = b"{\"b\":1,\"a\":2}\n{\"x\": \"y\"}\n";
        assert_eq!(jsonl_to_records(raw), r#"[{"b":1,"a":2},{"x": "y"}]"#);
    }

    #[test]
    fn jsonl_to_records_skips_blank_and_malformed_lines() {
        // The harness occasionally truncates the final line mid-write;
        // the Go client (and the server) skip it rather than fail.
        let raw = b"{\"ok\":1}\n\n   \n{\"trunc\":tr\n{\"ok\":2}";
        assert_eq!(jsonl_to_records(raw), r#"[{"ok":1},{"ok":2}]"#);
    }

    #[test]
    fn jsonl_to_records_empty_input_is_empty_array() {
        assert_eq!(jsonl_to_records(b""), "[]");
        assert_eq!(jsonl_to_records(b"\n\n"), "[]");
    }

    #[test]
    fn session_files_missing_main_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(session_files(dir.path(), "ghost").is_empty());
    }

    #[test]
    fn session_files_main_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sid-1.jsonl"), "{}\n").unwrap();
        let files = session_files(dir.path(), "sid-1");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].agent_id, None);
        assert_eq!(files[0].label("sid-1"), "sid-1/main");
    }

    #[test]
    fn session_files_discovers_subagents_with_meta() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sid-1.jsonl"), "{}\n").unwrap();
        let sub = dir.path().join("sid-1").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-abc.jsonl"), "{}\n").unwrap();
        std::fs::write(
            sub.join("agent-abc.meta.json"),
            r#"{"toolUseId":"toolu_01","agentType":"general-purpose","description":"dig"}"#,
        )
        .unwrap();
        // A subagent without meta.json degrades to empty fields.
        std::fs::write(sub.join("agent-zzz.jsonl"), "{}\n").unwrap();
        // Non-transcript noise is ignored.
        std::fs::write(sub.join("agent-abc.meta.json.bak"), "junk").unwrap();
        std::fs::write(sub.join("notes.txt"), "junk").unwrap();

        let files = session_files(dir.path(), "sid-1");
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].agent_id, None, "main sorts first");
        assert_eq!(files[1].agent_id.as_deref(), Some("abc"));
        assert_eq!(files[1].meta.tool_use_id, "toolu_01");
        assert_eq!(files[1].meta.agent_type, "general-purpose");
        assert_eq!(files[1].meta.description, "dig");
        assert_eq!(files[1].label("sid-1"), "sid-1/agent-abc");
        assert_eq!(files[2].agent_id.as_deref(), Some("zzz"));
        assert_eq!(files[2].meta, SubagentMeta::default());
    }

    #[test]
    fn fingerprint_tracks_len() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.jsonl");
        std::fs::write(&p, "{}\n").unwrap();
        let a = fingerprint(&p).unwrap();
        assert_eq!(a.len, 3);
        std::fs::write(&p, "{}\n{}\n").unwrap();
        let b = fingerprint(&p).unwrap();
        assert_ne!(a, b);
        assert!(fingerprint(&dir.path().join("missing")).is_none());
    }
}
