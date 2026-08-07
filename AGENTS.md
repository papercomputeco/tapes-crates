# Contributing

This repository is one Cargo workspace. The Nix flake dev shell is the
recommended development environment; it pins the Rust toolchain via
`rust-toolchain.toml`.

| crate | what it holds |
| --- | --- |
| `tapes-harnesses` (repository root) | knowledge that changes when a harness is added: the registry, launch recipes, config patch grammars, plugin artifacts, per-harness attribution lanes, transcript discovery, and the `X-Tapes-*` envelope |
| `tapes-capture` (`crates/tapes-capture/`) | knowledge that does not: the capture-gateway environment contract and launch-nonce protocol, peer-PID lookup, and the peer-trust ancestry check |
| `tapes-cassette-client` (`cassette-client/`) | the generated cassette surface: discovery, OpenAPI reduction, the surface cache, clap command synthesis, and the transport |

The membership test for the first two is **would adding one more harness change
this?** If not, it belongs in `tapes-capture`. The dependency edge runs one way
— `tapes-harnesses` depends on `tapes-capture`, never the reverse — so a
capture primitive cannot learn a harness's name even by accident.

Every crate is in the default workspace selection, so a bare `cargo test` or
`cargo clippy` at the root covers all of them; no `--manifest-path` threading.

```bash
nix develop
make check
```

Before opening a pull request, run:

```bash
make lint   # cargo fmt --all --check + cargo clippy -D warnings
make test   # cargo test
```

The crate denies `unwrap`, `expect`, and `panic` via `[lints]`; return
`Result` and surface errors through the crate error types instead.

## What lives here — and what must not

This crate holds **client-side harness knowledge only**: launch recipes,
attribution, transcript discovery/packaging, and the `X-Tapes-*` envelope
producer. It ships to non-Paper users, so nothing Paper-specific belongs here —
no Paper auth headers, no Paper endpoints, no Paper branding in behavior.
Delivery, auth, and retry live in each consumer (`tapesctl`, `paperd`), not
here.

The envelope is a cross-language contract with the Go parsers in tapes. Do not
change producer behavior without updating the shared fixture corpus in the
tapes repository first, then re-vendoring via
`scripts/sync-envelope-fixtures.sh`. The oracle in `src/envelope_fixtures.rs`
must stay green against the vendored corpus — if your change makes it fail,
the contract conversation happens in tapes, not by editing the fixtures here.

## Pull requests

Pull request titles must use one of the repository's accepted contribution
labels, such as `✨ feat:`, `🔧 fix:`, `🧹 chore:`, or `📚 docs:`, and reference
the relevant Linear issue with a magic word (e.g. `fixes PCC-123` or
`related to PCC-123`).
