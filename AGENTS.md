# Contributing

This repository is one Cargo workspace. The Nix flake dev shell is the
recommended development environment; it pins the Rust toolchain via
`rust-toolchain.toml`.

Three crates, and nothing else:

| crate | what it holds |
| --- | --- |
| `tapes-harnesses` (`crates/tapes-harnesses/`) | knowledge that changes when a harness is added: the registry, launch recipes, config patch grammars, plugin artifacts, per-harness attribution lanes, and transcript discovery |
| `tapes-capture` (`crates/tapes-capture/`) | knowledge that does not: the `X-Tapes-*` envelope producer, the capture-gateway environment contract and launch-nonce protocol, peer-PID lookup, and the peer-trust ancestry check |
| `tapes-client` (`crates/tapes-client/`) | the whole read surface: the sealed core contract and a deployment's discovered cassettes, driven through one transport seam |

The membership test for the first two is **would adding one more harness change
this?** If not, it belongs in `tapes-capture`. The dependency edge runs one way
— `tapes-harnesses` depends on `tapes-capture`, never the reverse — so a
capture primitive cannot learn a harness's name even by accident.

`tapes-client` absorbed two earlier crates, `tapes-read-contract` and
`tapes-cassette-client`, which held the sealed and discovered halves of the
read surface separately. They are gone, along with the compatibility shims that
briefly re-exported them; a consumer names `tapes-client`. Do not reintroduce a
per-surface crate — the split is what produced the drift described below.

The root manifest is a pure workspace whose members are `crates/*`, so every
crate is in the default selection: a bare `cargo test` or `cargo clippy` at the
root covers all of them, with no `--manifest-path` threading and no list of
members to forget a crate in.

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

`tapes-harnesses` holds **client-side harness knowledge only**: the registry,
launch recipes, config patch grammars, plugin artifacts, per-harness
attribution, and transcript discovery/packaging. The half of capture that no
harness changes — the `X-Tapes-*` envelope producer, the gateway/nonce
protocol, peer-PID lookup, peer trust — lives in `crates/tapes-capture/`, which
`tapes-harnesses` depends on and which may never depend back. Anything shipping to
non-Paper users means nothing Paper-specific belongs in either — no Paper auth
headers, no Paper endpoints, no Paper branding in behavior. Delivery, auth, and
retry live in each consumer (`tapesctl`, `paperd`), not here.

The envelope is a cross-language contract with the Go parsers in tapes. Do not
change producer behavior without updating the shared fixture corpus in the
tapes repository first, then re-vendoring via
`scripts/sync-envelope-fixtures.sh`. The oracle in `crates/tapes-capture/src/envelope_fixtures.rs`
must stay green against the vendored corpus — if your change makes it fail,
the contract conversation happens in tapes, not by editing the fixtures here.
Editing them is also futile: the corpus is sealed by a `DIGEST`, recomputed by
`tests/envelope_corpus_seal.rs` on every `cargo test`, so a local edit turns one
red oracle into a red oracle and a red seal.

## The `tapes-client` module layout

```text
crates/tapes-client/
├── contracts/tapes-api.yaml   # the sealed contract, pinned by PROVENANCE.md
└── src/
    ├── transport.rs           # THE seam — one trait both surfaces call through
    ├── error.rs               # one taxonomy: Contract/Transport/ApiStatus/Decode
    ├── decode.rs              # one decode policy (the generic-T seam)
    ├── page.rs                # one pagination convention (the cursor walk)
    ├── path.rs                # one join: PathMode::Direct | UnderBase
    ├── core/                  # the SEALED surface (table known at build time)
    │   ├── contract.rs        #   the vendored document, reduced to operations
    │   ├── coverage.rs        #   the fail-closed gate on contract bumps
    │   └── methods.rs         #   CoreClient, the call surface over a transport
    ├── cassettes/             # the DISCOVERED surface (table fetched at runtime)
    │   ├── discovery.rs       #   GET /v1/cassettes, cache + revalidation
    │   ├── cache.rs
    │   ├── spec.rs            #   a cassette's OpenAPI → method table
    │   └── invoke.rs
    ├── cli/                   # feature "cli": the generated clap surfaces
    └── http.rs                # feature "direct-http": DirectHttp
```

**The design rule, and the one thing to preserve when changing this crate:**
`core/` and `cassettes/` are **thin method tables**. Everything that could
drift lives exactly once in the floor above them — transport, error, decode,
page, path. A sealed-contract call and a discovered-cassette call go through
the identical pipeline; the only difference is where the operation table came
from.

This is not a stylistic preference. The two surfaces were previously two
crates, and every answer written twice diverged: two error vocabularies for one
API, two spellings of a URL failure, a non-success status that was rich on one
side and absent on the other, a conditional fetch one path could not express,
and a path join where only one side had learned about gateway prefixes. So:

- Adding an operation means adding a **table entry**, not a request-sending,
  status-reading, body-decoding, or URL-building code path.
- If you find yourself writing a second error variant that means what an
  existing one means, a second cursor loop, or a second `Url::join`, the change
  belongs in the floor.
- The transport seam takes a **contract-relative path**, never a URL. Base
  resolution, auth, retry, and TLS live inside a `TapesTransport` impl, which
  is what keeps consumers with wildly different delivery stories on one
  implementation of the read surface.
- Coverage tables stay with consumers. They describe one client's surface, and
  sharing them would break the gate they exist to be.

## Pull requests

Pull request titles must use one of the repository's accepted contribution
labels, such as `✨ feat:`, `🔧 fix:`, `🧹 chore:`, or `📚 docs:`, and reference
the relevant Linear issue with a magic word (e.g. `fixes PCC-123` or
`related to PCC-123`).
