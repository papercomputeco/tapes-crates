//! The version manifest: what the matrix actually ran against.
//!
//! # Why a run emits this
//!
//! A matrix cell that passes tells you the composition worked *with the harness
//! version that happened to be installed*. Harnesses ship constantly, and the
//! failure this whole exercise is aimed at — each layer green in isolation while
//! the composition breaks — arrives most often as a harness release that changes
//! a wire detail nobody's unit tests cover. A green run whose harness versions
//! are unrecorded cannot distinguish "we still work against the current release"
//! from "we still work against the release that was current six weeks ago".
//!
//! So every run writes down what it ran: the resolved binary, the version string
//! it printed, and — for the cells that did not run — the reason, in the same
//! document. That last part matters as much as the versions. A manifest listing
//! only successes would let a harness silently drop out of the matrix and still
//! produce a plausible-looking record.
//!
//! This is the drift watch's input, and it is deliberately only the input: this
//! module records and compares, and takes no view on what should happen when a
//! version moves. That policy belongs with whatever runs on a schedule, not with
//! a test that runs on every change.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The manifest schema version.
///
/// Bumped when the shape changes incompatibly, so a drift watch reading an old
/// artifact can say "I do not understand this" instead of silently comparing
/// fields that have moved.
pub const SCHEMA_VERSION: u32 = 1;

/// What happened to one matrix participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum Status {
    /// It ran, and reported this version.
    Ran {
        /// The version string the binary printed, trimmed.
        version: String,
        /// Where the binary was resolved from.
        path: PathBuf,
    },
    /// It was not run, for a stated reason.
    ///
    /// A skip is a first-class outcome with a mandatory reason: an absent row
    /// and a passing row look identical in a report, which is the ambiguity the
    /// matrix exists to remove.
    Skipped {
        /// Why it did not run, in words a reader can act on.
        reason: String,
    },
}

impl Status {
    /// Did this participant run?
    #[must_use]
    pub const fn ran(&self) -> bool {
        matches!(self, Self::Ran { .. })
    }

    /// The version string, when it ran.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Ran { version, .. } => Some(version),
            Self::Skipped { .. } => None,
        }
    }
}

/// One difference between two manifests.
///
/// Structured rather than only rendered, because the same three differences mean
/// different things depending on what is being compared. Between two *runs*,
/// [`Change::Stopped`] means a harness fell out of the matrix; between a run and
/// the committed version record it means this run did not cover a harness the
/// record knows about, which on a machine without that harness installed is
/// unremarkable. One comparison, two vocabularies — so the comparison produces
/// values and each caller supplies its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The version moved.
    Moved {
        /// The participant.
        name: String,
        /// What it was.
        from: String,
        /// What it is now.
        to: String,
    },
    /// It ran here and not in what it is being compared against.
    Started {
        /// The participant.
        name: String,
        /// The version it ran at.
        version: String,
    },
    /// It ran in what is being compared against, and not here.
    Stopped {
        /// The participant.
        name: String,
        /// The version it last ran at.
        was: String,
    },
}

impl Change {
    /// The participant this change is about.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Moved { name, .. } | Self::Started { name, .. } | Self::Stopped { name, .. } => {
                name
            }
        }
    }

    /// The run-to-run rendering, as [`VersionManifest::drift_from`] emits it.
    #[must_use]
    pub fn to_line(&self) -> String {
        match self {
            Self::Moved { name, from, to } => format!("{name}: {from} -> {to}"),
            Self::Started { name, version } => format!("{name}: now running at {version}"),
            Self::Stopped { name, was } => format!("{name}: no longer running (was {was})"),
        }
    }
}

/// One row: a harness or a CLI, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The participant's name — a registry harness id, or a CLI's name.
    pub name: String,
    /// What happened to it.
    #[serde(flatten)]
    pub status: Status,
}

/// Everything a matrix run ran against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionManifest {
    /// The schema version this document is written to.
    pub schema: u32,
    /// The registry harnesses, in registry order.
    pub harnesses: Vec<Entry>,
    /// The CLIs the composition cells were driven through.
    pub clis: Vec<Entry>,
}

impl VersionManifest {
    /// An empty manifest at the current schema version.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            harnesses: Vec::new(),
            clis: Vec::new(),
        }
    }

    /// Record a harness row.
    pub fn record_harness(&mut self, name: impl Into<String>, status: Status) {
        self.harnesses.push(Entry {
            name: name.into(),
            status,
        });
    }

    /// Record a CLI row.
    pub fn record_cli(&mut self, name: impl Into<String>, status: Status) {
        self.clis.push(Entry {
            name: name.into(),
            status,
        });
    }

    /// The manifest as pretty JSON with a trailing newline, ready to write.
    ///
    /// # Errors
    ///
    /// Returns the serialisation failure, which in practice cannot happen for
    /// this shape but is surfaced rather than swallowed.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        Ok(format!("{}\n", serde_json::to_string_pretty(self)?))
    }

    /// Every participant that ran, name to version.
    ///
    /// The comparison unit for a drift watch: two runs' maps differ exactly
    /// where a version moved or a participant changed whether it ran at all.
    #[must_use]
    pub fn versions(&self) -> BTreeMap<String, String> {
        self.harnesses
            .iter()
            .chain(self.clis.iter())
            .filter_map(|entry| {
                entry
                    .status
                    .version()
                    .map(|version| (entry.name.clone(), version.to_owned()))
            })
            .collect()
    }

    /// How `self` differs from `previous`, as structured changes.
    ///
    /// Reports three kinds of change and no others: a version that moved, a
    /// participant that started running, and one that stopped. The third is the
    /// one a naive diff would miss — a harness that drops out of the matrix
    /// produces no failing cell, only a row that quietly became a skip.
    ///
    /// Ordered by participant name, so two callers rendering the same comparison
    /// print it in the same order.
    #[must_use]
    pub fn changes_from(&self, previous: &Self) -> Vec<Change> {
        let (before, after) = (previous.versions(), self.versions());
        let mut changes = Vec::new();

        for (name, version) in &after {
            match before.get(name) {
                Some(old) if old != version => changes.push(Change::Moved {
                    name: name.clone(),
                    from: old.clone(),
                    to: version.clone(),
                }),
                Some(_) => {}
                None => changes.push(Change::Started {
                    name: name.clone(),
                    version: version.clone(),
                }),
            }
        }
        for (name, old) in &before {
            if !after.contains_key(name) {
                changes.push(Change::Stopped {
                    name: name.clone(),
                    was: old.clone(),
                });
            }
        }
        changes.sort_by(|a, b| a.name().cmp(b.name()));
        changes
    }

    /// [`Self::changes_from`], rendered for a run-to-run comparison.
    #[must_use]
    pub fn drift_from(&self, previous: &Self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .changes_from(previous)
            .iter()
            .map(Change::to_line)
            .collect();
        lines.sort();
        lines
    }

    /// The harness rows alone, as a manifest.
    ///
    /// The comparison against the committed version record is about harnesses
    /// and nothing else: the record holds no client CLIs, so comparing whole
    /// manifests would report every developer who supplied a client binary as
    /// running something unrecorded.
    #[must_use]
    pub fn harnesses_only(&self) -> Self {
        Self {
            schema: self.schema,
            harnesses: self.harnesses.clone(),
            clis: Vec::new(),
        }
    }

    /// Why `name` did not run, when the manifest says it did not.
    #[must_use]
    pub fn skip_reason(&self, name: &str) -> Option<&str> {
        self.harnesses
            .iter()
            .chain(self.clis.iter())
            .find(|entry| entry.name == name)
            .and_then(|entry| match &entry.status {
                Status::Skipped { reason } => Some(reason.as_str()),
                Status::Ran { .. } => None,
            })
    }
}

impl Default for VersionManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// Ask a binary for its version.
///
/// Returns [`Status::Skipped`] with a reason rather than an error, because
/// "this binary is not installed here" is an ordinary and expected outcome — it
/// is the normal state of most harnesses on most CI runners — and forcing every
/// caller to translate an error into a skip reason is how skip reasons end up
/// inconsistent.
#[must_use]
pub fn probe(binary: &str, version_args: &[&str]) -> Status {
    if version_args.is_empty() {
        return Status::Skipped {
            reason: format!("{binary} has no version invocation"),
        };
    }

    let Some(path) = which(binary) else {
        return Status::Skipped {
            reason: format!("{binary} is not on PATH"),
        };
    };

    match Command::new(&path).args(version_args).output() {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let version = text.trim();
            // Some harnesses print the version on stderr.
            let version = if version.is_empty() {
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            } else {
                version.to_owned()
            };
            if version.is_empty() {
                Status::Skipped {
                    reason: format!("{binary} printed no version"),
                }
            } else {
                Status::Ran { version, path }
            }
        }
        Ok(output) => Status::Skipped {
            reason: format!("{binary} --version exited {}", output.status),
        },
        Err(err) => Status::Skipped {
            reason: format!("{binary} could not be run: {err}"),
        },
    }
}

/// Resolve `binary` on `PATH`.
///
/// Hand-rolled rather than pulled in as a dependency: it is eight lines, and a
/// test-support crate earning a transitive dependency tree for a `PATH` walk is
/// a poor trade in a repository this lean.
#[must_use]
pub fn which(binary: &str) -> Option<PathBuf> {
    // An absolute or relative path is taken as given, so a caller can point at
    // a build output that is not installed anywhere.
    if binary.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(binary);
        return path.is_file().then_some(path);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(binary))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn ran(version: &str) -> Status {
        Status::Ran {
            version: version.to_owned(),
            path: PathBuf::from("/usr/bin/x"),
        }
    }

    /// A skip carries its reason into the document, so a manifest cannot record
    /// a cell that did not run without saying why.
    #[test]
    fn a_skip_is_serialised_with_its_reason() {
        let mut manifest = VersionManifest::new();
        manifest.record_harness(
            "codex-app",
            Status::Skipped {
                reason: "no one-shot invocation".to_owned(),
            },
        );
        let json = manifest.to_json().unwrap();
        assert!(json.contains(r#""status": "skipped""#));
        assert!(json.contains("no one-shot invocation"));
        assert!(json.ends_with('\n'));
    }

    /// A manifest round-trips through JSON unchanged — the property a drift
    /// watch depends on when it reads yesterday's artifact.
    #[test]
    fn a_manifest_round_trips_through_json() {
        let mut manifest = VersionManifest::new();
        manifest.record_harness("claude", ran("2.1.219"));
        manifest.record_cli(
            "tapesctl",
            Status::Skipped {
                reason: "TAPESCTL_BIN unset".to_owned(),
            },
        );
        let json = manifest.to_json().unwrap();
        let parsed: VersionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, manifest);
    }

    /// A moved version is reported with both sides, so a reader knows which
    /// direction it went.
    #[test]
    fn drift_reports_a_moved_version_with_both_sides() {
        let mut before = VersionManifest::new();
        before.record_harness("claude", ran("2.1.218"));
        let mut after = VersionManifest::new();
        after.record_harness("claude", ran("2.1.219"));

        assert_eq!(
            after.drift_from(&before),
            vec!["claude: 2.1.218 -> 2.1.219"]
        );
    }

    /// A harness that stops running is drift, not silence. This is the case a
    /// diff of successes alone would miss entirely, and it is how a harness
    /// falls out of the matrix unnoticed.
    #[test]
    fn a_harness_that_stops_running_is_reported() {
        let mut before = VersionManifest::new();
        before.record_harness("pi", ran("0.9.0"));
        let mut after = VersionManifest::new();
        after.record_harness(
            "pi",
            Status::Skipped {
                reason: "pi is not on PATH".to_owned(),
            },
        );

        assert_eq!(
            after.drift_from(&before),
            vec!["pi: no longer running (was 0.9.0)"],
        );
    }

    /// An unchanged run reports no drift at all.
    #[test]
    fn an_unchanged_run_reports_nothing() {
        let mut manifest = VersionManifest::new();
        manifest.record_harness("claude", ran("2.1.219"));
        assert!(manifest.drift_from(&manifest.clone()).is_empty());
    }

    /// A binary that is not installed is a skip naming the binary, not an
    /// error the caller has to translate.
    #[test]
    fn an_absent_binary_probes_as_a_named_skip() {
        let status = probe(
            "tapes-mock-upstream-definitely-not-installed",
            &["--version"],
        );
        match status {
            Status::Skipped { reason } => assert!(reason.contains("not on PATH")),
            Status::Ran { .. } => panic!("a missing binary must not report as ran"),
        }
    }

    /// `which` honours an explicit path, so a caller can name a build output
    /// that was never installed.
    #[test]
    fn which_accepts_an_explicit_path() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().display().to_string();
        assert_eq!(which(&path), Some(file.path().to_path_buf()));
        assert_eq!(which(&format!("{path}-absent")), None);
    }
}
