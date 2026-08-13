# tapes-harnesses

Client-side knowledge about coding-agent harnesses, for [Tapes](https://tapes.dev)
capture.

This is the half of capture that changes *because* a harness was added. Its
membership test is one question: **would adding one more harness change this?**
If yes it belongs here; if no it belongs in
[`tapes-capture`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-capture/README.md), which this crate depends on and
which may never depend back.

Consumed by `tapesctl` and by Paper Compute's `paper`/`paperd`, so the two
clients launch and attribute sessions with the same code rather than with two
implementations kept in agreement by review.

## Public seams

- **`harness`** — the registry, and the vocabulary the rest of the crate speaks.
  One declaration per harness bundles its id, User-Agent rule, launch support,
  attribution strategy, transcript location, and plugin needs. Every other
  module takes its harness ids from here, and consumers derive their
  supported-agent lists from it rather than hard-coding one.
- **`launch`** — per-harness environment and config injection for running a
  harness under a capture proxy. Recipes are **pure**: they plan an argv prefix,
  an environment overlay, and any config documents a harness reads from disk.
  Spawning, materialisation, and cleanup stay with the consumer.
- **`attribution`** — session-file reads, fork-parent recovery, peer-PID lookup,
  and the session watchers, grouped per harness (`attribution/claude/`,
  `attribution/codex/`, …) with the harness-agnostic pieces shared. The
  composition itself lives in `attribution/pipeline.rs`.
- **`plugin`** — the plugin and extension artifacts a harness needs on disk,
  with the vendor-neutrality bar enforced by the module's own tests.
- **`transcript`** — discovering and packaging harness transcripts for the
  transcript ingest lane. Delivery, auth, and retry are the consumer's.
- **`config`** — the config-document patch grammars the launch recipes plan
  against.

No feature flags: everything above is always compiled.

## Adding a harness

Start at `src/harness.rs`, then follow
[`docs/adding-a-harness.md`](https://github.com/papercomputeco/tapes-crates/blob/main/docs/adding-a-harness.md) at the repository
root — it covers the registry declaration, when a launch recipe is needed, which
attribution strategy applies, and what the tapes deriver needs on its side.

## Stability

This crate is **supported public API**, meant to be depended on directly. So
are its two siblings — [`tapes-capture`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-capture/README.md) (the
capture protocol) and [`tapes-client`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-client/README.md) (the read
client) — and all three version independently on crates.io.

Pre-1.0, `0.x` versions carry the usual Cargo meaning: a breaking change bumps
the minor (`0.2.0`), anything compatible bumps the patch (`0.1.1`). What counts
as breaking is the boundary in the [repository README](https://github.com/papercomputeco/tapes-crates/blob/main/README.md#the-public-api-boundary),
not just the signatures: knowledge that stops being harness-specific belongs in
`tapes-capture`, and moving it is a break here even when nothing stops
compiling.

Adding a harness to the registry is additive, and it is the change most worth
reading about — consumers derive their supported-agent lists from that registry
rather than hard-coding one, so a new entry appears in their surface without
their doing anything. This crate also requires `tapes-capture` at a version, so
an upgrade may carry one; the changelog says when it does.

Changes are recorded in [`CHANGELOG.md`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-harnesses/CHANGELOG.md).

## License

Dual-licensed under MIT OR Apache-2.0; see the repository root.
