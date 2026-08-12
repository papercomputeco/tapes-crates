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
error ────── one taxonomy: Contract / Transport / ApiStatus / Decode
decode ───── one policy: bytes to document, document to caller's type
page ─────── one cursor convention
path ─────── one join, in the two modes deployments actually need
   │
   ├── core/ ────── the SEALED surface, table from the vendored contract
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
  allow-lists, and `methods` is `CoreClient`, the call surface over a transport.
- **`cassettes`** — the discovered surface: `discovery` (with cache and
  `If-None-Match` revalidation), `spec` (a cassette's OpenAPI reduced to a
  method table), and `invoke`.

**Behind features:**

- **`cli`** — synthesizing clap commands from a discovered surface and resolving
  a parsed match back into a call. It stops at `resolve_invocation`; executing
  and printing stay with the consumer.
- **`http`** — `DirectHttp`, an unauthenticated transport for one tapes server.
  Redirects are refused rather than followed, in two layers: the client is built
  with `Policy::none`, *and* every response is checked to have come from the
  configured origin.

## Features

| feature | default | what it does |
| --- | --- | --- |
| `cli` | on | The generated clap surfaces. Off, the operation tables and the transport seam are unchanged — only command synthesis goes away, and clap is never compiled. A consumer embedding this in a GUI takes `--no-default-features`. |
| `direct-http` | on | The in-crate `DirectHttp` transport. Off, the crate has no HTTP client at all and a consumer plugs its own into `TapesTransport`. |

Both are additive, and neither is load-bearing for the crate's own logic.

## What is not here

No authentication, no notion of a tenant, no opinion about how a response is
rendered, and — beyond `decode`'s policy — no view on whether a response is
typed at all. Each of those is a consumer's, and each consumer's answer is
different. The one transport this crate does ship carries no credential.

It also does not hold the coverage tables. Those describe *one client's*
surface; sharing them would break the gate they exist to be.

## The vendored contract

`contracts/tapes-api.yaml` is a copy of a published release asset, pinned by
fingerprint in `contracts/PROVENANCE.md`. `make contracts-check` at the
repository root verifies it against both that fingerprint and the published
asset; CI runs it with `TAPES_CONTRACT_STRICT=1`, so a gate that cannot reach
its input fails rather than reporting a comparison it never made.

## License

Dual-licensed under MIT OR Apache-2.0; see the repository root.
