# `tapes-envelope-fixtures/` — vendoring + sync

`cases/*.json`, `DIGEST`, and `README.upstream.md` are a **verbatim copy** of the
shared envelope fixture corpus authored in the `tapes` repository. Do not
hand-edit them here — a change belongs upstream, and this copy is refreshed with
`scripts/sync-envelope-fixtures.sh`. `DIGEST` makes that rule enforceable rather
than advisory; see [The seal](#the-seal) below.

## Source

* **Repo:** `tapes` (Paper Compute).
* **Path within repo:** `fixtures/envelope/`.
* **Current snapshot SHA:** `40c90342e5dd669b2d9d16ac6874498476c335ff`
  ("✨ feat(sessions): resolve a fork parent in the parent's own harness") —
  **an unreleased tapes commit**, vendored ahead of its release so the producer
  half of `x-tapes-parent-harness-id` lands with the parser half. Re-pin this
  line (and re-run the sync, which must be a no-op) to the tapes release that
  carries it before a `tapes-capture` release depends on the new cases. TODO:
  replace with a tagged fixture-cut id once tapes publishes versioned cuts
  (`fixtures/manifest.json` reserves the `cut` block for exactly that).

  It adds three cases on top of the previous snapshot (`7e3cdefb`, #284):

  * `codex-with-cross-harness-parent` (`roundtrip`) — a codex child whose
    parent lives in claude: `x-tapes-parent-harness-id: claude` beside the
    parent session id.
  * `hermes-session` (`roundtrip`) — a hermes envelope with a session id and
    cwd, pinning that harness ids are an open set carried verbatim.
  * `error-parent-harness-id-empty` (`decode`) — an empty parent-harness header
    is dropped by the reader rather than normalised to `unknown`.

  The producer satisfies both new roundtrip cases with no behaviour change: the
  fixture reader already carried `parent_harness_id` into
  `TapesAttribution::parent_harness_id`, which the producer emits beside the
  parent session id. The decode case is skipped by the producer oracle by
  design. `README.upstream.md` gained the matching header-table row and notes.

The snapshot is the contract: it is pinned to a specific upstream commit, and a
refresh lands in the same PR as whatever consumer change it forces.

## What it pins

The `X-Tapes-*` header ↔ session-envelope contract. The contract has sides that
live in different repositories and different languages, and drift between them
is invisible until a session lands mis-attributed:

* **Producer** (this crate) — `envelope::inject_tapes_attribution` turns a
  resolved session identity into the on-wire header set: percent-encoding, the
  256-byte session-name cap, base64url metadata, and the 8 KiB budget.
* **Parser** — the tapes capture extproc's `ParseSessionEnvelope`
  (`extproc/headers/` in `tapes`), and the tapes ingest reader, read that
  header set back into an envelope. Both live in `tapes` and test against the
  upstream corpus directly, so they need no vendored copy.

Every side table-tests against this one corpus. There are two vendored copies
in total and **both must be refreshed from the same upstream SHA**:

| Repo | Path |
| --- | --- |
| `tapes-harnesses` (here) | `crates/tapes-capture/vendor/tapes-envelope-fixtures/` |
| `platform/paper` | `crates/paper-daemon/vendor/tapes-envelope-fixtures/` |

The retired standalone `tapes-extproc` repository carried a third copy under
`internal/headers/testdata/envelope/`; nothing deploys from it, so it is no
longer refreshed.

## The seal

Vendoring two copies only buys parity if the two are the same bytes, and
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
