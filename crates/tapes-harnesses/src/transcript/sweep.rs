//! Sweep-on-start: find transcripts on disk that no live session would reveal.
//!
//! This is the one piece of the transcript lane that is **new** rather than
//! moved, and it closes a real gap in paperd's design.
//!
//! # The gap
//!
//! paperd learns which sessions to upload from its proxy's per-request
//! attribution: a forwarded request maps to a `~/.claude/sessions/<pid>.json`,
//! and that session enters the registry. The registry lives in memory only, and
//! is rebuilt from live traffic after a restart. That works for any session still
//! running — its next request re-registers it — but a session that **started and
//! ended while the daemon was down**, or one that was mid-flight when the daemon
//! died and exited before it came back, is never re-registered. Its transcript
//! sits on disk indefinitely, recoverable only by someone remembering to run
//! `tapes backfill transcripts` by hand.
//!
//! The window is not exotic: it is every daemon restart, every upgrade, and every
//! crash. And the transcript is the *only* source of the causal/fork skeleton —
//! wire capture yields a complete call inventory but no fork edges — so a missed
//! transcript is permanently missing structure, not a delayed duplicate.
//!
//! # The fix
//!
//! [`sweep`] reads the transcript tree directly instead of asking the registry,
//! so it sees every session that ever wrote a transcript under the given root
//! regardless of whether a process is alive. A client runs it once at startup and
//! pushes what it finds.
//!
//! This is only safe because the ingest endpoint dedups on a content hash (see
//! [`super::payload`]): a sweep that re-offers a thousand already-uploaded
//! transcripts costs bandwidth and answers `deduped`, and re-offering is
//! precisely the point — the client cannot know what the *previous* process
//! managed to send. [`SweepOptions::modified_within`] exists to bound that cost,
//! not to make it correct.
//!
//! # Recovering the session envelope
//!
//! The projects directory name is the cwd with `/` replaced by `-` (see
//! [`crate::attribution::claude::fork_parent::encode_cwd`]), which is **not reversible**:
//! a path containing a literal `-` decodes ambiguously. So sweep does not decode
//! it. It reads the head of the transcript instead, where the harness records the
//! true `cwd` and its own `version` on most records — exact values rather than
//! guesses.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use super::files::{self, TranscriptFile};

/// Bytes of each transcript read while recovering `cwd` / `version`.
///
/// The harness writes a handful of preamble records (`mode`,
/// `permission-mode`, `file-history-snapshot`) that carry neither field before
/// the first record that does, so this has to be more than a line or two — but a
/// transcript can be many megabytes, and sweeping a large tree must not read all
/// of it.
const HEAD_SCAN_BYTES: usize = 64 * 1024;

/// A session discovered on disk by [`sweep`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweptSession {
    /// The harness session id, from the transcript's filename.
    pub session_id: String,
    /// The project directory the transcript set lives in.
    pub projects_dir: PathBuf,
    /// True working directory, read out of the transcript's own records. `None`
    /// when no record in the scanned head carried one.
    pub cwd: Option<String>,
    /// Harness version, read out of the transcript's own records.
    pub harness_version: Option<String>,
    /// The session's full upload set — main transcript plus any subagents, in the
    /// same order [`files::session_files`] returns.
    pub files: Vec<TranscriptFile>,
}

/// Bounds on what a sweep will report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepOptions {
    /// Only report sessions whose newest transcript file was modified within this
    /// window. `None` reports everything under the root.
    ///
    /// A cost control, not a correctness one: the endpoint dedups, so a wider
    /// window is always safe. Clients with a long-lived transcript tree will want
    /// one — a year of sessions is a lot of pointless `deduped` responses at
    /// every restart.
    pub modified_within: Option<Duration>,
}

impl SweepOptions {
    /// Report only sessions touched within `window`.
    #[must_use]
    pub fn modified_within(window: Duration) -> Self {
        Self {
            modified_within: Some(window),
        }
    }
}

/// Walk `projects_root` and return every session with a transcript on disk.
///
/// `projects_root` is the harness's project tree — `~/.claude/projects/` — whose
/// immediate children are cwd-encoded directories, each holding `<sid>.jsonl`
/// files.
///
/// Best-effort throughout: an unreadable root yields an empty vec, and an
/// unreadable subdirectory or a vanished file is skipped rather than failing the
/// sweep. A startup scan that cannot read one project must still report the rest.
///
/// Results are sorted by session id so a sweep is reproducible; `read_dir` order
/// is platform-dependent.
#[must_use]
pub fn sweep(projects_root: &Path, options: &SweepOptions) -> Vec<SweptSession> {
    let cutoff = options
        .modified_within
        .and_then(|window| SystemTime::now().checked_sub(window));

    let Ok(projects) = std::fs::read_dir(projects_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for project in projects.flatten() {
        let projects_dir = project.path();
        if !projects_dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&projects_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(session_id) = name.strip_suffix(".jsonl") else {
                continue;
            };
            // `session_files` re-stats the main transcript and returns an empty
            // set if it is not a regular file, which also filters directories
            // that happen to end in `.jsonl`.
            let set = files::session_files(&projects_dir, session_id);
            if set.is_empty() {
                continue;
            }
            if let Some(cutoff) = cutoff
                && !touched_since(&set, cutoff)
            {
                continue;
            }
            let (cwd, harness_version) = read_session_facts(&entry.path());
            out.push(SweptSession {
                session_id: session_id.to_owned(),
                projects_dir: projects_dir.clone(),
                cwd,
                harness_version,
                files: set,
            });
        }
    }
    // projects_dir breaks ties so equal session ids (the same session seen
    // under two roots) sweep in one reproducible order rather than
    // filesystem-enumeration order.
    out.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then_with(|| a.projects_dir.cmp(&b.projects_dir))
    });
    out
}

/// `true` when any file in the set was modified at or after `cutoff`.
fn touched_since(set: &[TranscriptFile], cutoff: SystemTime) -> bool {
    set.iter()
        .filter_map(|file| files::fingerprint(&file.path))
        .any(|fp| fp.mtime >= cutoff)
}

/// One transcript record, reduced to the two facts a sweep needs.
///
/// The harness stamps `cwd` and `version` on most records but not on its preamble
/// (`mode`, `permission-mode`, `file-history-snapshot`), so a scan takes the
/// first of each it finds rather than assuming record zero has them.
#[derive(Deserialize)]
struct SessionFacts {
    cwd: Option<String>,
    version: Option<String>,
}

/// Recover `(cwd, harness_version)` from the head of a transcript.
///
/// Returns `(None, None)` when the file cannot be read or no scanned record
/// carried either field — both are optional on the wire, so an unknown value
/// simply travels as absent.
fn read_session_facts(path: &Path) -> (Option<String>, Option<String>) {
    let Some(head) = read_head(path, HEAD_SCAN_BYTES) else {
        return (None, None);
    };
    let mut cwd = None;
    let mut version = None;
    for line in head.split(|&b| b == b'\n') {
        // The final line of a bounded read is usually truncated mid-record; a
        // failed parse is expected and simply skipped.
        let Ok(facts) = serde_json::from_slice::<SessionFacts>(line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = facts.cwd.filter(|value| !value.is_empty());
        }
        if version.is_none() {
            version = facts.version.filter(|value| !value.is_empty());
        }
        if cwd.is_some() && version.is_some() {
            break;
        }
    }
    (cwd, version)
}

/// Read up to `cap` bytes from the head of `path`.
fn read_head(path: &Path, cap: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; cap];
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);
    Some(buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Lay down `<root>/<encoded-cwd>/<sid>.jsonl` with records that carry the
    /// harness's `cwd` and `version`, preceded by the preamble records that
    /// carry neither.
    fn write_session(root: &Path, cwd: &str, sid: &str) -> PathBuf {
        let dir = root.join(crate::attribution::claude::fork_parent::encode_cwd(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{sid}.jsonl"));
        // Two preamble records carrying neither fact, then a real one — the
        // layout a live harness writes.
        let body = format!(
            concat!(
                "{{\"type\":\"mode\",\"sessionId\":\"x\"}}\n",
                "{{\"type\":\"file-history-snapshot\"}}\n",
                "{{\"type\":\"user\",\"sessionId\":\"{sid}\",\"cwd\":\"{cwd}\",",
                "\"version\":\"2.1.205\"}}\n",
            ),
            sid = sid,
            cwd = cwd,
        );
        std::fs::write(&path, body).unwrap();
        path
    }

    /// Backdate a file's mtime without sleeping.
    fn backdate(path: &Path, by: Duration) {
        let file = std::fs::File::options().append(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    /// The gap this module exists to close.
    ///
    /// A client whose session registry is rebuilt from live traffic can only see
    /// `live` — the session with a `sessions/<pid>.json` behind it. `orphan`
    /// started and ended while the client was down, so no request will ever
    /// re-register it and its transcript would sit on disk forever. Sweep reads
    /// the transcript tree instead of the registry, so it reports both.
    ///
    /// The `assert!` on `orphan` is the one that would fail against pre-sweep
    /// behaviour: registry-driven discovery returns exactly `[live]`.
    #[test]
    fn sweep_finds_sessions_no_live_registry_would_reveal() {
        let root = tempfile::tempdir().unwrap();
        write_session(root.path(), "/x/y", "live");
        write_session(root.path(), "/a/b", "orphan");

        let swept = sweep(root.path(), &SweepOptions::default());
        let ids: Vec<&str> = swept.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["live", "orphan"], "sorted by session id");
        assert!(
            ids.contains(&"orphan"),
            "a session with no live process must still be swept",
        );
    }

    /// Sweep recovers the *true* cwd from the transcript's own records rather
    /// than decoding the directory name — which is not decodable, because
    /// `encode_cwd` maps `/` to `-` and a path containing a literal `-` is
    /// ambiguous. This cwd round-trips to the same encoded directory as
    /// `/opt/my-project`, so a decoder would have to guess.
    #[test]
    fn sweep_reads_the_true_cwd_and_version_from_the_transcript() {
        let root = tempfile::tempdir().unwrap();
        write_session(root.path(), "/opt/my-project", "sid-1");

        let swept = sweep(root.path(), &SweepOptions::default());
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].cwd.as_deref(), Some("/opt/my-project"));
        assert_eq!(swept[0].harness_version.as_deref(), Some("2.1.205"));
        assert_eq!(
            swept[0].projects_dir.file_name().unwrap().to_string_lossy(),
            "-opt-my-project",
            "the encoded directory is genuinely ambiguous, hence reading the records",
        );
    }

    /// A transcript whose records carry neither fact still sweeps — both fields
    /// are optional on the wire, so unknown travels as absent rather than
    /// blocking the upload.
    #[test]
    fn sweep_reports_a_session_whose_records_carry_no_facts() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("-x-y");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bare.jsonl"), "{\"type\":\"mode\"}\n").unwrap();

        let swept = sweep(root.path(), &SweepOptions::default());
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].session_id, "bare");
        assert_eq!(swept[0].cwd, None);
        assert_eq!(swept[0].harness_version, None);
    }

    /// The full upload set comes along, subagents and fork metadata included —
    /// sweep is discovery for the same push path a live session uses, not a
    /// reduced one.
    #[test]
    fn sweep_carries_the_whole_upload_set() {
        let root = tempfile::tempdir().unwrap();
        write_session(root.path(), "/x/y", "sid-1");
        let sub = root.path().join("-x-y").join("sid-1").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-abc.jsonl"), "{}\n").unwrap();
        std::fs::write(
            sub.join("agent-abc.meta.json"),
            r#"{"toolUseId":"toolu_7","agentType":"explore","description":"d"}"#,
        )
        .unwrap();

        let swept = sweep(root.path(), &SweepOptions::default());
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].files.len(), 2);
        assert_eq!(swept[0].files[0].agent_id, None, "main sorts first");
        assert_eq!(swept[0].files[1].agent_id.as_deref(), Some("abc"));
        assert_eq!(swept[0].files[1].meta.tool_use_id, "toolu_7");
    }

    /// The age window bounds sweep's cost on a long-lived transcript tree.
    #[test]
    fn sweep_honours_the_modified_within_window() {
        let root = tempfile::tempdir().unwrap();
        write_session(root.path(), "/x/y", "recent");
        let stale = write_session(root.path(), "/a/b", "stale");
        backdate(&stale, Duration::from_secs(60 * 60 * 24 * 30));

        let all = sweep(root.path(), &SweepOptions::default());
        assert_eq!(all.len(), 2, "no window reports everything");

        let recent = sweep(
            root.path(),
            &SweepOptions::modified_within(Duration::from_secs(3600)),
        );
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].session_id, "recent");
    }

    /// A session is inside the window when *any* of its files is: a long-idle
    /// main transcript whose subagent just wrote must not be dropped.
    #[test]
    fn sweep_window_considers_the_newest_file_in_the_set() {
        let root = tempfile::tempdir().unwrap();
        let main = write_session(root.path(), "/x/y", "sid-1");
        let sub = root.path().join("-x-y").join("sid-1").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-abc.jsonl"), "{}\n").unwrap();
        backdate(&main, Duration::from_secs(60 * 60 * 24 * 30));

        let swept = sweep(
            root.path(),
            &SweepOptions::modified_within(Duration::from_secs(3600)),
        );
        assert_eq!(swept.len(), 1, "the fresh subagent keeps the session in");
    }

    /// Non-transcript noise and an unreadable root are both non-events: a
    /// startup scan must degrade rather than fail.
    #[test]
    fn sweep_ignores_noise_and_a_missing_root() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("-x-y");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "junk").unwrap();
        std::fs::write(dir.join("archive.jsonl.bak"), "junk").unwrap();
        // A directory whose name ends in .jsonl is not a transcript.
        std::fs::create_dir_all(dir.join("weird.jsonl")).unwrap();
        // A stray file directly under the root, not inside a project dir.
        std::fs::write(root.path().join("loose.jsonl"), "{}\n").unwrap();

        assert!(sweep(root.path(), &SweepOptions::default()).is_empty());
        assert!(sweep(&root.path().join("nope"), &SweepOptions::default()).is_empty());
    }
}
