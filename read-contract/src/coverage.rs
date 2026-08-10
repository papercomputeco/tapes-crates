//! The operation-coverage gate.
//!
//! Vendoring a contract invites a quieter failure than drift: an operation the
//! server grows that a client silently never exposes. The gate partitions every
//! `operationId` in the vendored document into exposed and deliberately
//! unexposed, and fails — naming the unmapped ids — the moment a contract bump
//! adds one that is in neither list.
//!
//! # The tables stay with the consumer
//!
//! Only the *mechanism* lives here. `EXPOSED` / `UNEXPOSED` are a statement
//! about one client's surface: a CLI that authors skills locally deliberately
//! does not expose a server-side skills store, and a client that does would
//! have to. Sharing the tables would make the gate report on the union of two
//! surfaces and silently stop protecting whichever client differs — which is
//! precisely the failure the gate exists to prevent, reintroduced one layer up.
//!
//! # Usage
//!
//! Each consumer keeps its own tables and calls [`check`] from a test:
//!
//! ```no_run
//! # use tapes_read_contract::coverage;
//! const EXPOSED: &[(&str, &str)] = &[("listSessions", "sessions list")];
//! const UNEXPOSED: &[(&str, &str)] = &[("ping", "liveness probe; no CLI health verb asked for")];
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! coverage::check(EXPOSED, UNEXPOSED)?;
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeSet;

use crate::contract::core;
use crate::error::Result;

/// A coverage table: `operationId` paired with prose for the reviewer and the
/// failure message.
pub type Table<'a> = &'a [(&'a str, &'a str)];

/// What a coverage check found wrong.
///
/// Rendered as a single message naming every offending id, because a gate that
/// reports one failure at a time turns a contract bump into a sequence of runs.
#[derive(Debug, PartialEq, Eq)]
pub struct CoverageReport {
    /// Ids in the contract that appear in neither table.
    pub unmapped: Vec<String>,
    /// Ids in a table that the contract does not have.
    pub stale: Vec<String>,
    /// Ids that appear in both tables.
    pub contradictory: Vec<String>,
}

impl CoverageReport {
    /// Whether the tables and the contract agree.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.unmapped.is_empty() && self.stale.is_empty() && self.contradictory.is_empty()
    }
}

impl std::fmt::Display for CoverageReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.unmapped.is_empty() {
            write!(
                f,
                "operations in the vendored tapes-api contract that this client neither exposes \
                 nor allow-lists: {:?} — add each to the exposed table (and wire it up) or to the \
                 unexposed table with the reason it stays unexposed. ",
                self.unmapped,
            )?;
        }
        if !self.stale.is_empty() {
            write!(
                f,
                "operations named by a coverage table that the vendored tapes-api contract does \
                 not have: {:?} — the contract dropped or renamed them, and the mapping must move \
                 in the same change. ",
                self.stale,
            )?;
        }
        if !self.contradictory.is_empty() {
            write!(
                f,
                "operations in both coverage tables: {:?}. ",
                self.contradictory,
            )?;
        }
        Ok(())
    }
}

/// Compare a consumer's coverage tables against the vendored contract.
///
/// Returns the report whether or not it is clean; [`check`] is the assertion
/// form.
pub fn report(exposed: Table<'_>, unexposed: Table<'_>) -> Result<CoverageReport> {
    let surface = core()?;
    let known: BTreeSet<&str> = surface.operation_ids().collect();
    let exposed_ids: BTreeSet<&str> = exposed.iter().map(|(id, _)| *id).collect();
    let unexposed_ids: BTreeSet<&str> = unexposed.iter().map(|(id, _)| *id).collect();

    let owned =
        |ids: BTreeSet<&str>| -> Vec<String> { ids.into_iter().map(ToOwned::to_owned).collect() };

    Ok(CoverageReport {
        unmapped: owned(
            known
                .iter()
                .filter(|id| !exposed_ids.contains(*id) && !unexposed_ids.contains(*id))
                .copied()
                .collect(),
        ),
        stale: owned(
            exposed_ids
                .union(&unexposed_ids)
                .filter(|id| !known.contains(*id))
                .copied()
                .collect(),
        ),
        contradictory: owned(exposed_ids.intersection(&unexposed_ids).copied().collect()),
    })
}

/// The assertion form of [`report`]: `Ok(())` when the tables and the contract
/// agree, and the rendered report as an error otherwise.
///
/// Returns a `String` error rather than this crate's [`crate::Error`] because
/// its only caller is a test assertion, and the message is the whole value.
pub fn check(exposed: Table<'_>, unexposed: Table<'_>) -> std::result::Result<(), String> {
    let report = report(exposed, unexposed).map_err(|e| e.to_string())?;
    if report.is_clean() {
        return Ok(());
    }
    Err(report.to_string())
}

/// Every `operationId` in the vendored contract, sorted.
///
/// The input a consumer's own gate reads; exposed as a convenience so a
/// consumer that wants a different check than [`check`] does not have to reach
/// through [`crate::contract::core`].
pub fn operation_ids() -> Result<Vec<String>> {
    let mut ids: Vec<String> = core()?.operation_ids().map(ToOwned::to_owned).collect();
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::ops;

    #[test]
    fn the_contract_has_operations_to_gate() {
        let ids = operation_ids().unwrap();
        assert!(ids.contains(&ops::LIST_SESSIONS.to_owned()), "got: {ids:?}");
    }

    #[test]
    fn an_operation_in_neither_table_is_reported_as_unmapped() {
        // The gate's whole purpose: a contract bump that adds an operation
        // must fail a consumer's build until somebody decides about it.
        let report = report(&[(ops::LIST_SESSIONS, "sessions list")], &[]).unwrap();
        assert!(!report.is_clean());
        assert!(
            report.unmapped.contains(&ops::GET_SESSION.to_owned()),
            "got: {report:?}",
        );
        assert!(
            report.to_string().contains("neither exposes"),
            "got: {report}",
        );
    }

    #[test]
    fn a_table_entry_the_contract_does_not_have_is_reported_as_stale() {
        // A dropped or renamed operation; the mapping must move in the same
        // change rather than sit pointing at nothing.
        let report = report(&[("launchMissiles", "nowhere")], &[]).unwrap();
        assert_eq!(report.stale, vec!["launchMissiles".to_owned()]);
    }

    #[test]
    fn an_operation_in_both_tables_is_reported_as_contradictory() {
        let report = report(
            &[(ops::LIST_SESSIONS, "sessions list")],
            &[(ops::LIST_SESSIONS, "also here, somehow")],
        )
        .unwrap();
        assert_eq!(report.contradictory, vec![ops::LIST_SESSIONS.to_owned()]);
    }

    #[test]
    fn a_complete_partition_is_clean() {
        // Build the tables from the contract itself: this asserts the
        // mechanism, not any particular client's surface.
        let ids = operation_ids().unwrap();
        let exposed: Vec<(&str, &str)> = ids.iter().map(|id| (id.as_str(), "exposed")).collect();
        assert_eq!(check(&exposed, &[]), Ok(()));
    }

    #[test]
    fn the_failure_names_every_offending_id_at_once() {
        // A gate that reported one at a time would turn a contract bump into
        // a sequence of runs.
        let err = check(&[], &[]).unwrap_err();
        assert!(err.contains(ops::LIST_SESSIONS), "got: {err}");
        assert!(err.contains(ops::GET_SESSION), "got: {err}");
    }
}
