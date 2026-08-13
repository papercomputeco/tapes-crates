# tapes-client

One client for the whole [Tapes](https://tapes.dev) read surface.

A tapes deployment answers two kinds of question. Some operations are a
**published contract**: sealed in the tapes repository, attached to releases as
an OpenAPI document, and therefore known to a client at build time. Others
belong to **cassettes** — independently built API extensions that core
reverse-proxies — whose set is deployment configuration and is therefore
discovered when a process starts.

Those are different facts about a server, and they used to be different crates.
That was the mistake this crate corrects. Both surfaces need the same things —
send a request, read a status, decode a body, follow a cursor, join a path onto
a base — and every one of those answers that got written twice drifted: two
error vocabularies for one API, two spellings of a URL failure, a non-success
status that was rich on one side and absent on the other, and a conditional
fetch one path could not express at all.

## The shape

```text
transport ── the seam: one trait, request in, status + bytes out
http ─────── the HTTP engine; the credential half is a small hook
error ────── one taxonomy: Contract / Transport / ApiStatus / Decode
decode ───── one policy: bytes to document, document to caller's type
page ─────── one cursor convention
path ─────── one join, in the two modes deployments actually need
   │
   ├── core/ ────── the SEALED surface: operation table AND models,
   │                both reduced from the vendored contract
   └── cassettes/ ─ the DISCOVERED surface, table from a live document
```

**The design rule:** `core` and `cassettes` are thin method tables. Everything
that could drift lives once in the floor above them. A sealed call and a
discovered call go through the identical pipeline; the only difference is where
the operation table came from. When that stops being true, the crate has stopped
doing its job.

## Public seams

**The floor** — one implementation each, shared by both surfaces:

- **`transport`** — `TapesTransport`: request in, status + headers + bytes out.
  A `WireRequest` carries a *contract-relative path*, not a URL, so base
  resolution, authentication, retry policy, and TLS all live inside an
  implementation and there is nowhere for a caller to smuggle a host into one.
  A transport can cache, multiplex, or log; it structurally cannot grow a
  semantic verb, because the frame has no vocabulary for one.
- **`error`** — the single taxonomy. `Contract` is a refusal (nothing is sent),
  `Transport` could not deliver, `ApiStatus` means the server refused and the
  body travels with the status, `Decode` means the bytes are not what was asked
  for. `#[non_exhaustive]`.
- **`decode`** — `json` (bytes to document), `typed` (document to the caller's
  type), and `json_typed` for both at once. Split so a consumer holding an
  already-decoded document reaches the same typed decode rather than a second
  one that rounds differently.
- **`page`** — the cursor walk. Absent, `null`, and `""` are three spellings of
  "last page", which is exactly why reading them belongs in one place.
- **`path`** — `call_url` with `PathMode::Direct` (a server's root) or
  `PathMode::UnderBase` (mounted under a gateway prefix). The caller says which;
  a builder that silently picks one is wrong for the other client.

**The two surfaces:**

- **`core`** — the sealed contract. `contract` reduces the vendored document to
  an operation table, `coverage` is the gate that fails a build when a contract
  bump adds an operation the client neither exposes nor deliberately
  allow-lists, `models` is that same document's response and request shapes as
  Rust types (with a gate of their own — see
  [Typed by default](#typed-by-default-generic-when-you-mean-it)), and `methods`
  is `CoreClient`, the call surface over a transport.
- **`cassettes`** — the discovered surface: `discovery` (with cache and
  `If-None-Match` revalidation), `spec` (a cassette's OpenAPI reduced to a
  method table), and `invoke`.

**Behind features:**

- **`cli`** — synthesizing clap commands from a discovered surface and resolving
  a parsed match back into a call. It stops at `resolve_invocation`; executing
  and printing stay with the consumer.
- **`http`** — `HttpEngine`, the HTTP half of a transport, and `HttpAuth`, the
  credential half a consumer supplies. `DirectHttp` is the engine with `NoAuth`
  in it: an unauthenticated transport for one tapes server. Redirects are
  refused rather than followed, in two layers: the engine's own client is built
  with `Policy::none`, *and* every response is checked to have come from the
  configured origin — the layer that still holds when the client was injected.
  See [Authenticating](#authenticating-a-hook-not-a-transport).

## Features

| feature | default | what it does |
| --- | --- | --- |
| `cli` | on | The generated clap surfaces. Off, the operation tables and the transport seam are unchanged — only command synthesis goes away, and clap is never compiled. A consumer embedding this in a GUI takes `--no-default-features`. |
| `direct-http` | on | The in-crate HTTP engine — `HttpEngine`, `HttpAuth`, `DirectHttp`. Off, the crate has no HTTP client at all and a consumer plugs its own into `TapesTransport`. |

Both are additive, and neither is load-bearing for the crate's own logic.

## Typed by default, generic when you mean it

`CoreClient`'s named methods return the models in `core::models`:

```rust,ignore
let client = CoreClient::new(DirectHttp::new(base));

let page = client.list_sessions(&SessionListParams { limit: Some(25), ..Default::default() }).await?;
let all  = client.list_all_sessions(&SessionListParams::default()).await?; // follows next_cursor
let one  = client.get_session("s-1").await?;
let spans = client.get_session_traces("s-1", &SessionTracesParams { payload: Some(PayloadDetail::Preview) }).await?;
```

The shape of a sealed response is not a consumer's opinion — it is published,
vendored here, and held to the document by a build-time gate. Every client that
modelled it privately was keeping a second copy of a shared fact.

**The escape hatch is still there**, one layer down, and it is not the default:

```rust,ignore
let document: serde_json::Value = client.call("listSessions", vec![("limit", "25".into())]).await?;
```

`CoreClient::call` is generic in its response type and reaches *every* operation
by `operationId`, including the ones no named method covers. Reach for it in two
places: an operation this crate has not typed yet, and the fidelity reads —
export, raw turns — where a typed decode would silently write an archive of the
fields this build happened to know about.

The models decode permissively on purpose. Unknown fields pass (a newer server
is not a malformed response), an absent field takes its default (the contract
requires nothing), and a `null` in an array, map, or nested object decodes as
empty rather than failing the whole document. What catches an added field is not
the runtime — it is `core::models::coverage`, at build time, where somebody can
decide about it: the gate synthesises a document from each schema, round-trips
it through the model, and names by path anything the model does not carry.

## Authenticating: a hook, not a transport

The tapes read API carries no authentication of its own. A consumer that holds a
credential does so because *its* edge demands one — and it used to pay for that
by implementing `TapesTransport` outright: verb parsing, path joining, header
copying, redirect policy, response splitting, error mapping, and the whole thing
again for streaming. The only part of that which was genuinely its own was the
credential.

So write the credential, not the transport:

```rust,ignore
struct MintedToken { /* whatever produces a credential */ }

impl HttpAuth for MintedToken {
    async fn authorize(&self, _request: &WireRequest<'_>, _attempt: u32)
        -> Result<Vec<(String, String)>, TransportError>
    {
        let token = self.mint().await.map_err(|e| TransportError::with_source("mint failed", e))?;
        Ok(vec![("x-tapes-auth".into(), format!("Bearer {token}"))])
    }

    async fn on_unauthorized(&self, rejected: Rejected<'_>) -> Unauthorized {
        // The policy is data; the loop is the engine's.
        if rejected.attempt == 1 { Unauthorized::Retry } else { Unauthorized::Surface }
    }
}

let engine = HttpEngine::with_auth(base, MintedToken { .. })
    .under_base()             // mounted behind a gateway prefix
    .with_client(my_client);  // your TLS, proxy, or connection pool
```

- `authorize` runs **once per attempt**, so a consumer that mints per request
  keeps doing exactly that — including on the retry.
- `on_unauthorized` returns a *decision*: retry, surface the 401 to the caller
  with the body that explains it, or fail with an error of the consumer's own
  ("run the login command" is a fact a user can act on; `401` is not). The
  engine owns the loop and caps it, so a retry policy is data rather than a
  `while` loop rewritten per consumer with its own answer to "how many times?".
- The HTTP client is a constructor argument, not a trait method, because TLS is
  a property of the client and not of the credential: a deployment that pins a
  root but sends no credential should not have to implement an auth trait to say
  so.

`TapesTransport` is unchanged and still open: a consumer whose transport is a
local socket, a test double, or anything that is not HTTP implements it directly,
and the engine is simply one implementation of it.

## What is not here

No notion of a tenant, and no opinion about how a response is rendered.

No credential, either — `HttpAuth` is the shape of the hole where one goes, and
the engine never holds, caches, or refreshes anything. What changed is that the
*HTTP around* a credential is no longer a consumer's problem.

It also does not hold the **operation** coverage tables. Those describe *one
client's* surface, and sharing them would make the gate report on a union and
protect nobody. The **schema** coverage tables are the opposite case and do ship
here, because the models they describe ship here too.

## The vendored contract

`contracts/tapes-api.yaml` is a copy of a published release asset, pinned by
fingerprint in `contracts/PROVENANCE.md`. `make contracts-check` at the
repository root verifies it against both that fingerprint and the published
asset; CI runs it with `TAPES_CONTRACT_STRICT=1`, so a gate that cannot reach
its input fails rather than reporting a comparison it never made.

## Migrating from `tapes-read-contract` / `tapes-cassette-client`

This crate absorbed both of them. Each name survived for one step as a
re-export shim so that a consumer pinning it compiled across the move; the
shims are gone now, and this section is where their item-by-item tables live.

### From `tapes-read-contract`

| was | is |
| --- | -- |
| `contract` | `tapes_client::core::contract` |
| `coverage` | `tapes_client::core::coverage` |
| `invoke` | `tapes_client::path` |
| `error` | `tapes_client::error` |
| `transport` | `tapes_client::transport` and `tapes_client::core::methods` |
| `invoke::call_url` | `tapes_client::path::call_url` |
| `invoke::PathMode::RootAbsolute` | `tapes_client::path::PathMode::Direct` |
| `invoke::PathMode::UnderBase` | `tapes_client::path::PathMode::UnderBase` |

`PathMode`'s default variant was **renamed**: `RootAbsolute` is now `Direct`.
The behaviour is identical — still the default, still the join that drops any
path prefix the base carried — so this is one word at each call site, and it is
the only rename in either table.

The `ReadTransport` / `ReadOperations` pair did not survive the move. It was a
second seam describing the same thing as the cassette client's, which is
precisely the duplication the merge exists to remove; its replacement is
`tapes_client::transport::TapesTransport`, with `tapes_client::core::CoreClient`
as the call surface over it. Nothing consumed the old pair.

### From `tapes-cassette-client`

| was | is |
| --- | -- |
| `cache` | `tapes_client::cassettes::cache` |
| `command` | `tapes_client::cli` |
| `discovery` | `tapes_client::cassettes::discovery` |
| `spec` | `tapes_client::cassettes::spec` |
| `invoke` | `tapes_client::path` and `tapes_client::cassettes::invoke` |
| `transport` | `tapes_client::transport` and `tapes_client::http` |
| `invoke::call_url(base, call)` | `tapes_client::path::call_url(base, call, PathMode::Direct)` |

`call_url` gained its `PathMode` argument because a client mounted under a
gateway prefix and one addressed at a server's root are not the same join. That
crate only ever performed the root-absolute one, so `PathMode::Direct`
reproduces its behaviour exactly.

### The one type that could not be re-exported

`tapes_cassette_client::Error` was preserved verbatim and inert rather than
aliased to the merged taxonomy, for a reason that is a property of Rust rather
than a preference: a consumer matching it *exhaustively* — no wildcard arm —
stops compiling the moment a variant is added or removed, and one taxonomy for
one API necessarily has both more variants and different ones.

There is now a single `tapes_client::Error`, described under
[Public seams](#public-seams). A consumer moving across deletes its
`From<tapes_cassette_client::Error>` implementation and writes one for the
merged variants — the same work either way, at a time it chooses.

## Shedding an adapter

If you carry a `TapesTransport` implementation of your own, or response types
that mirror the sealed contract, both can now go. The move is mechanical, and
each step stands alone — nothing here has to be done in one change.

**1. A hand-written HTTP transport becomes a hook.** Keep whatever mints your
credential; delete everything around it.

| what your adapter does today | where it goes |
| --- | --- |
| parse the verb, join the path, copy headers, set the content type | gone — `HttpEngine` |
| `PathMode::UnderBase` because you sit behind a gateway | `.under_base()` |
| a `reqwest::Client` you built (TLS, proxy, pool) | `.with_client(client)` |
| attach `Authorization` / a bespoke auth header | `HttpAuth::authorize` |
| retry once on a 401, then surface your own error | `HttpAuth::on_unauthorized` returning `Retry`, then `Fail(..)` |
| split status/headers/bytes; map failures into the taxonomy | gone — `HttpEngine` |
| the whole thing again for streaming, plus "a non-success status is an error" | gone — `HttpEngine` implements `StreamingTransport` |

What you are left with is the `impl HttpAuth` above, and
`HttpEngine::with_auth(base, hook)` where the adapter used to be. The engine
implements `TapesTransport` and `StreamingTransport`, so everything downstream —
`CoreClient`, the cassette cache, `Wire` — takes it unchanged.

Two behaviours may be *new* to you, both deliberately: redirects are refused
(with a per-response origin check that also covers an injected client), and a
streamed non-success status becomes `Error::ApiStatus` instead of a readable
body. The second is what stops an export writing a JSON error page into the file
a user asked for.

**2. Private response types become the shipped models.** Replace your own
`SessionItem`/`SpanItem`/… with `tapes_client::core::models`, and your query
parameter builders with the `*Params` structs. Two things to expect:

- Response models are `#[non_exhaustive]`: decode them, do not construct them.
  Request bodies (`CreateSkillRequest`, `SessionUpdateRequest`, …) are yours to
  build and are not marked, down to their nested components.
- The partial-update bodies carry `Option` fields, and an unset one is absent
  from the bytes rather than sent empty. That is what makes
  `UpdateSkillRequest { name: Some(..), ..Default::default() }` a rename
  instead of a rename plus the erasure of everything it did not mention — the
  server applies the properties the body carries and leaves the rest alone.
- Timestamps and enumerable strings stay `String`. The contract declares them
  that way, and a typed decode that rejected an unparseable timestamp — or an
  unfamiliar `status` — would blank a whole page over an additive change.

Anything you decode as `serde_json::Value` today for **fidelity** should stay
that way. `CoreClient::call` is generic exactly so it can.

## Stability

This crate is **supported public API**, meant to be depended on directly. So
are its two siblings — [`tapes-capture`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-capture/README.md) (the
capture protocol) and [`tapes-harnesses`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-harnesses/README.md) (the
harness knowledge) — and all three version independently on crates.io. This one
sits on neither side of the repository's single dependency edge, so its releases
are ordered against nothing.

Pre-1.0, `0.x` versions carry the usual Cargo meaning: a breaking change bumps
the minor (`0.2.0`), anything compatible bumps the patch (`0.1.1`). What counts
as breaking is the boundary in the [repository README](https://github.com/papercomputeco/tapes-crates/blob/main/README.md#the-public-api-boundary),
not just the signatures: authentication, tenancy, transport, and rendering are
outside this crate by design, and growing one of them here would break the
promise while compiling cleanly.

`Error` is `#[non_exhaustive]`, so a new variant is an additive change — match
it with a wildcard arm. The sealed surface moves when the vendored contract
under `contracts/` is refreshed, which can add or change operations without a
line of Rust changing; those refreshes are versioned like any other change.

Changes are recorded in [`CHANGELOG.md`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-client/CHANGELOG.md).

## License

Dual-licensed under MIT OR Apache-2.0; see the repository root.
