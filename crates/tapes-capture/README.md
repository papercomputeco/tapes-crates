# tapes-capture

The [Tapes](https://tapes.dev) capture protocol: the primitives that are true of
every harness.

This is the half of capture that **no harness changes**. Nothing here may learn
a harness's name — the moment it does, it has stopped being the thing every
harness shares. Where capture needs something *from* a harness, it declares a
trait and [`tapes-harnesses`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-harnesses/README.md) implements it; the
dependency edge runs one way and Cargo enforces it rather than review.

## Public seams

- **`envelope`** — the `X-Tapes-*` envelope producer and the harness-id
  vocabulary it stamps. This is a **cross-language contract**: produced here in
  Rust, parsed by the Go implementations in tapes' ingest and gateway capture.
- **`gateway`** — the capture-gateway environment contract and **two**
  sub-protocols over it. The launch-nonce protocol covers what a launched
  process is told about where to send capture, and what proves the process is
  the one that was launched. The provider-route protocol covers how one gateway
  address serves several upstream providers, by labelling the request path with
  the provider it belongs to. A summary that mentions only the nonce leaves out
  five of the module's nine public items — all nine are re-exported at the crate
  root.
- **`peer_pid`** — resolving the process on the other end of a local connection.
- **`peer_trust`** — the ancestry walk that decides whether that peer is trusted.
- **`session`** — `HarnessSession`, the trait by which a harness supplies the
  envelope the session fields it needs.

## Features

| feature | default | what it does |
| --- | --- | --- |
| `envelope-fixtures` | off | Exposes `envelope::fixtures`, the reader for the shared fixture corpus vendored under `vendor/tapes-envelope-fixtures/`, so a consumer can table-test its own envelope composition against the same cases this crate does. |

`envelope-fixtures` is off by default and must stay that way: the reader panics
by design on a malformed corpus, which does not belong in a production build.
Enable it under `[dev-dependencies]`.

The corpus itself resolves by a path relative to the crate manifest, and it
ships inside the packaged crate — so the reader works from a crates.io
dependency and from a git checkout alike. This crate's own documentation is
built with every feature enabled, so `envelope::fixtures` is on the API docs
even though a default build does not compile it.

## The fixture corpus

Do not change producer behaviour without updating the shared corpus in the tapes
repository first, then re-vendoring with `scripts/sync-envelope-fixtures.sh`. The
oracle in `src/envelope_fixtures.rs` must stay green against the vendored copy —
if a change makes it fail, the contract conversation happens in tapes, not by
editing the fixtures here.

That last rule is enforced, not merely stated: the corpus carries a `DIGEST`
over its case set, and `tests/envelope_corpus_seal.rs` recomputes it on every
`cargo test`. Editing a vendored case turns a contract disagreement that would
have been caught in review into a red seal naming the file.

## Stability

This crate is **supported public API**, meant to be depended on directly. So
are its two siblings — [`tapes-harnesses`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-harnesses/README.md) (the
harness knowledge) and [`tapes-client`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-client/README.md) (the read
client) — and all three version independently on crates.io.

Pre-1.0, `0.x` versions carry the usual Cargo meaning: a breaking change bumps
the minor (`0.2.0`), anything compatible bumps the patch (`0.1.1`). What counts
as breaking is the boundary in the [repository README](https://github.com/papercomputeco/tapes-crates/blob/main/README.md#the-public-api-boundary),
not just the signatures: a capture primitive that starts knowing a harness's
name has broken this crate's promise whether or not anything stops compiling.

The `X-Tapes-*` envelope is the exception worth stating outright, because it is
not only Rust. It is a cross-language contract that Go parsers read on the other
side, so an envelope change is a change to something this crate's version number
cannot describe on its own — it goes through the shared fixture corpus first.

Changes are recorded in [`CHANGELOG.md`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-capture/CHANGELOG.md).

## License

Dual-licensed under MIT OR Apache-2.0; see the repository root.
