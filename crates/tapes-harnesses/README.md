# tapes-harnesses

Client-side knowledge about coding-agent harnesses, for [Tapes](https://tapes.dev)
capture.

This is the half of capture that changes *because* a harness was added. Its
membership test is one question: **would adding one more harness change this?**
If yes it belongs here; if no it belongs in
[`tapes-capture`](../tapes-capture/README.md), which this crate depends on and
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
[`docs/adding-a-harness.md`](../../docs/adding-a-harness.md) at the repository
root — it covers the registry declaration, when a launch recipe is needed, which
attribution strategy applies, and what the tapes deriver needs on its side.

## License

Dual-licensed under MIT OR Apache-2.0; see the repository root.
