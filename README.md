# tapes-harnesses

Shared, open-source client-side harness knowledge for [Tapes](https://tapes.dev)
capture. This crate is the single home for everything a capture client needs to
know about a coding-agent harness on the client side. It is consumed by both
`tapesctl` (open source) and `paperd` (Paper Compute's daemon), so ingest parity
between `tapesctl start` and `paper start` is **structural, not policed** — the
same code runs in both.

Exactly three places hold harness knowledge; this crate is one of them (the
tapes deriver and the envelope spec/fixtures are the other two). It owns these
responsibilities:

- **`harness`** — the registry. One declaration per harness bundles its id,
  User-Agent rule, launch support, attribution strategy, transcript location,
  and plugin needs; the other modules take their harness ids from it and
  consumers derive their supported-agent lists from it.
- **`launch`** — per-harness env/config injection to run a harness under a
  capture proxy. Recipes are pure: they plan the argv prefix, the environment
  overlay, and any config documents a harness reads from disk, and the consumer
  owns process spawning, materialisation, and cleanup.
- **`attribution`** — session-file reads, fork-parent recovery, peer-PID
  lookup, and the Codex session watcher, grouped per harness
  (`attribution/claude/`, `attribution/codex/`) with the harness-agnostic
  pieces shared.
- **`transcript`** — discovering and packaging harness transcripts for the
  `POST /v1/ingest/transcript` lane.

## The crates

The repository is one workspace. Its first two crates are split by a single
test: does adding one more harness change this code?

- **`tapes-harnesses`** (root) — no, it changes *because* of harnesses. The
  registry, launch recipes, config patch grammars, plugin artifacts, the
  per-harness attribution lanes and their composition, and per-harness
  transcript knowledge.
- **`tapes-capture`** (`crates/tapes-capture/`) — the half that does not: the
  `X-Tapes-*` envelope producer and the harness-id vocabulary it stamps, the
  capture-gateway environment contract and launch-nonce protocol, peer-PID
  lookup, and the peer-trust ancestry walk.
- **`tapes-cassette-client`** (`cassette-client/`) — the cassette surface a
  consumer's CLI generates from a fetched document.
- **`tapes-read-contract`** (`read-contract/`) — the other document source for
  the same reducer: the *published* tapes read contract, vendored here from a
  release asset so the system holds one copy rather than one per client, plus
  the operation lookup, URL construction, transport seam, and coverage gate
  that turn it into requests.

The dependency edge runs one way, and Cargo enforces it rather than review:
`tapes-harnesses` depends on `tapes-capture`, never the reverse. The moment a
capture primitive knows a harness's name it stops being the thing every harness
shares. Where capture needs something *from* a harness — the envelope needs a
session's fields — it declares a trait and the harness crate implements it.

## Adding a harness

Teaching this crate about a new coding agent starts with one `const` in
`src/harness.rs`. [`docs/adding-a-harness.md`](docs/adding-a-harness.md) walks
the whole path: the registry declaration, when a launch recipe is needed, which
attribution strategy applies, and what the tapes deriver needs on its side.

## The envelope contract

The `X-Tapes-*` envelope is a cross-language contract: this crate produces it
in Rust, and the Go parsers in tapes' ingest and gateway capture read it back.
The shared fixture corpus is vendored under `crates/tapes-capture/vendor/tapes-envelope-fixtures/`
(authored in the tapes repository at `fixtures/envelope/`) and the
producer-side oracle in `crates/tapes-capture/src/envelope_fixtures.rs` runs against it — the same
corpus the Go parsers test against. `scripts/sync-envelope-fixtures.sh`
refreshes the copy and detects drift.

## Developing

The Nix flake dev shell pins the toolchain via `rust-toolchain.toml`:

```bash
nix develop
make check   # build + fmt-check + clippy + test
```

The crate denies `unwrap`, `expect`, and `panic` via `[lints]`; return
`Result` and surface errors through the crate error types instead.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.
