---
title: tapes-client
description: The read client — one core contract sealed at build time, a deployment's cassettes discovered at start, one transport seam, and a credential hook instead of a transport.
sidebar:
  order: 4
---

`tapes-client` is one client for the whole tapes read surface. A tapes
deployment answers two kinds of question: some operations are a **published
contract** — sealed in the tapes repository, attached to releases as an
OpenAPI document, and therefore known to a client at build time — while
others belong to **cassettes**, independently built API extensions whose set
is deployment configuration and is therefore discovered when a process
starts.

Both surfaces are thin method tables over one shared floor: one transport
seam, one error taxonomy, one decode policy, one pagination convention, one
path join. A sealed call and a discovered call go through the identical
pipeline; the only difference is where the operation table came from.

```bash
cargo add tapes-client
```

Full API documentation is on
[docs.rs](https://docs.rs/tapes-client/latest/tapes_client/).

## A first read

This compiles as-is against the published crate (with `url` and `tokio` as
dependencies). Running it needs the **read listener** of a live tapes
deployment — reads are served on the read port (8081 in a default local
deployment), not the ingest port:

```rust
use tapes_client::models::SessionListParams;
use tapes_client::{CoreClient, DirectHttp};
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The read listener of a running tapes deployment.
    let base: Url = "http://127.0.0.1:8081".parse()?;
    let client = CoreClient::new(DirectHttp::new(base));

    let params = SessionListParams { limit: Some(25), ..Default::default() };
    let page = client.list_sessions(&params).await?;
    for session in page.items {
        println!("{}  {}  {}", session.id, session.harness_id, session.display_title);
    }
    Ok(())
}
```

`DirectHttp` is the crate's own HTTP engine with no credentials in it — the
right transport for a tapes server reached directly. When your deployment
sits behind an authenticating edge, see [Authenticating](#authenticating-a-hook-not-a-transport).

## The sealed core

`core` is the published contract, vendored into the crate as
`contracts/tapes-api.yaml` and pinned by fingerprint. `CoreClient`'s named
methods return the typed models in `core::models`; the shape of a sealed
response is not a consumer's opinion — it is published, vendored, and held to
the document by build-time gates.

**Typed by default, generic when you mean it.** `CoreClient::call` is generic
in its response type and reaches *every* operation by `operationId`,
including ones no named method covers. Reach for it in two places: an
operation the crate has not typed yet, and the fidelity reads — export, raw
turns — where a typed decode would silently write an archive of only the
fields this build happened to know about.

The models decode permissively on purpose: unknown fields pass (a newer
server is not a malformed response), an absent field takes its default, and a
`null` in an array or map decodes as empty rather than failing the whole
document. What catches an added field is not the runtime but a build-time
gate.

**Two gates, easy to conflate, failing differently.** `core::coverage` gates
*operations*: it fails a build when a contract bump adds an operation the
client neither exposes nor deliberately allow-lists. `core::models::coverage`
gates *shapes*: it synthesises a document from each schema, round-trips it
through the model, and names by path anything the model drops. An operation
gap is a call you cannot make; a shape gap is a field you silently lose.

The sealed surface moves when the vendored contract is refreshed, which can
add or change operations without a line of Rust changing; those refreshes are
versioned like any other change.

## Cassettes: the discovered surface

`cassettes` drives whatever command surface a deployment serves beyond the
core contract: `discovery` fetches and caches the deployment's discovery
document (with `If-None-Match` revalidation), `spec` reduces a cassette's
OpenAPI to a method table, and `invoke` makes the call — through the same
floor as every sealed call. What cassettes are and how a deployment serves
them is server-side documentation:
[tapes.dev/docs/tapes/](https://tapes.dev/docs/tapes/).

Behind the `cli` feature, the crate can synthesize clap commands from a
discovered surface and resolve a parsed match back into a call. It stops at
`resolve_invocation`; executing and printing stay with the consumer.

## The transport seam

`transport::TapesTransport` is the one seam: request in, status + headers +
bytes out. A `WireRequest` carries a *contract-relative path*, not a URL, so
base resolution, authentication, retry policy, and TLS all live inside an
implementation and there is nowhere for a caller to smuggle a host into one.
A transport can cache, multiplex, or log; it structurally cannot grow a
semantic verb, because the frame has no vocabulary for one.

A consumer whose transport is not HTTP — a local socket, a test double —
implements the trait directly; the HTTP engine below is simply one
implementation of it.

## Authenticating: a hook, not a transport

The tapes read API carries no authentication of its own. A consumer that
holds a credential does so because *its* edge demands one — and the only part
of a hand-written HTTP transport that was ever genuinely its own was the
credential. So write the credential, not the transport: implement
`http::HttpAuth`, whose two methods are

- `authorize` — produce headers for one attempt. It runs **once per
  attempt**, so a consumer that mints per request keeps doing exactly that,
  including on the retry.
- `on_unauthorized` — a *decision*, not a loop: retry, surface the rejection
  with the body that explains it, or fail with an error of the consumer's own
  ("run the login command" is a fact a user can act on; `401` is not). The
  engine owns the loop and caps the attempts.

`HttpEngine::with_auth(base, hook)` is the engine around it; `.under_base()`
selects the path join for a client mounted behind a gateway prefix, and
`.with_client(..)` injects your own TLS, proxy, or connection pool. Redirects
are refused rather than followed, in two layers — the engine's client policy,
and a per-response origin check that still holds when the client was
injected. A streamed non-success status becomes an error instead of a
readable body, which is what stops an export writing a JSON error page into
the file a user asked for.

The full worked example is in the crate's
[README](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-client/README.md#authenticating-a-hook-not-a-transport)
and on [docs.rs](https://docs.rs/tapes-client/latest/tapes_client/http/).

## The floor

- **`error`** — one taxonomy, `#[non_exhaustive]`: `Contract` is a refusal
  (nothing was sent), `Transport` could not deliver, `ApiStatus` means the
  server refused and the body travels with the status, `Decode` means the
  bytes are not what was asked for. Match with a wildcard arm.
- **`decode`** — `json` (bytes to document), `typed` (document to the
  caller's type), and `json_typed` for both at once.
- **`page`** — the cursor walk. Absent, `null`, and `""` are three spellings
  of "last page", which is exactly why reading them belongs in one place.
  `list_all_sessions` and its siblings follow `next_cursor` to the end
  through this one convention.
- **`path`** — `call_url` with `PathMode::Direct` (a server's root) or
  `PathMode::UnderBase` (mounted under a gateway prefix). The caller says
  which; a builder that silently picks one is wrong for the other client.

## Features

| feature | default | what it does |
| --- | --- | --- |
| `cli` | on | The generated clap surfaces. Off, only command synthesis goes away and clap is never compiled. |
| `direct-http` | on | The in-crate HTTP engine — `HttpEngine`, `HttpAuth`, `DirectHttp`. Off, the crate has no HTTP client at all and a consumer plugs its own into `TapesTransport`. |

Both are additive, and neither is load-bearing for the crate's own logic.

## What is not here

No notion of a tenant, no opinion about rendering, and no credential —
`HttpAuth` is the shape of the hole where one goes, and the engine never
holds, caches, or refreshes anything.

## Migrating from the merged crates

`tapes-client` absorbed two earlier crates, `tapes-read-contract` and
`tapes-cassette-client`. The item-by-item mapping — including the one renamed
variant and the one type that could not be re-exported — lives in the crate's
[README](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-client/README.md#migrating-from-tapes-read-contract--tapes-cassette-client),
which also walks shedding a hand-written transport adapter step by step.
