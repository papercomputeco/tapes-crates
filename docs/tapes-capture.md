---
title: tapes-capture
description: The capture protocol crate — the X-Tapes-* envelope and its budgets, the gateway environment contract with its two sub-protocols, and the peer-trust primitives.
sidebar:
  order: 2
---

`tapes-capture` is the half of capture that no harness changes. Nothing here
may learn a harness's name — the moment it does, it has stopped being the
thing every harness shares. Where capture needs something *from* a harness, it
declares the `HarnessSession` trait and
[tapes-harnesses](./tapes-harnesses.md) implements it; the dependency edge
runs one way and Cargo enforces it rather than review.

```bash
cargo add tapes-capture
```

Full API documentation is on
[docs.rs](https://docs.rs/tapes-capture/latest/tapes_capture/). This page is
the contract-level reference: the envelope, its budgets and invariants, the
gateway environment, and the fixture corpus that seals it all.

## The envelope

The `X-Tapes-*` request-header contract carries attribution and provenance
from a capture transport — a client's just-in-time proxy, a long-lived daemon
client, or a server-side gateway filter — into the tapes ingest server. It is
a cross-language contract: produced here in Rust, parsed by the Go
implementations in the tapes server's ingest and gateway capture. Both halves
table-test against one shared fixture corpus (see
[The fixture corpus](#the-fixture-corpus)).

### The headers

| header | required | value |
| --- | --- | --- |
| `x-tapes-harness-id` | **always** | The harness id — the one mandatory header. |
| `x-tapes-harness-session-id` | when the id is not `unknown` | Opaque harness-side session identifier. |
| `x-tapes-harness-version` | no | Harness version string. |
| `x-tapes-cwd` | no | Working directory, percent-encoded UTF-8. |
| `x-tapes-session-name` | no | User-given session name, percent-encoded, capped at 256 raw bytes. |
| `x-tapes-parent-harness-session-id` | no | Fork-parent's harness session id, when the client recovered lineage. |
| `x-tapes-harness-metadata` | no | base64url (no padding) of a JSON object, capped at 4 KiB raw, dropped first under budget pressure. |

The harness-id vocabulary is declared in this crate: `unknown`, `claude`,
`codex`, `codex-app`, `opencode`, `pi`. The arrow points the way that first
looks backwards — the harness registry in `tapes-harnesses` takes its ids
*from* this list rather than declaring them and having the envelope import
them back. Harness ids are envelope vocabulary, they are what goes on the
wire, and that direction is what lets this crate name them without depending
on any harness.

### The invariants

1. **Budget.** The total `X-Tapes-*` byte budget is 8 KiB
   (`X_TAPES_TOTAL_BUDGET`). Headers are inserted in a fixed order — harness
   id, then the plain fields, then metadata last — and the metadata header is
   dropped *before* insertion when it would push the total over, so drop
   semantics survive reordering. The metadata blob is separately capped at
   4 KiB of raw JSON (`X_TAPES_METADATA_RAW_CAP`); an oversize blob drops the
   whole header rather than travelling truncated.
2. **Encoding.** Session name and working directory may carry arbitrary UTF-8,
   which RFC 7230 forbids in raw header values, so both are percent-encoded.
   Session names beyond 256 raw bytes are truncated to the cap, walking back
   to a UTF-8 boundary. Metadata is base64url without padding.
3. **Fail to sentinel, never fail the request.** An optional field with bytes
   invalid in an HTTP header is silently omitted. A failure on the required
   header wipes the partial envelope and substitutes `unknown`. The guarantee
   is that `X-Tapes-Harness-Id` always ships; capture degrades, forwarding
   never breaks.
4. **The completeness rule.** An inbound envelope is believed only when the
   harness id is present *and not* `unknown` *and* the session id is present
   and non-blank. `TapesAttribution::from_headers` is the single
   implementation of that rule, and `inject_unattributed_envelope` uses the
   same answer to decide whether to *preserve* an inbound envelope (a
   self-attributing harness knows more than a failed lookup does) or replace
   it with the sentinel.

The completeness rule alone is not a security boundary. Trusting an inbound
envelope is safe only in combination with the launch nonce below: the
peer-trust ancestry walk cannot distinguish the harness from the harness's
own subprocesses, and the nonce is what proves the envelope came from the
process that was launched.

This compiles and runs against the published crate:

```rust
use http::HeaderMap;
use tapes_capture::envelope::{self, TapesAttribution};
use tapes_capture::{provider_route, split_provider_route};

fn main() {
    // A request nobody could attribute still ships an envelope:
    // exactly `X-Tapes-Harness-Id: unknown`, and nothing else.
    let mut headers = HeaderMap::new();
    if envelope::inject_unattributed_envelope(&mut headers).is_err() {
        return; // unreachable in practice: the sentinel is ASCII
    }

    // The readback applies the producer's own completeness rule:
    // a sentinel envelope is not a believable identity.
    assert!(TapesAttribution::from_headers(&headers).is_none());

    // Per-provider routing labels the path; the split is its inverse.
    assert_eq!(provider_route("anthropic"), "/_tapes/provider/anthropic");
    assert_eq!(
        split_provider_route("/_tapes/provider/anthropic/v1/messages"),
        Some(("anthropic", "/v1/messages"))
    );
}
```

The full producer surface — `inject_session_envelope` for a resolved session,
`TapesAttribution` and its constructors, the header-name and cap constants —
is documented on
[docs.rs](https://docs.rs/tapes-capture/latest/tapes_capture/envelope/).

### The request-capture cap

`REQUEST_CAPTURE_CAP` (32 MiB, in wire bytes) is the largest request body a
capture client should retain for capture before degrading to forward-only. It
matches the gateway side of the same contract, so client capture never
silently records less than a server-side gateway captures for the same
traffic. It is capture-only: forwarding must never gate on this value.

## The gateway contract

The `gateway` module is the wire/environment agreement between a launching
capture client and whatever runs inside the harness on the other end. It is
two sub-protocols, and a reader who takes it for one will be surprised by the
other:

1. **Launch and trust.** `TAPES_GATEWAY_URL` names the proxy,
   `TAPES_GATEWAY_SCHEMA` hints at which upstream schema it fronts (a display
   hint — a plugin must not gate the redirect on it), and
   `TAPES_GATEWAY_NONCE` carries a per-launch secret that an installed plugin
   echoes back in the nonce header. `nonce_matches` is the constant-time
   comparison that decides whether the echo counts. An installed plugin must
   read the nonce once at load and delete it from its process environment
   immediately, before any tool can run, so the harness's own subprocesses
   never receive it.
2. **Per-provider routing.** A plugin can register more providers than a
   single-upstream proxy can serve. `TAPES_GATEWAY_PROVIDER_ROUTES` set to `1`
   tells the plugin to label each request's path with the provider it belongs
   to, under `/_tapes/provider/<name>`; `provider_route` builds the labelled
   base URL and `split_provider_route` is the proxy-side inverse. Unset, a
   plugin registers everything at the base URL unchanged — which is exactly
   what a client predating this protocol gets.

Four of the seven gateway environment variables live here, because they are
protocol. The other three (`TAPES_GATEWAY_LABEL`, `TAPES_GATEWAY_LABEL_SUFFIX`,
`TAPES_GATEWAY_REMEDY`) are presentation — what a product calls itself in a
harness's status bar — and live with the artifact that reads them, in
`tapes-harnesses`. The full seven-variable table a launching consumer needs is
in [tapes-harnesses](./tapes-harnesses.md#plugins-and-the-gateway-environment-a-launcher-sets).

## Peer trust

Two modules answer the question every capture client asks before it believes
anything a connection tells it about itself, and neither has ever needed a
harness id to answer it:

- **`peer_pid`** maps an accepted loopback connection to one of a candidate
  PID set, via per-OS kernel APIs.
- **`peer_trust`** is the ancestry walk: is the process on the other end the
  harness this client launched, or one of its descendants?

A descendant is deliberately as far as the walk can go — a command run by a
shell tool is a descendant of the launched PID too, which is why a
self-attributing harness's envelope additionally needs the nonce echo before
it is believed.

## The fixture corpus

The envelope contract is sealed by a shared fixture corpus, authored in the
tapes repository and vendored here under
[`crates/tapes-capture/vendor/tapes-envelope-fixtures/`](https://github.com/papercomputeco/tapes-crates/tree/main/crates/tapes-capture/vendor/tapes-envelope-fixtures).
Three rules make several vendored copies one corpus:

- The corpus is vendored into **every** implementation of the contract, in
  every language, and all copies must move together from one upstream
  revision. A copy that moves alone is a test suite going green against bytes
  no other implementation has ever seen.
- `DIGEST` makes "the same corpus" checkable rather than asserted: sort the
  case files by base name, feed `"<basename>  <sha256>\n"` for each into
  SHA-256, and compare. The recipe is deliberately trivial so each language
  restates it in a few lines rather than sharing an implementation that would
  itself need vendoring. A test recomputes it on every `cargo test`.
- Cases carry a direction — `roundtrip`, `encode`, or `decode` — saying which
  half of the contract asserts them. A producer runs the first two and skips
  the third by design; a parser runs the first and third.

Consumers can table-test their own envelope composition against the same
corpus: the `envelope-fixtures` feature exposes `envelope::fixtures`, the
crate's reader for the vendored cases. The feature is off by default and must
stay that way — the reader panics by design on a malformed corpus, which does
not belong in a production build. Enable it under `[dev-dependencies]`:

```toml
[dev-dependencies]
tapes-capture = { version = "0.1", features = ["envelope-fixtures"] }
```

The corpus resolves by a path relative to the crate manifest and ships inside
the packaged crate, so the reader works from a crates.io dependency and from a
git checkout alike. The crate's docs.rs pages are built with every feature on,
so [`envelope::fixtures`](https://docs.rs/tapes-capture/latest/tapes_capture/envelope/fixtures/)
renders there whether or not you have enabled it.

Do not change producer behaviour without updating the shared corpus in the
tapes repository first, then re-vendoring. If a change makes the oracle fail,
the contract conversation happens in tapes — not by editing the vendored
fixtures, which the seal turns into a red test naming the file.
