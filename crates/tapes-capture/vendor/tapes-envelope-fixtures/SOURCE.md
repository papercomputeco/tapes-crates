# `tapes-envelope-fixtures/` — vendoring + sync

`cases/*.json`, `DIGEST`, and `README.upstream.md` are a **verbatim copy** of the
shared envelope fixture corpus authored in the `tapes` repository. Do not
hand-edit them here — a change belongs upstream, and this copy is refreshed with
`scripts/sync-envelope-fixtures.sh`. `DIGEST` makes that rule enforceable rather
than advisory; see [The seal](#the-seal) below.

## Source

* **Repo:** `tapes` (Paper Compute).
* **Path within repo:** `fixtures/envelope/`.
* **Current snapshot SHA:** `7e3cdefbd3f5989be0656822208e0d3a59754413`
  ("✨ feat(fixtures): seal the envelope corpus and gate it in CI (#284)") — the
  last commit to touch `fixtures/envelope`. TODO: replace with a tagged
  fixture-cut id once tapes publishes versioned cuts (`fixtures/manifest.json`
  reserves the `cut` block for exactly that).

  Two upstream commits landed between this snapshot and the previous one
  (`9cc71917`), and they are why the refresh was not a no-op:

  * `d89c330` (#281) moved cwd decoding to the reader and made it refuse
    control bytes. It rewrote `cwd-unicode` — now a `roundtrip` case whose
    envelope carries the decoded path, where it used to be a lossy `encode`
    case asserting the reader stored the escaped form — and dropped the
    now-unreachable `cwd` from `cwd-control-bytes-escaped`'s envelope.
  * `7e3cdef` (#284) added the seal plus four cases: `cwd-literal-plus`,
    `cwd-malformed-percent-encoding`, `error-metadata-padded-base64`, and
    `session-name-control-bytes-escaped`.

  None of it moved this crate's producer. Both rewritten cases changed only
  their decode side — `headers` and `encode_from`, which are the only fields
  the producer oracle reads, are untouched — and of the four new cases the two
  the producer must satisfy were already satisfied: `+` is outside
  `UTF8_VALUE_ESCAPE`, so it crosses the wire raw, and control bytes were
  already escaped. The corpus grew cases pinning behaviour this crate had but
  nothing had yet asserted.

The snapshot is the contract: it is pinned to a specific upstream commit, and a
refresh lands in the same PR as whatever consumer change it forces.

## What it pins

The `X-Tapes-*` header ↔ session-envelope contract. The contract has sides that
live in different repositories and different languages, and drift between them
is invisible until a session lands mis-attributed:

* **Producer** (this crate) — `envelope::inject_tapes_attribution` turns a
  resolved session identity into the on-wire header set: percent-encoding, the
  256-byte session-name cap, base64url metadata, and the 8 KiB budget.
* **Parser** — `tapes-extproc`'s `ParseSessionEnvelope`, and the tapes ingest
  reader, read that header set back into an envelope.

Every side table-tests against this one corpus. There are three vendored copies
in total and **all must be refreshed from the same upstream SHA**:

| Repo | Path |
| --- | --- |
| `tapes-harnesses` (here) | `crates/tapes-capture/vendor/tapes-envelope-fixtures/` |
| `platform/paper` | `crates/paper-daemon/vendor/tapes-envelope-fixtures/` |
| `tapes-extproc` | `internal/headers/testdata/envelope/` |

## The seal

Vendoring three copies only buys parity if the three are the same bytes, and
nothing about copying guarantees that. A hand-edit to one leaves two
implementations testing against different corpora while both stay green — the
same invisible drift the corpus exists to prevent, moved up a level.

`DIGEST` closes it. One line, `sha256:<hex>`, over the case set:

> for each `cases/*.json`, sorted by base name, feed
> `"<basename>  <sha256-hex-of-file-bytes>\n"` into a SHA-256; the digest is the
> hex of that hash.

Trivial on purpose, so each consumer restates it in its own language instead of
sharing an implementation — two copies of one implementation agree even when
both are wrong. It hashes raw bytes rather than parsed JSON, because the sync
script copies bytes and a reformat is drift too, and it covers names as well as
contents, so an addition, a deletion, and a rename are each caught.

Two things here recompute it, and both fail closed:

* `crates/tapes-capture/tests/envelope_corpus_seal.rs` — runs in the default
  `cargo test` (it needs no `envelope-fixtures` feature, since the seal is
  about bytes rather than the reader) and in CI as `make corpus-seal`. This is
  what catches a stale or edited copy with no tapes checkout and no network.
* `scripts/sync-envelope-fixtures.sh` — verifies the staged corpus against the
  `DIGEST` it shipped with *before* swapping it in, so a refresh that does not
  match is refused with the live copy untouched. `--check` additionally holds
  the vendored copy against its own seal.

The corpus sits under `tapes-capture` because that is where the producer it
pins lives. The envelope is a wire format, and a wire format does not change
when a harness is added — so neither it nor its fixtures belong in the crate
that declares harnesses.

## What consumes it here

`crates/tapes-capture/src/envelope_fixtures.rs` — the producer-side oracle. For
each case whose `direction` is `roundtrip` or `encode` it builds the logical
envelope (`encode_from` when the case is lossy, else `envelope`), emits the
headers, and asserts they match the case byte for byte.

`direction: decode` cases are parser-only — malformed or missing-header input a
well-behaved producer never emits — and are skipped here by design. They are
covered by the parser-side oracles.

`crates/tapes-capture/tests/envelope_fixture_corpus.rs` — the same corpus read
through the public API, proving a consumer can do what the in-crate oracle does.

`crates/tapes-capture/tests/envelope_corpus_seal.rs` — the seal above. It
asserts nothing about the envelope contract; it asserts these are the right
bytes to be asserting it against.

## How to refresh

```sh
# from a tapes checkout at the target commit
./scripts/sync-envelope-fixtures.sh /path/to/tapes

# or detect drift without writing anything
./scripts/sync-envelope-fixtures.sh --check /path/to/tapes
```

`DIGEST` is carried across with the cases and verified against them before the
refresh installs, so there is no separate step to remember and no way to land a
corpus the seal does not cover.

Then update the snapshot SHA above, run `cargo test`, and
commit the fixture change with any producer change it forced.
