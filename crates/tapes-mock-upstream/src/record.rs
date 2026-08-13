//! The committed version record: what the matrix last passed against.
//!
//! # Why a run needs something to compare to
//!
//! [`crate::manifest`] answers "what did this run run against". It cannot answer
//! the question that follows — "is that still the current release?" — because a
//! single run has nothing to compare itself to. Comparing to the *previous* run
//! does not answer it either: two runs a minute apart on the same runner agree
//! perfectly while both sit six weeks behind upstream.
//!
//! So the answer is written down. `harness-versions.json` at the repository root
//! records, per harness, the version the matrix last passed against, and how to
//! ask upstream what the current version is. It is a committed file rather than
//! a cached artifact for the property that makes the whole scheme reviewable: a
//! version moves only through a pull request a human merged, so the record is
//! the set of versions this repository *claims* to work against, not a log of
//! what some runner happened to have installed.
//!
//! # Two versions per harness, deliberately
//!
//! A harness prints its version in whatever shape it likes — `2.1.220 (Claude
//! Code)`, `codex-cli 0.145.0`, a bare `1.18.4` — and its package registry
//! answers with a bare semver. Parsing one into the other means a regex per
//! harness, and a regex that silently stops matching after an upstream reword
//! turns the watch off without saying so.
//!
//! Both are recorded instead. [`RecordEntry::version`] is the exact string the
//! binary printed, compared to a run's manifest by equality and nothing else.
//! [`RecordEntry::upstream_version`] is the registry's version for that same
//! release, compared to discovery. Neither comparison parses anything, and a
//! harness that rewords its `--version` output shows up as ordinary drift a
//! human reads, rather than as a watch that quietly stopped working.
//!
//! # This module compares; it decides nothing
//!
//! A [`Comparison`] is a description. Whether drift should open a pull request,
//! file an issue, or be ignored until Monday is policy, and policy lives in the
//! scheduled workflow. In particular a matrix run *states* drift and stays
//! green: the record is what the matrix last passed against, and a newer harness
//! on a developer's machine is information, not a regression.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use snafu::{ResultExt as _, Snafu};

use crate::manifest::{Change, VersionManifest};

/// The record schema version.
///
/// Read strictly: a record written to a schema this code does not know is an
/// error rather than a best-effort parse, for the same reason the manifest
/// carries one. A comparison that silently ignored fields that had moved would
/// report "no drift" with more confidence than it had.
pub const SCHEMA_VERSION: u32 = 1;

/// The record's file name, at the repository root.
pub const RECORD_NAME: &str = "harness-versions.json";

/// The variable that points at a record other than the committed one.
///
/// For tests, and for the perturbation check that proves a doctored record makes
/// a run report drift — which is the only way to see the drift path work while
/// the committed record is, as it should be, up to date.
pub const RECORD_ENV: &str = "HARNESS_VERSIONS_RECORD";

/// Where a harness's current version is discovered.
///
/// A closed set on purpose. A record naming a kind this code does not implement
/// fails to load rather than being treated as unwatched: the scheduled watcher
/// would otherwise silently stop watching a harness the record says it watches,
/// which is the class of quiet coverage loss this whole area exists to refuse.
/// Adding a kind — GitHub releases, a nixpkgs attribute — means a variant here
/// and a branch in `scripts/harness-latest-versions.sh`, both of which are
/// required for it to work at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiscoveryKind {
    /// An npm package; the current version is its `latest` dist-tag.
    Npm,
    /// Not watched automatically, for the reason in the entry's note.
    Unwatched,
}

/// How to find out what the current version of a harness is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discovery {
    /// Which kind of source answers the question.
    pub kind: DiscoveryKind,
    /// The source itself — an npm package name — or `null` when unwatched.
    pub source: Option<String>,
    /// Why this source, and anything a reader needs to trust the answer.
    ///
    /// Mandatory. The note is where a harness whose packaging does not match its
    /// provenance says so, and an entry that cannot explain its own source is an
    /// entry nobody can audit.
    pub note: String,
}

/// One harness's record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordEntry {
    /// The registry harness id.
    pub name: String,
    /// The exact version string the binary printed in the passing run, or `null`
    /// for a harness the matrix cannot run at all.
    pub version: Option<String>,
    /// The discovery source's version for that same release, or `null`.
    pub upstream_version: Option<String>,
    /// Where its current version is discovered.
    pub discovery: Discovery,
}

impl RecordEntry {
    /// Is this entry watched by the scheduled drift watch?
    #[must_use]
    pub fn is_watched(&self) -> bool {
        !matches!(self.discovery.kind, DiscoveryKind::Unwatched)
    }
}

/// The whole record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The schema this document is written to.
    pub schema: u32,
    /// Every registry harness, watched or not.
    pub harnesses: Vec<RecordEntry>,
}

/// Why a record could not be loaded.
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
#[non_exhaustive]
pub enum RecordError {
    /// The file could not be read.
    #[snafu(display("the version record at {} could not be read", path.display()))]
    Read {
        /// Where it was looked for.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The file is not a record.
    #[snafu(display("the version record at {} could not be parsed", path.display()))]
    Parse {
        /// The file that could not be parsed.
        path: PathBuf,
        /// The underlying deserialisation failure.
        source: serde_json::Error,
    },

    /// The record is written to a schema this code does not know.
    #[snafu(display(
        "the version record at {} is schema {found}; this build understands schema {SCHEMA_VERSION}",
        path.display(),
    ))]
    Schema {
        /// The file that was read.
        path: PathBuf,
        /// The schema it declares.
        found: u32,
    },
}

impl Record {
    /// Where the committed record lives.
    ///
    /// Resolved from this crate's own manifest directory rather than from the
    /// working directory: a test's working directory is the crate root, a
    /// `make` invocation's is the repository root, and a path that depends on
    /// which one you used is a path that works until someone runs it the other
    /// way.
    #[must_use]
    pub fn default_path() -> PathBuf {
        std::env::var_os(RECORD_ENV).map_or_else(
            || {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(RECORD_NAME)
            },
            PathBuf::from,
        )
    }

    /// Load a record from `path`.
    ///
    /// # Errors
    ///
    /// Returns [`RecordError`] when the file is absent, unparseable, or written
    /// to another schema. All three are repository faults rather than drift, and
    /// they are surfaced rather than degraded into "no drift found".
    pub fn load(path: &Path) -> Result<Self, RecordError> {
        let text = std::fs::read_to_string(path).context(record_error::ReadSnafu { path })?;
        let record: Self =
            serde_json::from_str(&text).context(record_error::ParseSnafu { path })?;
        snafu::ensure!(
            record.schema == SCHEMA_VERSION,
            record_error::SchemaSnafu {
                path,
                found: record.schema,
            }
        );
        Ok(record)
    }

    /// The entry for `name`, if the record has one.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&RecordEntry> {
        self.harnesses.iter().find(|entry| entry.name == name)
    }

    /// The record as a manifest, so one comparison serves both.
    ///
    /// The record is a claim of the form a manifest makes — these harnesses, at
    /// these versions — so it is compared by turning it into one rather than by
    /// writing a second comparison that would have to be kept in step with
    /// [`VersionManifest::changes_from`].
    #[must_use]
    pub fn as_manifest(&self) -> VersionManifest {
        let mut manifest = VersionManifest::new();
        for entry in &self.harnesses {
            let status = match (&entry.version, &entry.discovery) {
                (Some(version), _) => crate::manifest::Status::Ran {
                    version: version.clone(),
                    // The record keeps no path: where a binary lived on the
                    // runner that last passed is not a claim about anything.
                    path: PathBuf::from(&entry.name),
                },
                (None, discovery) => crate::manifest::Status::Skipped {
                    reason: discovery.note.clone(),
                },
            };
            manifest.record_harness(entry.name.clone(), status);
        }
        manifest
    }

    /// How a run's manifest stands against this record.
    #[must_use]
    pub fn compare(&self, manifest: &VersionManifest) -> Comparison {
        let ran = manifest.harnesses_only();
        let mut comparison = Comparison::default();

        for change in ran.changes_from(&self.as_manifest()) {
            match change {
                Change::Moved { name, from, to } => comparison.drifted.push(format!(
                    "{name}: the record passed against {from}; this run ran {to}",
                )),
                // A harness the record has a version for that this run did not
                // run. Not drift — it is the ordinary state of any machine
                // missing a harness — so it is reported in its own bucket, with
                // the run's own skip reason rather than a guess.
                Change::Stopped { name, was } => {
                    let reason = manifest
                        .skip_reason(&name)
                        .unwrap_or("no reason recorded")
                        .to_owned();
                    comparison.uncovered.push(format!(
                        "{name}: recorded at {was}; not run here — {reason}"
                    ));
                }
                Change::Started { name, version } => comparison.unrecorded.push(format!(
                    "{name}: ran at {version}, and {RECORD_NAME} has no version for it",
                )),
            }
        }
        comparison
    }
}

/// A run's manifest, held against the record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Harnesses that ran at a version the record does not name.
    pub drifted: Vec<String>,
    /// Harnesses the record has a version for that this run did not run.
    pub uncovered: Vec<String>,
    /// Harnesses that ran with no recorded version at all.
    pub unrecorded: Vec<String>,
}

impl Comparison {
    /// Did every harness that ran match the record?
    ///
    /// Uncovered harnesses do not count against this: a laptop without `codex`
    /// installed has not drifted from anything.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.drifted.is_empty() && self.unrecorded.is_empty()
    }

    /// The comparison as a printable section.
    ///
    /// Printed by every run, clean or not, for the reason the skip table is:
    /// "no drift" is a result, and a section that appears only when something is
    /// wrong is indistinguishable from a section nobody wired up.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from("--- drift from the version record ---\n");
        if self.is_clean() {
            out.push_str("  none: every harness that ran matches the recorded version\n");
        }
        for line in &self.drifted {
            out.push_str(&format!("  DRIFT  {line}\n"));
        }
        for line in &self.unrecorded {
            out.push_str(&format!("  NEW    {line}\n"));
        }
        for line in &self.uncovered {
            out.push_str(&format!("  ----   {line}\n"));
        }
        out.push_str(
            "  (drift is stated, not failed: the record moves through a reviewed pull request)\n",
        );
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::manifest::Status;

    fn entry(name: &str, version: Option<&str>) -> RecordEntry {
        RecordEntry {
            name: name.to_owned(),
            version: version.map(ToOwned::to_owned),
            upstream_version: version.map(ToOwned::to_owned),
            discovery: Discovery {
                kind: if version.is_some() {
                    DiscoveryKind::Npm
                } else {
                    DiscoveryKind::Unwatched
                },
                source: version.is_some().then(|| "a-package".to_owned()),
                note: "a note".to_owned(),
            },
        }
    }

    fn record(entries: Vec<RecordEntry>) -> Record {
        Record {
            schema: SCHEMA_VERSION,
            harnesses: entries,
        }
    }

    fn ran(version: &str) -> Status {
        Status::Ran {
            version: version.to_owned(),
            path: PathBuf::from("/usr/bin/x"),
        }
    }

    fn write(text: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(text.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    /// The committed record loads, covers every registry harness, and every
    /// watched entry carries both versions and a source.
    ///
    /// The one test here that reads the real file: a record that does not parse
    /// makes every matrix run fail, and a record missing a harness makes the
    /// watch quietly narrower than it claims to be.
    #[test]
    fn the_committed_record_is_complete() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(RECORD_NAME);
        let record = Record::load(&path).expect("the committed record must load");

        for harness in tapes_harnesses::harness::REGISTRY {
            let entry = record
                .entry(harness.id())
                .unwrap_or_else(|| panic!("{} has no entry in {RECORD_NAME}", harness.id()));
            assert!(
                !entry.discovery.note.trim().is_empty(),
                "{}'s entry must explain its discovery source",
                harness.id(),
            );
            if entry.is_watched() {
                assert!(
                    entry.discovery.source.is_some(),
                    "{} is watched, so it needs a source to watch",
                    harness.id(),
                );
                assert!(
                    entry.version.is_some() && entry.upstream_version.is_some(),
                    "{} is watched, so it needs both a printed and an upstream version",
                    harness.id(),
                );
            }
        }
    }

    /// A moved version is drift, and the line says which side is which.
    #[test]
    fn a_moved_version_is_reported_as_drift() {
        let record = record(vec![entry("claude", Some("2.1.220 (Claude Code)"))]);
        let mut manifest = VersionManifest::new();
        manifest.record_harness("claude", ran("2.1.229 (Claude Code)"));

        let comparison = record.compare(&manifest);
        assert!(!comparison.is_clean());
        assert_eq!(comparison.drifted.len(), 1);
        assert!(comparison.drifted[0].contains("the record passed against 2.1.220 (Claude Code)"));
        assert!(comparison.drifted[0].contains("this run ran 2.1.229 (Claude Code)"));
    }

    /// A harness the record knows and this run did not run is *not* drift — it
    /// is a machine without that harness — and it is reported with the run's own
    /// skip reason rather than being dropped.
    #[test]
    fn a_harness_this_run_did_not_have_is_uncovered_not_drift() {
        let record = record(vec![entry("pi", Some("0.80.10"))]);
        let mut manifest = VersionManifest::new();
        manifest.record_harness(
            "pi",
            Status::Skipped {
                reason: "pi is not on PATH".to_owned(),
            },
        );

        let comparison = record.compare(&manifest);
        assert!(comparison.is_clean(), "an absent harness has not drifted");
        assert_eq!(comparison.drifted, Vec::<String>::new());
        assert_eq!(comparison.uncovered.len(), 1);
        assert!(comparison.uncovered[0].contains("pi is not on PATH"));
    }

    /// A harness that ran with no recorded version is called out, because that
    /// is how a harness joins the matrix without joining the watch.
    #[test]
    fn a_harness_with_no_record_entry_is_called_out() {
        let record = record(vec![]);
        let mut manifest = VersionManifest::new();
        manifest.record_harness("newcomer", ran("0.1.0"));

        let comparison = record.compare(&manifest);
        assert!(!comparison.is_clean());
        assert_eq!(comparison.unrecorded.len(), 1);
        assert!(comparison.unrecorded[0].contains("newcomer"));
    }

    /// The client CLIs are not part of this comparison. A developer who supplied
    /// a client binary must not be told the client is unrecorded on every run.
    #[test]
    fn client_clis_are_not_compared_against_the_record() {
        let record = record(vec![entry("claude", Some("2.1.220 (Claude Code)"))]);
        let mut manifest = VersionManifest::new();
        manifest.record_harness("claude", ran("2.1.220 (Claude Code)"));
        manifest.record_cli("tapesctl", ran("0.3.1"));

        assert_eq!(record.compare(&manifest), Comparison::default());
    }

    /// A clean comparison still prints a section, so a reader can tell "no
    /// drift" from "nobody wired this up".
    #[test]
    fn a_clean_comparison_still_prints_a_section() {
        let rendered = Comparison::default().render();
        assert!(rendered.contains("drift from the version record"));
        assert!(rendered.contains("none:"));
    }

    /// An unknown schema refuses by name rather than parsing what it can.
    #[test]
    fn a_record_from_another_schema_is_refused() {
        let file = write(r#"{"schema": 99, "harnesses": []}"#);
        let error = Record::load(file.path()).expect_err("schema 99 must not load");
        assert!(error.to_string().contains("schema 99"), "{error}");
    }

    /// So does an unknown discovery kind: a watch that cannot run is not a watch
    /// that silently covers nothing.
    #[test]
    fn an_unknown_discovery_kind_is_refused() {
        let file = write(
            r#"{"schema": 1, "harnesses": [{"name": "x", "version": null,
                "upstream_version": null,
                "discovery": {"kind": "carrier_pigeon", "source": null, "note": "n"}}]}"#,
        );
        let error = Record::load(file.path()).expect_err("an unknown kind must not load");
        assert!(error.to_string().contains("could not be parsed"), "{error}");
    }

    /// An absent record is an error naming the path, not an empty record that
    /// would report every harness as unrecorded.
    #[test]
    fn an_absent_record_is_an_error_naming_the_path() {
        let error = Record::load(Path::new("/nonexistent/harness-versions.json"))
            .expect_err("an absent record must not load");
        assert!(
            error.to_string().contains("harness-versions.json"),
            "{error}"
        );
    }

    /// The record round-trips, which is what lets the watcher rewrite one field
    /// with `jq` and leave the rest byte-identical.
    #[test]
    fn a_record_round_trips_through_json() {
        let original = record(vec![
            entry("claude", Some("2.1.220")),
            entry("codex-app", None),
        ]);
        let text = serde_json::to_string_pretty(&original).unwrap();
        let parsed: Record = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, original);
    }
}
