//! The mock ingest's envelope reader, checked case by case against the shared
//! fixture corpus.
//!
//! `tapes-capture` runs the *producer* side of this corpus: given an envelope,
//! does it emit the right headers. This file runs the reader side from the
//! matrix's point of view: given those headers, does the mock ingest recover the
//! envelope every other reader recovers.
//!
//! Without this, the matrix's central assertion would be circular. "Launched
//! implies attributed" is only worth asserting if "attributed" means what ingest
//! means by it — a mock with a lenient parser of its own would happily report
//! attribution that the real system rejects, and the matrix would be green over
//! exactly the composition failure it was built to catch.
//!
//! Every case is asserted, in every direction. A `decode` case is a malformed
//! input a well-behaved producer never emits, which is precisely the input a
//! forged or half-configured envelope looks like — so those are the cases the
//! refusal assertions depend on, not ones to skip.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use tapes_capture::envelope::fixtures::{self, FixtureCase};
use tapes_mock_upstream::ingest::{HARNESS_ID_UNKNOWN, read_envelope};

/// The `x-tapes-*` half of a case's headers — the only half a client sends.
/// The `x-paper-auth-*` headers in the corpus are server-trusted and are not a
/// producer's to emit, so a reader must not be handed them here.
fn tapes_headers(case: &FixtureCase) -> BTreeMap<String, String> {
    case.expected_tapes_headers()
}

/// Every case: reading the headers recovers the case's envelope.
///
/// This holds across all three directions, which is worth stating because it is
/// not obvious. A lossy `encode` case's `headers` already carry the truncated or
/// dropped value, and its `envelope` is what a reader gets back from them — the
/// pre-loss input lives in `encode_from` and is the producer's problem, not
/// this side's.
#[test]
fn every_corpus_case_reads_back_to_its_envelope() {
    let cases = fixtures::load_cases();
    assert!(!cases.is_empty(), "the vendored corpus is empty");

    for case in &cases {
        let read = read_envelope(&tapes_headers(case));
        let expected = &case.envelope;

        let expected_id = expected
            .harness_id
            .clone()
            .unwrap_or_else(|| HARNESS_ID_UNKNOWN.to_owned());
        assert_eq!(read.harness_id, expected_id, "{}: harness_id", case.name,);
        assert_eq!(
            read.harness_session_id, expected.harness_session_id,
            "{}: harness_session_id",
            case.name,
        );
        assert_eq!(
            read.harness_version, expected.harness_version,
            "{}: harness_version",
            case.name,
        );
        assert_eq!(read.cwd, expected.cwd, "{}: cwd", case.name);
        assert_eq!(read.name, expected.name, "{}: name", case.name);
        assert_eq!(
            read.parent_harness_session_id, expected.parent_harness_session_id,
            "{}: parent_harness_session_id",
            case.name,
        );
        assert_eq!(
            read.harness_metadata, expected.harness_metadata,
            "{}: harness_metadata",
            case.name,
        );
    }
}

/// The corpus covers the unknown sentinel, and the reader must agree with it —
/// this is the value every refusal assertion in the matrix checks for.
#[test]
fn the_unknown_cases_read_as_the_unknown_sentinel() {
    let cases = fixtures::load_cases();
    let unknown: Vec<&FixtureCase> = cases
        .iter()
        .filter(|case| case.name.starts_with("unknown-"))
        .collect();
    assert!(
        !unknown.is_empty(),
        "the corpus should carry unknown-harness cases",
    );

    for case in unknown {
        let read = read_envelope(&tapes_headers(case));
        assert_eq!(
            read.harness_id, HARNESS_ID_UNKNOWN,
            "{}: an unknown-harness case must read as the sentinel",
            case.name,
        );
        assert!(
            !read.is_attributed(),
            "{}: an unknown-harness case must not count as attributed",
            case.name,
        );
    }
}

/// Every case whose envelope names a harness *and* a session id counts as
/// attributed, and no other case does. This is the exact predicate the matrix's
/// launched-implies-attributed assertion rests on, so it is pinned against the
/// corpus rather than against hand-written examples.
#[test]
fn the_attribution_predicate_matches_the_corpus() {
    for case in &fixtures::load_cases() {
        let read = read_envelope(&tapes_headers(case));
        let should_be_attributed = case
            .envelope
            .harness_id
            .as_deref()
            .is_some_and(|id| id != HARNESS_ID_UNKNOWN)
            && case.envelope.harness_session_id.is_some();

        assert_eq!(
            read.is_attributed(),
            should_be_attributed,
            "{}: attribution predicate disagrees with the corpus",
            case.name,
        );
    }
}

/// A partial envelope — a harness id with no session id — never counts as
/// attributed. The corpus carries this shape (`codex-bare`, `unknown-bare`) and
/// it is the shape a forged claim produces, so it gets its own assertion rather
/// than riding on the sweep above.
#[test]
fn a_partial_envelope_is_never_attributed() {
    let cases = fixtures::load_cases();
    let partial: Vec<&FixtureCase> = cases
        .iter()
        .filter(|case| {
            case.envelope.harness_id.is_some() && case.envelope.harness_session_id.is_none()
        })
        .collect();
    assert!(
        !partial.is_empty(),
        "the corpus should carry at least one id-without-session case",
    );

    for case in partial {
        assert!(
            !read_envelope(&tapes_headers(case)).is_attributed(),
            "{}: an envelope with no session id must not be attributed",
            case.name,
        );
    }
}
