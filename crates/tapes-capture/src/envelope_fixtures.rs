//! The shared envelope fixture corpus vendored at
//! `vendor/tapes-envelope-fixtures/` (source: tapes `fixtures/envelope/` — see
//! that directory's `SOURCE.md`), as a reader plus this crate's producer-side
//! oracle.
//!
//! The corpus pins the `X-Tapes-*` header ↔ session-envelope contract. This
//! crate is the **producer**: it turns a resolved session identity into the on-wire
//! header set. The parsers on the other side (tapes-extproc's
//! `ParseSessionEnvelope`, the tapes ingest reader) table-test against the same
//! files. Drift between the two halves is otherwise invisible until a captured
//! session lands mis-attributed, so the oracle below makes the corpus
//! executable here rather than merely documentary.
//!
//! ### Why the reader is public
//!
//! A capture client composes envelopes of its own — from an inbound envelope it
//! chose to trust, from a session file it resolved, from request headers it
//! parsed — and each composition is a place its bytes can drift from the
//! contract. Those clients could only table-test against this corpus by
//! re-implementing the loader, the case-direction rules, and the
//! metadata-as-JSON comparison; a second reader is a second set of decisions
//! about what a case *means*, which is exactly the drift the corpus exists to
//! prevent. So the reader ships behind the `envelope-fixtures` feature, off by
//! default: a consumer enables it under `[dev-dependencies]` and gets the same
//! corpus, read the same way.
//!
//! This is a **test utility**. [`load_cases`] and [`decode_metadata`] panic on a
//! missing, truncated, or malformed corpus rather than returning a `Result` —
//! a broken corpus is a broken checkout, not a runtime condition to handle, and
//! a `Result` here would invite a consumer to swallow it and silently test
//! nothing. Do not call these from production paths.
//!
//! ### Which cases this side owns
//!
//! Each case declares a `direction`:
//!
//! * `roundtrip` — `encode(envelope) == headers`. Asserted here.
//! * `encode` — a *lossy* producer transform (session-name truncation,
//!   oversize-metadata drop, percent-encoding a path the reader won't decode
//!   back). The logical input is the case's `encode_from`, not its `envelope`;
//!   `encode(encode_from) == headers` is asserted here.
//! * `decode` — parser-only cases: malformed or missing-header input that a
//!   well-behaved producer never emits (empty parent header, metadata that
//!   isn't valid base64, a missing harness-id). Skipped here **by design** —
//!   there is no encode side to assert. The parser oracles cover them.
//!
//! ### What is compared
//!
//! Only the `x-tapes-*` headers. Every case's header set also carries the
//! server-trusted identity headers of the deployment that authored the corpus
//! (`x-paper-auth-org-id` / `x-paper-auth-subject`): an authenticating edge sets
//! those from validated credential claims, so a producer must never forge them.
//! The test asserts that this producer emits none of them.
//!
//! The metadata header is compared as *decoded JSON*, not as a base64 string.
//! JSON key ordering is not part of the contract, so byte-comparing the encoded
//! blob would pin an implementation detail of whichever serializer produced the
//! fixture. Every other header is compared byte for byte.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::HeaderMap;
use serde::Deserialize;

use super::{HARNESS_ID_UNKNOWN, TapesAttribution};

/// A case's `direction`: which half of the contract it asserts.
///
/// Read this rather than string-matching `direction`, so a consumer's skip
/// rule and this crate's cannot disagree about what a case claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `encode(envelope) == headers`, and the parsers get the same envelope
    /// back. Both halves assert it.
    Roundtrip,
    /// A *lossy* producer transform (session-name truncation, oversize-metadata
    /// drop, percent-encoding a path the reader won't decode back). The logical
    /// input is [`FixtureCase::encode_from`], not `envelope`.
    Encode,
    /// Parser-only: malformed or missing-header input a well-behaved producer
    /// never emits. There is no encode side to assert.
    Decode,
}

/// One `cases/*.json` file. Unknown fields (`grounding`, `notes`, `error`, …)
/// are ignored: they carry provenance for humans, not assertions for this side.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct FixtureCase {
    /// The case's name, as its filename declares it. Use it in assertion
    /// messages — a failure that names the case is one lookup from the file.
    pub name: String,
    /// Which half of the contract this case asserts, as the raw string. Prefer
    /// [`FixtureCase::direction`].
    pub direction: String,
    /// The complete header set for the case, including the server-trusted
    /// identity headers a producer must never emit — an authenticating edge
    /// sets those, and the corpus carries the spelling its authoring deployment
    /// uses.
    pub headers: BTreeMap<String, String>,
    /// The envelope the headers correspond to.
    pub envelope: FixtureEnvelope,
    /// Present only on lossy (`direction: encode`) cases: the logical envelope
    /// a producer starts from, before truncation / drop / percent-encoding.
    #[serde(default)]
    pub encode_from: Option<FixtureEnvelope>,
}

impl FixtureCase {
    /// This case's [`Direction`].
    ///
    /// # Panics
    ///
    /// If the case declares a direction this crate does not know. An
    /// unrecognised direction means the corpus grew a contract this side has
    /// not been taught, and silently skipping it would let the new contract go
    /// unasserted — the failure mode the corpus exists to prevent.
    #[must_use]
    pub fn direction(&self) -> Direction {
        match self.direction.as_str() {
            "roundtrip" => Direction::Roundtrip,
            "encode" => Direction::Encode,
            "decode" => Direction::Decode,
            other => panic!("{}: unknown direction {other:?}", self.name),
        }
    }

    /// The envelope a producer starts from: [`Self::encode_from`] on a lossy
    /// case, the case's own envelope otherwise.
    ///
    /// # Panics
    ///
    /// If a non-`encode` case carries an `encode_from`. The corpus reserves
    /// that field for lossy cases, so a `roundtrip` case carrying one is
    /// claiming `encode(envelope) == headers` while handing the producer a
    /// different input. Whichever of the two it means, the case is not saying
    /// it — better to fail than to silently prefer one.
    #[must_use]
    pub fn logical_envelope(&self) -> &FixtureEnvelope {
        assert!(
            self.encode_from.is_none() || self.direction() == Direction::Encode,
            "{}: encode_from is reserved for lossy `encode` cases, but direction is {:?}",
            self.name,
            self.direction,
        );
        self.encode_from.as_ref().unwrap_or(&self.envelope)
    }

    /// The `x-tapes-*` subset of this case's expected headers — what a
    /// producer must emit, with the server-trusted headers excluded.
    #[must_use]
    pub fn expected_tapes_headers(&self) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .filter(|(name, _)| name.starts_with("x-tapes-"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }
}

/// The envelope side of a case. `org_id` / `auth_subject` are deliberately not
/// modelled — they are not the producer's to emit (see module docs).
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct FixtureEnvelope {
    /// Harness id; absent means the case expects the `unknown` sentinel.
    #[serde(default)]
    pub harness_id: Option<String>,
    /// Opaque harness-side session id.
    #[serde(default)]
    pub harness_session_id: Option<String>,
    /// Harness version string.
    #[serde(default)]
    pub harness_version: Option<String>,
    /// Harness working directory, decoded.
    #[serde(default)]
    pub cwd: Option<String>,
    /// User-given session name, decoded and untruncated.
    #[serde(default)]
    pub name: Option<String>,
    /// Fork-parent's harness session id.
    #[serde(default)]
    pub parent_harness_session_id: Option<String>,
    /// Free-form harness metadata, as JSON rather than base64url.
    #[serde(default)]
    pub harness_metadata: Option<serde_json::Value>,
}

/// The directory holding the vendored `cases/*.json`.
///
/// Resolved from this crate's `CARGO_MANIFEST_DIR`, so a consumer that enables
/// the feature reads the corpus out of the crate's own checkout — the same
/// bytes this crate tests against, at whatever revision the consumer pinned.
#[must_use]
pub fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("tapes-envelope-fixtures")
        .join("cases")
}

/// Load every vendored case, sorted by path so failures report in a stable
/// order regardless of directory iteration order.
///
/// # Panics
///
/// If the corpus directory is unreadable, empty, or holds a file that is not a
/// valid case. See the module docs: a broken corpus is a broken checkout.
#[must_use]
pub fn load_cases() -> Vec<FixtureCase> {
    let dir = cases_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();

    assert!(
        !paths.is_empty(),
        "no envelope fixture cases under {} — run scripts/sync-envelope-fixtures.sh <tapes-checkout>",
        dir.display(),
    );

    paths
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
        })
        .collect()
}

/// Build the attribution a producer would hold for `env`.
///
/// This constructs [`TapesAttribution`] field-by-field rather than going
/// through `from_session()` / `codex_session()` because the corpus spans harnesses
/// those constructors don't cover (`pi`) and field combinations they can't
/// express. The named constructors are what production uses; this exercises the
/// serialization they all funnel into.
#[must_use]
pub fn attribution_from(env: &FixtureEnvelope) -> TapesAttribution {
    let metadata = match &env.harness_metadata {
        Some(serde_json::Value::Object(map)) => map.clone(),
        // A non-object metadata value is a parser-side concern; no producer
        // path can construct one (the field is typed as a JSON object).
        _ => serde_json::Map::new(),
    };

    TapesAttribution {
        harness_id: env
            .harness_id
            .clone()
            .unwrap_or_else(|| HARNESS_ID_UNKNOWN.to_owned()),
        session_id: env.harness_session_id.clone(),
        version: env.harness_version.clone(),
        cwd: env.cwd.clone(),
        name: env.name.clone(),
        parent_sid: env.parent_harness_session_id.clone(),
        metadata,
    }
}

/// The `x-tapes-*` subset of a header map, as plain strings.
///
/// The comparison unit for every producer assertion: pair it with
/// [`FixtureCase::expected_tapes_headers`].
///
/// # Panics
///
/// If an emitted header value is not visible ASCII. That is a producer bug —
/// every field is percent-encoded or base64url before it reaches a header.
#[must_use]
pub fn tapes_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter(|(name, _)| name.as_str().starts_with("x-tapes-"))
        .map(|(name, value)| {
            let v = value
                .to_str()
                .expect("emitted header value must be visible ASCII")
                .to_owned();
            (name.as_str().to_owned(), v)
        })
        .collect()
}

/// Decode a base64url(no-pad) metadata header into JSON.
///
/// Metadata is compared as decoded JSON, never as a base64 string: JSON key
/// ordering is not part of the contract, so byte-comparing the encoded blob
/// would pin an implementation detail of whichever serializer produced the
/// fixture.
///
/// # Panics
///
/// If `encoded` is not base64url(no-pad) of a JSON document.
#[must_use]
pub fn decode_metadata(encoded: &str) -> serde_json::Value {
    let raw = URL_SAFE_NO_PAD
        .decode(encoded)
        .unwrap_or_else(|e| panic!("metadata header is not base64url(no-pad): {e}"));
    serde_json::from_slice(&raw)
        .unwrap_or_else(|e| panic!("metadata header does not decode to JSON: {e}"))
}

// --- this crate's producer-side oracle over the corpus above ---------
//
// These stay `#[cfg(test)]` while the reader they use is public: a consumer
// wants the corpus and the reading rules, not this crate's assertions about
// its own producer.

#[cfg(test)]
use crate::envelope::{
    X_TAPES_HARNESS_METADATA, inject_tapes_attribution, inject_unattributed_envelope,
};

#[cfg(test)]
#[test]
fn produces_every_encodable_fixture_case() {
    let cases = load_cases();

    // A corpus that silently lost most of its files would otherwise "pass" on
    // whatever survived.
    assert!(
        cases.len() >= 15,
        "only {} envelope fixture cases loaded; the vendored corpus looks truncated",
        cases.len(),
    );

    let mut produced = 0_usize;
    let mut skipped = Vec::new();

    for case in &cases {
        // Skipping is driven purely by the case's own `direction`, never by a
        // hardcoded list here — a new case is covered the moment it is synced.
        // `direction()` also rejects a direction this crate has not been
        // taught, so a corpus that grows a new contract fails loudly here
        // rather than quietly skipping it.
        if case.direction() == Direction::Decode {
            skipped.push(case.name.clone());
            continue;
        }

        // A lossy case encodes from `encode_from`; a round-tripping one from
        // its own envelope, and `logical_envelope` rejects a case that
        // declares both inconsistently.
        let logical = case.logical_envelope();

        let mut headers = HeaderMap::new();
        inject_tapes_attribution(&mut headers, attribution_from(logical))
            .unwrap_or_else(|e| panic!("{}: inject failed: {e:?}", case.name));

        let got = tapes_headers(&headers);
        let want = case.expected_tapes_headers();

        // Compare the header *sets* first: a missing or surplus header is a
        // clearer failure than a per-value mismatch on one of them.
        let got_names: Vec<&String> = got.keys().collect();
        let want_names: Vec<&String> = want.keys().collect();
        assert_eq!(
            got_names, want_names,
            "{}: emitted header set does not match the fixture",
            case.name,
        );

        for (name, want_value) in &want {
            let got_value = &got[name];
            if name == X_TAPES_HARNESS_METADATA {
                assert_eq!(
                    decode_metadata(got_value),
                    decode_metadata(want_value),
                    "{}: {name} decodes to different JSON",
                    case.name,
                );
            } else {
                assert_eq!(got_value, want_value, "{}: {name}", case.name);
            }
        }

        // The producer must not forge the server-trusted identity headers; the
        // cloud edge sets those from validated JWT claims.
        for name in headers.keys() {
            assert!(
                !name.as_str().starts_with("x-paper-auth-"),
                "{}: producer emitted server-trusted header {name}",
                case.name,
            );
        }

        produced += 1;
    }

    preserves_complete_inbound_envelopes(&cases);

    assert_eq!(
        produced + skipped.len(),
        cases.len(),
        "every case must be either produced or explicitly skipped",
    );
    assert!(
        produced >= 10,
        "only {produced} cases exercised the producer; skipped: {skipped:?}",
    );
}

/// The `unknown` harness-id is a distinct code path in
/// [`inject_tapes_attribution`] — it returns after one header rather than
/// walking the budget. The corpus's `unknown-bare` case pins the result, but
/// only in aggregate with everything else; assert the path directly so a
/// regression names itself.
#[cfg(test)]
#[test]
fn unknown_harness_case_emits_only_the_required_header() {
    let case = load_cases()
        .into_iter()
        .find(|c| c.name == "unknown-bare")
        .expect("corpus contains the unknown-bare case");

    let mut headers = HeaderMap::new();
    inject_tapes_attribution(&mut headers, attribution_from(&case.envelope)).unwrap();

    let got = tapes_headers(&headers);
    assert_eq!(got.len(), 1, "unknown harness attaches exactly one header");
    assert_eq!(got["x-tapes-harness-id"], HARNESS_ID_UNKNOWN);
}

/// Cases whose inbound headers already carry a complete envelope pin a
/// different contract from the rest of the corpus: the producer must leave
/// them alone.
///
/// The producer loop above cannot cover it. It reconstructs headers from the
/// parsed envelope via `inject_tapes_attribution`, which is the wrong entry
/// point — preservation is decided in `inject_unattributed_envelope`, by
/// `has_complete_inbound_envelope`, before any attribution is built. A
/// regression that broke complete-envelope detection would leave every
/// assertion above green while the producer silently overwrote a caller's identity
/// with `unknown`.
///
/// So drive the real entry point with the case's own inbound headers — the
/// unattributed caller this contract exists for — and require the X-Tapes-*
/// set to come back untouched.
///
/// Selection is by shape, not by name: any case whose headers carry a usable
/// harness id and session id is a preservation case, so a future one is
/// covered the moment it is synced.
#[cfg(test)]
fn preserves_complete_inbound_envelopes(cases: &[FixtureCase]) {
    let mut checked = 0;

    for case in cases {
        let inbound: BTreeMap<String, String> = case
            .headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();

        let harness_id = inbound
            .get("x-tapes-harness-id")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty() && *v != HARNESS_ID_UNKNOWN);
        let session_id = inbound
            .get("x-tapes-harness-session-id")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty());
        if harness_id.is_none() || session_id.is_none() {
            continue;
        }

        let mut headers = HeaderMap::new();
        for (name, value) in &case.headers {
            let parsed_name = match http::HeaderName::from_bytes(name.as_bytes()) {
                Ok(n) => n,
                // A case may deliberately carry a header an HTTP stack would
                // reject; those are parser fixtures, not producer ones.
                Err(_) => continue,
            };
            let Ok(parsed_value) = http::HeaderValue::from_str(value) else {
                continue;
            };
            headers.insert(parsed_name, parsed_value);
        }
        let before = tapes_headers(&headers);
        if before.get("x-tapes-harness-id").map(String::as_str) != harness_id {
            // The header did not survive HeaderMap construction, so this case
            // is not exercising the preservation path.
            continue;
        }

        inject_unattributed_envelope(&mut headers).unwrap_or_else(|e| {
            panic!("{}: inject_unattributed_envelope failed: {e:?}", case.name)
        });

        assert_eq!(
            tapes_headers(&headers),
            before,
            "{}: a complete inbound envelope must be preserved as-is",
            case.name,
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no case exercised the complete-inbound-envelope preservation path; \
         the corpus should retain at least one (e.g. pi-complete)",
    );
}
