//! The registry completeness gate, wired to the real registry and the real
//! repository files.
//!
//! The registry is the source of truth and most coverage *derives* from it —
//! the matrix iterates `REGISTRY`, and the recipe table is invariant-tested
//! against it in both directions. What nothing gated until now is the
//! hand-maintained far side: the version record, the shipped asset files, and
//! the documentation. A harness can be declared, compile, pass every unit
//! test, and still be invisible to the drift watch, uninstallable, and absent
//! from the docs. This test walks the registry and fails, per harness, with
//! the exact edit that is missing — see `src/completeness.rs` for the legs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tapes_harnesses::harness::{self, Harness, PluginDelivery};
use tapes_mock_upstream::completeness::{
    ArtifactClaim, AssetEvidence, Evidence, HarnessClaims, check,
};
use tapes_mock_upstream::recipe::RECIPES;
use tapes_mock_upstream::record::Record;

/// The repository root, resolved from this crate's manifest directory the same
/// way `Record::default_path` resolves the record: independent of whether the
/// invocation's working directory is the crate or the repository.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Where a harness's plugin assets live by convention, per
/// `docs/adding-a-harness.md`: `crates/tapes-harnesses/assets/<harness>/`.
fn asset_path(harness_id: &str, file_name: &str) -> PathBuf {
    repo_root()
        .join("crates/tapes-harnesses/assets")
        .join(harness_id)
        .join(file_name)
}

/// Flatten one registry harness into checkable claims, gathering the on-disk
/// asset evidence for each artifact it hands out.
fn claims_for(entry: &Harness) -> HarnessClaims {
    let artifacts = entry
        .plugin_artifacts()
        .iter()
        .map(|artifact| {
            let path = asset_path(entry.id(), artifact.file_name());
            let rel = format!(
                "crates/tapes-harnesses/assets/{}/{}",
                entry.id(),
                artifact.file_name(),
            );
            let asset = match std::fs::read_to_string(&path) {
                Ok(on_disk) if on_disk == artifact.contents() => AssetEvidence::Matches,
                Ok(_) => AssetEvidence::Diverged { path: rel },
                Err(_) => AssetEvidence::Missing { expected: rel },
            };
            ArtifactClaim {
                file_name: artifact.file_name().to_owned(),
                asset,
            }
        })
        .collect();

    HarnessClaims {
        id: entry.id().to_owned(),
        bundled_extension: matches!(entry.plugin(), PluginDelivery::BundledExtension(_)),
        artifacts,
    }
}

/// Every registry harness clears all three legs: matrix coverage (recipe +
/// version record), plugin assets where the registry claims them, and a named
/// row in the docs. A failure message is the edit to make.
#[test]
fn every_registry_harness_is_complete() {
    let record = Record::load(&Record::default_path()).expect("harness-versions.json must load");
    let docs = std::fs::read_to_string(repo_root().join("docs/harness-matrix.md"))
        .expect("docs/harness-matrix.md must exist");

    let evidence = Evidence {
        recipe_ids: RECIPES
            .iter()
            .map(|recipe| recipe.harness_id.to_owned())
            .collect(),
        record_names: record
            .harnesses
            .iter()
            .map(|entry| entry.name.clone())
            .collect(),
        docs,
    };

    let claims: Vec<HarnessClaims> = harness::REGISTRY.iter().map(claims_for).collect();
    let failures = check(&claims, &evidence);
    assert!(
        failures.is_empty(),
        "{} registry completeness failure(s):\n  {}",
        failures.len(),
        failures.join("\n  "),
    );
}

/// The reverse direction: the hand-maintained surfaces name no harness the
/// registry does not have. A record or recipe entry for a retired id is how a
/// gate goes on asserting facts about nothing.
#[test]
fn no_stale_names_outside_the_registry() {
    let registry_ids: BTreeSet<&str> = harness::REGISTRY.iter().map(Harness::id).collect();

    let record = Record::load(&Record::default_path()).expect("harness-versions.json must load");
    for entry in &record.harnesses {
        assert!(
            registry_ids.contains(entry.name.as_str()),
            "harness-versions.json names {:?}, which is not in the registry — remove the entry \
             or restore the harness",
            entry.name,
        );
    }
    for recipe in RECIPES {
        assert!(
            registry_ids.contains(recipe.harness_id),
            "RECIPES names {:?}, which is not in the registry",
            recipe.harness_id,
        );
    }
}
