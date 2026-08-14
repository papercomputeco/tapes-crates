//! Registry completeness: the gates a new harness must clear beyond compiling.
//!
//! # The gap this closes
//!
//! The registry is the source of truth, and most of what hangs off it is
//! *derived*: the matrix iterates `REGISTRY` directly, and the recipe table is
//! invariant-tested against it in both directions — so a harness cannot land
//! in the registry and quietly acquire no matrix row. What that machinery does
//! **not** gate is everything hand-maintained on the far side of the
//! derivation: the version record a matrix run compares itself against, the
//! asset files a `BundledExtension` claims to ship, and the documentation that
//! tells a reader the harness exists at all. A harness added without those is
//! a registry entry that compiles, passes `cargo test`, and ships incomplete —
//! unrecorded versions, an installer with nothing to install, docs that still
//! say "five harnesses" when there are six.
//!
//! # The three legs, per registry harness
//!
//! 1. **Matrix coverage.** A one-shot recipe exists in [`crate::recipe::RECIPES`]
//!    (restated here so the completeness report is one list, though the recipe
//!    invariant tests also fail without it), and `harness-versions.json` has an
//!    entry — the record is what a run's versions are compared against, and a
//!    harness absent from it can drift forever without the watch noticing.
//! 2. **Plugin assets.** A harness whose capture is delivered as a bundled
//!    extension actually carries artifacts, and each artifact's bytes exist as
//!    a source asset where the repository's convention says they live
//!    (`crates/tapes-harnesses/assets/<harness>/<file>`), matching the embedded
//!    contents byte for byte.
//! 3. **Docs presence.** `docs/harness-matrix.md` names the harness, so the
//!    honest table stays honest when the registry grows.
//!
//! The checker is pure over its inputs — a caller supplies the claims and the
//! evidence — which is what lets its own failure modes be tested with a fake
//! registry entry missing each leg. The integration test in
//! `tests/registry_completeness.rs` wires it to the real registry and the real
//! repository files.

use std::collections::BTreeSet;

/// What one registry harness claims, flattened for checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessClaims {
    /// The registry harness id.
    pub id: String,
    /// Whether the registry delivers this harness's capture as a bundled
    /// extension — the delivery that promises artifacts.
    pub bundled_extension: bool,
    /// The artifacts the registry hands out for this harness, with the
    /// evidence of their in-repository source assets.
    pub artifacts: Vec<ArtifactClaim>,
}

/// One claimed plugin artifact and the state of its source asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactClaim {
    /// The file name the artifact installs as.
    pub file_name: String,
    /// What the repository holds at the artifact's conventional asset path.
    pub asset: AssetEvidence,
}

/// The state of an artifact's source asset on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetEvidence {
    /// The asset file exists and its bytes equal the embedded contents.
    Matches,
    /// No file exists at the conventional path. `expected` is that path,
    /// relative to the repository root, so the failure says where to put it.
    Missing {
        /// The path the asset was expected at.
        expected: String,
    },
    /// A file exists at `path` but its bytes differ from what the registry
    /// embeds — a stale copy, or an embed that reads from somewhere else.
    Diverged {
        /// The path holding the diverging bytes.
        path: String,
    },
}

/// The hand-maintained surfaces the registry must be reflected in.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    /// Harness ids the one-shot recipe table covers.
    pub recipe_ids: BTreeSet<String>,
    /// Harness names present in `harness-versions.json`.
    pub record_names: BTreeSet<String>,
    /// The full text of `docs/harness-matrix.md`.
    pub docs: String,
}

/// Check every claim against the evidence, returning one message per failure.
///
/// An empty result is completeness. Each message names the harness, the leg it
/// is missing, and exactly what to add — the point is that the person who just
/// declared a harness reads the failure and knows the edit, not that they go
/// find the reviewer who knows.
#[must_use]
pub fn check(claims: &[HarnessClaims], evidence: &Evidence) -> Vec<String> {
    let mut failures = Vec::new();
    for harness in claims {
        let id = &harness.id;

        // Leg 1a: a matrix row needs a recipe. The recipe table's own
        // invariant tests fail too; repeating it here keeps the completeness
        // report a single checklist.
        if !evidence.recipe_ids.contains(id) {
            failures.push(format!(
                "{id}: no one-shot recipe — add a OneShotRecipe to RECIPES in \
                 crates/tapes-mock-upstream/src/recipe.rs (an unlaunchable harness still gets an \
                 entry with an `unsupported` reason, so its cell skips visibly)",
            ));
        }

        // Leg 1b: the version record. Without an entry, the matrix has no
        // baseline for the harness and the drift watch never covers it.
        if !evidence.record_names.contains(id) {
            failures.push(format!(
                "{id}: no entry in harness-versions.json — add one recording the version the \
                 matrix passed against and how to discover upstream's (or an `unwatched` entry \
                 with its reason, if it cannot be watched)",
            ));
        }

        // Leg 2: claimed plugin assets exist where the registry claims.
        if harness.bundled_extension && harness.artifacts.is_empty() {
            failures.push(format!(
                "{id}: PluginDelivery::BundledExtension carries no artifacts — declare its \
                 PluginArtifact(s) in crates/tapes-harnesses/src/plugin.rs, or change the \
                 delivery to one that ships nothing",
            ));
        }
        for artifact in &harness.artifacts {
            match &artifact.asset {
                AssetEvidence::Matches => {}
                AssetEvidence::Missing { expected } => failures.push(format!(
                    "{id}: plugin artifact `{}` has no source asset at {expected} — the \
                     registry claims bytes the repository does not ship at its conventional \
                     path (crates/tapes-harnesses/assets/<harness>/<file>)",
                    artifact.file_name,
                )),
                AssetEvidence::Diverged { path } => failures.push(format!(
                    "{id}: plugin artifact `{}` differs from {path} — the embedded contents \
                     and the on-disk asset must be the same bytes",
                    artifact.file_name,
                )),
            }
        }

        // Leg 3: docs presence, matched as the backticked id so prose that
        // merely mentions a similar word cannot satisfy it.
        if !evidence.docs.contains(&format!("`{id}`")) {
            failures.push(format!(
                "{id}: docs/harness-matrix.md does not name it — add its row to the honest \
                 table (what runs where, and what still skips)",
            ));
        }
    }
    failures
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A claims entry that satisfies every leg against [`evidence`].
    fn complete_claims(id: &str) -> HarnessClaims {
        HarnessClaims {
            id: id.to_owned(),
            bundled_extension: true,
            artifacts: vec![ArtifactClaim {
                file_name: "tapes-gateway.ts".to_owned(),
                asset: AssetEvidence::Matches,
            }],
        }
    }

    /// Evidence in which `id` is fully present.
    fn evidence(id: &str) -> Evidence {
        Evidence {
            recipe_ids: [id.to_owned()].into_iter().collect(),
            record_names: [id.to_owned()].into_iter().collect(),
            docs: format!("| `{id}` | runs | runs | note |"),
        }
    }

    #[test]
    fn a_complete_harness_reports_nothing() {
        let failures = check(&[complete_claims("gemini")], &evidence("gemini"));
        assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    }

    /// Each leg, removed alone, fails alone — and the message names the exact
    /// edit, because that is the whole point of the gate.
    #[test]
    fn a_missing_recipe_fails_naming_the_recipe_table() {
        let mut ev = evidence("gemini");
        ev.recipe_ids.clear();
        let failures = check(&[complete_claims("gemini")], &ev);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("gemini"));
        assert!(failures[0].contains("OneShotRecipe"));
        assert!(failures[0].contains("recipe.rs"));
    }

    #[test]
    fn a_missing_record_entry_fails_naming_the_record() {
        let mut ev = evidence("gemini");
        ev.record_names.clear();
        let failures = check(&[complete_claims("gemini")], &ev);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("harness-versions.json"));
        // The record accepts an honest "cannot be watched" entry; the message
        // must say so, or an unwatchable harness reads this as a dead end.
        assert!(failures[0].contains("unwatched"));
    }

    #[test]
    fn a_missing_docs_row_fails_naming_the_docs_file() {
        let mut ev = evidence("gemini");
        ev.docs = "| `claude` | runs |".to_owned();
        let failures = check(&[complete_claims("gemini")], &ev);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("docs/harness-matrix.md"));
    }

    /// Docs matching is on the backticked id: prose containing the bare word
    /// does not count as a row.
    #[test]
    fn a_bare_word_mention_is_not_docs_presence() {
        let mut ev = evidence("gemini");
        ev.docs = "gemini is coming soon".to_owned();
        let failures = check(&[complete_claims("gemini")], &ev);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("docs/harness-matrix.md"));
    }

    #[test]
    fn a_bundled_extension_with_no_artifacts_fails() {
        let mut claims = complete_claims("gemini");
        claims.artifacts.clear();
        let failures = check(&[claims], &evidence("gemini"));
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("BundledExtension"));
        assert!(failures[0].contains("plugin.rs"));
    }

    #[test]
    fn a_missing_asset_fails_naming_the_expected_path() {
        let mut claims = complete_claims("gemini");
        claims.artifacts[0].asset = AssetEvidence::Missing {
            expected: "crates/tapes-harnesses/assets/gemini/tapes-gateway.ts".to_owned(),
        };
        let failures = check(&[claims], &evidence("gemini"));
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("assets/gemini/tapes-gateway.ts"));
    }

    #[test]
    fn a_diverged_asset_fails_naming_the_file() {
        let mut claims = complete_claims("gemini");
        claims.artifacts[0].asset = AssetEvidence::Diverged {
            path: "crates/tapes-harnesses/assets/gemini/tapes-gateway.ts".to_owned(),
        };
        let failures = check(&[claims], &evidence("gemini"));
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("differs from"));
    }

    /// A harness missing everything reports every leg at once — one run, the
    /// whole checklist, rather than a fix-rerun loop.
    #[test]
    fn every_missing_leg_is_reported_together() {
        let claims = HarnessClaims {
            id: "gemini".to_owned(),
            bundled_extension: true,
            artifacts: Vec::new(),
        };
        let failures = check(&[claims], &Evidence::default());
        assert_eq!(failures.len(), 4, "{failures:?}");
    }

    /// A harness that ships no plugin owes no assets: absence of artifacts is
    /// the ordinary case, not a violation.
    #[test]
    fn a_pluginless_harness_owes_no_assets() {
        let claims = HarnessClaims {
            id: "gemini".to_owned(),
            bundled_extension: false,
            artifacts: Vec::new(),
        };
        let failures = check(&[claims], &evidence("gemini"));
        assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    }
}
