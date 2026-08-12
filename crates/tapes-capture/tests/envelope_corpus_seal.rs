//! The seal over the vendored envelope fixture corpus.
//!
//! The corpus at `vendor/tapes-envelope-fixtures/` is a copy. Every other test
//! that reads it proves this crate *conforms* to those bytes — none of them
//! prove the bytes are the right ones. A hand-edit here (or a half-finished
//! sync, or a case dropped in a merge) leaves this crate and the Go parsers on
//! the other side of the wire table-testing against different corpora while
//! both stay green, which is the exact failure the shared corpus exists to
//! prevent: drift that is invisible until a captured session lands
//! mis-attributed.
//!
//! `DIGEST` closes that, and this test is what makes it bite. It is vendored
//! alongside `cases/` and recomputed here, so a stale or locally-edited copy
//! fails in this repository's own CI with no tapes checkout and no network.
//!
//! ### Why this file is not feature-gated
//!
//! `tests/envelope_fixture_corpus.rs` is gated on `envelope-fixtures` because
//! it exercises the corpus *reader*, which only exists behind that feature.
//! The seal is a statement about bytes on disk and needs no reader — gating it
//! the same way would mean the default `cargo test` run, which is what most
//! contributors execute, never checked the corpus it ships. So this file
//! resolves the path itself rather than borrowing `fixtures::cases_dir()`,
//! which also keeps the seal independent of the module it is meant to police.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The vendored corpus root, resolved from this crate's own manifest so the
/// seal reads the same checkout the crate compiles against.
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("tapes-envelope-fixtures")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Recompute the seal over `cases_dir`, as `sha256:<hex>`.
///
/// Upstream's algorithm, reimplemented rather than imported: for each
/// `cases/*.json`, sorted by base name, feed
/// `"<basename>  <sha256-hex-of-file-bytes>\n"` into a SHA-256 and hex the
/// result. It is trivial by design so that every consumer can restate it from
/// that one sentence without a canonical-JSON library — a shared *library*
/// would defeat the point, since two copies of one implementation agree even
/// when both are wrong.
///
/// Two properties are load-bearing. It hashes **raw bytes** rather than parsed
/// JSON, because the sync script copies bytes and a reformat is drift too. And
/// it covers **names as well as contents**, so an addition, a deletion, and a
/// rename are each caught rather than only an edit to a file that already
/// existed.
fn corpus_digest(cases_dir: &Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(cases_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", cases_dir.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| {
            p.file_name()
                .expect("case path has a file name")
                .to_str()
                .expect("case file name is UTF-8")
                .to_owned()
        })
        .collect();

    // An empty corpus must not produce a digest at all. SHA-256 of nothing is a
    // perfectly good hash, so without this the seal would happily compare two
    // empty directories and pass — a wiped corpus reading as a sealed one is
    // the one verdict this test must never return.
    assert!(
        !names.is_empty(),
        "no case files under {} — the corpus is missing, not merely stale",
        cases_dir.display(),
    );

    // Byte-order, matching the reference implementation's sort. Rust's `Ord`
    // for `String` is already bytewise; naming it here so a later switch to a
    // locale-aware or case-insensitive sort reads as the contract change it
    // would be.
    names.sort();

    let mut outer = Sha256::new();
    for name in &names {
        let bytes =
            std::fs::read(cases_dir.join(name)).unwrap_or_else(|e| panic!("read case {name}: {e}"));
        let inner = hex(&Sha256::digest(&bytes));
        // Two spaces between name and hash — the `shasum` convention upstream
        // adopted. It is part of the algorithm, not formatting.
        outer.update(format!("{name}  {inner}\n").as_bytes());
    }
    format!("sha256:{}", hex(&outer.finalize()))
}

/// The vendored corpus is exactly the case set its `DIGEST` seals.
///
/// A failure here is not a test to relax. It means the vendored bytes and the
/// seal disagree, and there are only two honest resolutions: re-sync from the
/// upstream commit the corpus belongs to, or — if upstream genuinely changed —
/// land the refreshed corpus and its new `DIGEST` together, in the same commit
/// as whatever consumer change it forced.
#[test]
fn the_vendored_corpus_matches_its_digest() {
    let corpus = corpus_dir();
    let digest_path = corpus.join("DIGEST");

    let sealed_raw = std::fs::read_to_string(&digest_path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}\nthe corpus ships a DIGEST; re-sync with \
             scripts/sync-envelope-fixtures.sh <tapes-checkout>",
            digest_path.display(),
        )
    });
    let sealed = sealed_raw.trim();

    // A DIGEST that is present but unreadable must fail rather than compare
    // loosely: an empty or truncated file would otherwise turn the seal into a
    // string comparison that happens to be false for the right reason today
    // and the wrong reason tomorrow.
    assert!(
        sealed.starts_with("sha256:") && sealed.len() == "sha256:".len() + 64,
        "{} does not hold a `sha256:<64 hex>` line: {sealed:?}",
        digest_path.display(),
    );

    let recomputed = corpus_digest(&corpus.join("cases"));

    assert_eq!(
        recomputed,
        sealed,
        "the vendored envelope corpus does not match its DIGEST.\n\
         \n\
         sealed:     {sealed}\n\
         recomputed: {recomputed}\n\
         \n\
         A case under {} has been edited, added, or removed without the seal \
         moving with it. Do not hand-edit the vendored copy: the corpus is \
         authored upstream in tapes at fixtures/envelope/. Re-sync with\n\
         \n    scripts/sync-envelope-fixtures.sh <tapes-checkout>\n\
         \n\
         and land the refreshed corpus together with any consumer change it \
         forces — the corpus is the contract, so both halves travel together.",
        corpus.join("cases").display(),
    );
}
