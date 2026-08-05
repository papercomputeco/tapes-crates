//! The shared envelope fixture corpus, read the way a consumer reads it.
//!
//! `envelope::fixtures` exists so a capture client can table-test its own
//! envelope composition against the same cases this crate does. That claim is
//! only true if the module is reachable and usable from *another* crate — the
//! in-crate oracle proves nothing about it, since it could reach a private
//! module just as easily.
//!
//! An integration test is a separate crate compiled against the public API, so
//! it is subject to exactly the rules tapesctl and paperd are. What this file
//! does, they can do.
//!
//! Requires the `envelope-fixtures` feature; without it the corpus reader is
//! not compiled and this file is empty.

#![cfg(feature = "envelope-fixtures")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tapes_harnesses::envelope::fixtures::{
    Direction, attribution_from, decode_metadata, load_cases, tapes_headers,
};
use tapes_harnesses::envelope::{
    HARNESS_ID_UNKNOWN, TapesAttribution, X_TAPES_HARNESS_METADATA, inject_tapes_attribution,
};

/// The corpus is reachable from outside the crate, and it is the real corpus —
/// not an empty directory a consumer would mistake for a passing table test.
#[test]
fn a_consumer_can_load_the_corpus() {
    let cases = load_cases();
    assert!(
        cases.len() >= 15,
        "only {} cases loaded from {}; the vendored corpus looks truncated",
        cases.len(),
        tapes_harnesses::envelope::fixtures::cases_dir().display(),
    );
    assert!(
        cases
            .iter()
            .any(|case| case.direction() == Direction::Decode),
        "the corpus should carry parser-only cases a producer skips",
    );
}

/// The composition a consumer actually needs to check: build an attribution,
/// emit an envelope, compare against the corpus. This is the loop a capture
/// client writes over its own composition path — here it runs against the
/// crate's own producer so the helpers are exercised end to end.
#[test]
fn a_consumer_can_table_test_envelope_composition_against_the_corpus() {
    let mut produced = 0_usize;

    for case in load_cases() {
        // Direction is the case's own declaration; a consumer never keeps a
        // hardcoded skip list.
        if case.direction() == Direction::Decode {
            continue;
        }

        let mut headers = http::HeaderMap::new();
        inject_tapes_attribution(&mut headers, attribution_from(case.logical_envelope()))
            .unwrap_or_else(|e| panic!("{}: inject failed: {e:?}", case.name));

        let got = tapes_headers(&headers);
        let want = case.expected_tapes_headers();
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>(),
            "{}: emitted header set does not match the fixture",
            case.name,
        );

        for (name, want_value) in &want {
            if name == X_TAPES_HARNESS_METADATA {
                // Compared as decoded JSON: key ordering is not contractual.
                assert_eq!(
                    decode_metadata(&got[name]),
                    decode_metadata(want_value),
                    "{}: {name} decodes to different JSON",
                    case.name,
                );
            } else {
                assert_eq!(&got[name], want_value, "{}: {name}", case.name);
            }
        }
        produced += 1;
    }

    assert!(
        produced >= 10,
        "only {produced} cases exercised the producer"
    );
}

/// The other direction a consumer needs: reading an inbound envelope back.
///
/// Every corpus case whose headers carry a complete envelope must read back
/// through [`TapesAttribution::from_headers`] with the harness id and session
/// id the case declares. This is the assertion that catches the bug the
/// readback path exists to prevent — headers that say `pi` producing a row
/// that says `unknown`.
#[test]
fn complete_corpus_envelopes_read_back_through_from_headers() {
    let mut checked = 0_usize;

    for case in load_cases() {
        let mut headers = http::HeaderMap::new();
        for (name, value) in &case.headers {
            // A case may deliberately carry a header an HTTP stack rejects;
            // those are parser fixtures, not readback ones.
            let (Ok(name), Ok(value)) = (
                http::HeaderName::from_bytes(name.as_bytes()),
                http::HeaderValue::from_str(value),
            ) else {
                continue;
            };
            headers.insert(name, value);
        }

        let declared_id = case.envelope.harness_id.as_deref().unwrap_or_default();
        let declared_sid = case.envelope.harness_session_id.as_deref().unwrap_or("");
        let expect_readable = !declared_id.is_empty()
            && declared_id != HARNESS_ID_UNKNOWN
            && !declared_sid.is_empty()
            // Only when the headers themselves actually survived construction
            // and carry the pair; some cases declare an envelope the headers
            // deliberately fail to state.
            && tapes_headers(&headers).contains_key("x-tapes-harness-session-id");

        match TapesAttribution::from_headers(&headers) {
            Some(read) if expect_readable => {
                assert_eq!(read.harness_id, declared_id, "{}", case.name);
                assert_eq!(
                    read.session_id.as_deref(),
                    Some(declared_sid),
                    "{}",
                    case.name
                );
                checked += 1;
            }
            Some(_) => panic!(
                "{}: an envelope the corpus does not call complete read back as one",
                case.name,
            ),
            None => assert!(
                !expect_readable,
                "{}: a complete corpus envelope did not read back",
                case.name,
            ),
        }
    }

    assert!(
        checked > 0,
        "no corpus case exercised the complete-envelope readback path",
    );
}
