# tapes-crates

The client-side Rust crates for [Tapes](https://tapes.dev): what a coding-agent
harness needs in order to run under capture, what capture puts on the wire, and
how a client reads the results back.

Three crates, one workspace. They are consumed by `tapesctl` (open source) and
by Paper Compute's `paper` CLI and `paperd` daemon, which is the point: parity
between `tapesctl start` and `paper start` is **structural, not policed**,
because the same code runs in both. A behaviour that lives here cannot differ
between clients; a behaviour that lives in a client can, and that is the test
for whether something belongs in this repository at all.

## The public API boundary

Each crate owns one question. The boundaries below are the contract this
repository publishes — a change that moves a responsibility across one of these
lines is a breaking change even when every signature still compiles.

| crate | owns | does **not** own |
| --- | --- | --- |
| **`tapes-harnesses`** (`crates/tapes-harnesses/`) | **Harness launch and attribution knowledge.** The registry, launch recipes, config patch grammars, plugin artifacts, per-harness attribution lanes, transcript discovery and packaging. | Anything true of every harness — that is `tapes-capture`. |
| **`tapes-capture`** (`crates/tapes-capture/`) | **The capture protocol.** The `X-Tapes-*` envelope producer and the harness-id vocabulary it stamps, the capture-gateway environment contract, the launch-nonce protocol, peer-PID lookup, and the peer-trust ancestry walk. | Any harness's name, and any knowledge that arrives *because* a harness was added. |
| **`tapes-client`** (`crates/tapes-client/`) | **The read surface.** The sealed core contract (vendored from a published release asset) and a deployment's discovered cassettes, driven over **one transport seam** — with the error taxonomy, decode policy, pagination convention, and path join written once beneath both. | Authentication, tenancy, transport, and rendering. Each is a consumer's, and each consumer's answer differs. |

The membership test for the first two is: **would adding one more harness change
this?** If yes it is `tapes-harnesses`; if no it is `tapes-capture`. The
dependency edge runs one way and Cargo enforces it rather than review —
`tapes-harnesses` depends on `tapes-capture`, never the reverse. The moment a
capture primitive knows a harness's name it has stopped being the thing every
harness shares. Where capture needs something *from* a harness — the envelope
needs a session's fields — it declares a trait and the harness crate implements
it.

The test for the third is different, because `tapes-client` is not split by
subject matter but by *when the operation table is known*: the core contract is
sealed at build time, a deployment's cassettes are discovered at runtime. Both
halves are thin method tables over one shared floor. That is the whole design —
see [`crates/tapes-client/README.md`](crates/tapes-client/README.md).

Nothing Paper-specific belongs in any of the three: no auth headers, no
endpoints, no branding in behaviour. Delivery, auth, and retry live in each
consumer.

## Publishing

Not yet published. These crates are consumed by git pin today, and every
manifest carries `publish = false` from the workspace root.

The intent is crates.io, under semantic versioning — which is why the boundary
table above is written as a contract rather than as a description of where
files currently sit. When the first release happens, `publish` flips once at
the root, together with that release PR, and the version numbers start meaning
something to people who are not in this repository.

## Adding a harness

Teaching this repository about a new coding agent starts with one `const` in
`crates/tapes-harnesses/src/harness.rs`.
[`docs/adding-a-harness.md`](docs/adding-a-harness.md) walks the whole path: the
registry declaration, when a launch recipe is needed, which attribution strategy
applies, and what the tapes deriver needs on its side.

## The envelope contract

The `X-Tapes-*` envelope is a cross-language contract: `tapes-capture` produces
it in Rust, and the Go parsers in tapes' ingest and gateway capture read it
back. The shared fixture corpus is vendored under
`crates/tapes-capture/vendor/tapes-envelope-fixtures/` (authored in the tapes
repository at `fixtures/envelope/`) and the producer-side oracle in
`crates/tapes-capture/src/envelope_fixtures.rs` runs against it — the same
corpus the Go parsers test against. `scripts/sync-envelope-fixtures.sh`
refreshes the copy and detects drift.

## The read contract

`tapes-client` vendors the published tapes read contract at
`crates/tapes-client/contracts/tapes-api.yaml`, pinned by fingerprint in the
`PROVENANCE.md` beside it. `make contracts-check` verifies the vendored bytes
against both that fingerprint and the published release asset, and CI runs it
with `TAPES_CONTRACT_STRICT=1` so a gate that cannot reach its input fails
rather than reporting a comparison it never made.

## Developing

The repository is one workspace whose members are `crates/*`, so a bare `cargo`
invocation at the root covers every crate — including crates added after a
command was written. The Nix flake dev shell pins the toolchain via
`rust-toolchain.toml`:

```bash
nix develop
make check   # build + fmt-check + clippy + test
```

Every crate denies `unwrap`, `expect`, and `panic` through the workspace lint
table; return `Result` and surface errors through the crate error types
instead.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.
