# tapes-capture

The [Tapes](https://tapes.dev) capture protocol: the primitives that are true of
every harness.

This is the half of capture that **no harness changes**. Nothing here may learn
a harness's name — the moment it does, it has stopped being the thing every
harness shares. Where capture needs something *from* a harness, it declares a
trait and [`tapes-harnesses`](../tapes-harnesses/README.md) implements it; the
dependency edge runs one way and Cargo enforces it rather than review.

## Public seams

- **`envelope`** — the `X-Tapes-*` envelope producer and the harness-id
  vocabulary it stamps. This is a **cross-language contract**: produced here in
  Rust, parsed by the Go implementations in tapes' ingest and gateway capture.
- **`gateway`** — the capture-gateway environment contract and the launch-nonce
  protocol: what a launched process is told about where to send capture, and
  what proves the process is the one that was launched.
- **`peer_pid`** — resolving the process on the other end of a local connection.
- **`peer_trust`** — the ancestry walk that decides whether that peer is trusted.
- **`session`** — `HarnessSession`, the trait by which a harness supplies the
  envelope the session fields it needs.

## Features

| feature | default | what it does |
| --- | --- | --- |
| `envelope-fixtures` | off | Exposes `envelope::fixtures`, the reader for the shared fixture corpus vendored under `vendor/tapes-envelope-fixtures/`, so a consumer can table-test its own envelope composition against the same cases this crate does. |

`envelope-fixtures` is off by default and must stay that way: the corpus is only
reachable because a git-dependency checkout carries the whole repository, and
the reader panics by design on a malformed corpus. Neither belongs in a
production build. Enable it under `[dev-dependencies]`.

## The fixture corpus

Do not change producer behaviour without updating the shared corpus in the tapes
repository first, then re-vendoring with `scripts/sync-envelope-fixtures.sh`. The
oracle in `src/envelope_fixtures.rs` must stay green against the vendored copy —
if a change makes it fail, the contract conversation happens in tapes, not by
editing the fixtures here.

## License

Dual-licensed under MIT OR Apache-2.0; see the repository root.
