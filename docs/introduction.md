---
title: tapes-crates
description: The client-side Rust crates for tapes — the crate map, the boundary each crate owns, and how to take them from crates.io.
sidebar:
  order: 1
---

`tapes-crates` holds the client-side Rust crates for
[tapes](https://tapes.dev): what a coding-agent harness needs in order to run
under capture, what capture puts on the wire, and how a client reads the
results back.

Three published crates and one internal test-support crate, in one workspace.
They are consumed by `tapesctl` and by closed-source clients alike, which is
the point: parity between two clients' `start` commands is structural, not
policed, because the same code runs in both. A behaviour that lives here
cannot differ between clients; a behaviour that lives in a client can, and
that is the test for whether something belongs in this repository at all.

## Install

All three crates are on crates.io and version independently:

```bash
cargo add tapes-capture     # the capture protocol
cargo add tapes-harnesses   # harness knowledge; requires tapes-capture
cargo add tapes-client      # the read surface
```

API documentation is on docs.rs:
[tapes-capture](https://docs.rs/tapes-capture/latest/tapes_capture/),
[tapes-harnesses](https://docs.rs/tapes-harnesses/latest/tapes_harnesses/),
[tapes-client](https://docs.rs/tapes-client/latest/tapes_client/).

## The names

Four similar names mean four different things, so here is the one
disambiguation this documentation makes; every page after this uses the
precise name.

- **`tapes-crates`** is the repository —
  [github.com/papercomputeco/tapes-crates](https://github.com/papercomputeco/tapes-crates).
- **`tapes-harnesses`** is one crate inside it.
- **`tapes-harness`** is not a crate. The singular spelling is reserved on
  crates.io as a stub redirect so the near-miss cannot be claimed by someone
  else.
- **`tapes`** is a different repository entirely: the server these clients
  capture to and read from, documented at
  [tapes.dev/docs/](https://tapes.dev/docs/). It is also the
  authoring home of the envelope fixture corpus vendored here.

## The crate map

Each crate owns one question. These boundaries are the contract this
repository publishes — a change that moves a responsibility across one of
these lines is a breaking change even when every signature still compiles.

| crate | owns | does **not** own |
| --- | --- | --- |
| [`tapes-capture`](./tapes-capture.md) | The capture protocol: the `X-Tapes-*` envelope producer and the harness-id vocabulary it stamps, the capture-gateway environment contract, the launch-nonce protocol, peer-PID lookup, and the peer-trust ancestry walk. | Any harness's name, and any knowledge that arrives *because* a harness was added. |
| [`tapes-harnesses`](./tapes-harnesses.md) | Harness launch and attribution knowledge: the registry, launch recipes, config patch grammars, plugin artifacts, per-harness attribution lanes, transcript discovery and packaging. | Anything true of every harness — that is `tapes-capture`. |
| [`tapes-client`](./tapes-client.md) | The read surface: the sealed core contract and a deployment's discovered cassettes, driven over one transport seam. | Authentication, tenancy, transport, and rendering. Each is a consumer's, and each consumer's answer differs. |

## The membership tests

The boundary between the first two crates is one question: **would adding one
more harness change this?** If yes it is `tapes-harnesses`; if no it is
`tapes-capture`. The dependency edge runs one way and Cargo enforces it rather
than review — `tapes-harnesses` depends on `tapes-capture`, never the reverse.
The moment a capture primitive knows a harness's name it has stopped being the
thing every harness shares. Where capture needs something *from* a harness —
the envelope needs a session's fields — it declares a trait and the harness
crate implements it.

The test for `tapes-client` is different, because it is not split by subject
matter but by *when the operation table is known*: the core contract is sealed
at build time from a vendored document, a deployment's cassettes are
discovered at process start. Both halves are thin method tables over one
shared floor — one transport seam, one error taxonomy, one decode policy, one
pagination convention, one path join.

## The fourth crate

The repository contains a fourth crate, `tapes-mock-upstream`. It is internal
test support — a streaming mock provider upstream, a mock ingest server, and
the scripted recipes behind the
[harness regression matrix](./harness-matrix.md) — and it is never released:
the release workflow accepts tags for the three published crates only. It is a
crate rather than a test module because the matrix launches real harness
binaries *through* `tapes-harnesses` rather than inside it, and an integration
test in a sibling crate can only reach items the tested crate exports.
Consumers who want it for their own tests take it as a git dependency under
`[dev-dependencies]`; it makes no stability promise.

## Versioning

Pre-1.0, `0.x` versions carry the usual Cargo meaning: a breaking change bumps
the minor (`0.2.0`), anything compatible bumps the patch (`0.1.1`). What
counts as breaking is the crate-map boundary above, not just the signatures.
Each crate keeps its own `CHANGELOG.md` beside its source; the release order
and tag scheme are in [Releasing](./releasing.md).

## The rest of this documentation

- [tapes-capture](./tapes-capture.md) — the envelope, its budgets, the
  gateway contract, and the fixture corpus.
- [tapes-harnesses](./tapes-harnesses.md) — the registry, the three capture
  mechanisms, launch recipes, plugins, attribution, transcripts.
- [tapes-client](./tapes-client.md) — the two read surfaces, the transport
  seam, and the credential hook.
- [Adding a harness](./adding-a-harness.md) — the walkthrough for teaching
  the crates about a new coding agent.
- [The harness regression matrix](./harness-matrix.md) — the CI tier that
  launches real harness binaries against mock endpoints.
- [Releasing](./releasing.md) — dependency order, tags, and the gates that
  keep the crates publishable.
