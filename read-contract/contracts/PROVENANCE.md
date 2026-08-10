# Vendored tapes read contract

`tapes-api.yaml` beside this file is the published tapes read-API OpenAPI
contract, vendored byte-for-byte. `src/contract.rs` embeds it and reduces it
with the same OpenAPI→CLI reducer the runtime-discovered cassette surface uses;
a consumer's read commands build their requests from it rather than from
hand-written URL builders.

It lives here, in the shared crate, rather than in each client: both clients
build against a published release asset, never against the tapes working tree,
so one vendored copy is enough — and two copies is a re-pin that takes two PRs
in two repositories which nothing checks for agreement.

The ingest write surface (`tapes-ingest.yaml`) is deliberately **not** here. It
is read only by `tapesctl`'s capture conformance tests, no other client vendors
it, and nothing at runtime reads it; it stays with its one consumer.

## Pin

- Release tag: **v0.34.0** — papercomputeco/tapes, commit `94b2ec7`
  ("feat: MCP cassettes (#289)"). This is the first tapes release that
  attaches the compiled contracts as assets.
- Vendored from the release asset, byte-for-byte:
  - <https://github.com/papercomputeco/tapes/releases/download/v0.34.0/tapes-api-v0.34.0.yaml>
- The asset is what `tapes dev openapi api --docs-root . --out <file>` emits at
  the tag — the exact command `dagger call contracts` (`make contracts` in
  tapes) runs; a local emission at `94b2ec7` was verified byte-identical to the
  asset.

## Fingerprints

Vendored file bytes (what `scripts/contracts-check.sh` verifies):

- `tapes-api.yaml` sha256 `e6b358bdb5169475f24ea946cbf8e8567ca85240e863b744e8e2f33320a29bab`

Prose-included document fingerprint (`CompiledDoc.Fingerprint()` as printed by
`tapes dev openapi`; the ETag a server would serve for the same document):

- api `sha256:9da9223f51ab7d3c0333725f6abfd8279465c7d5e970e1e8a589fce789362853`

Prose-stripped contract seal (the value in tapes `api/CONTRACT` at the pinned
tag; this changes only when the contract *shape* changes, so it is the identity
a doc-comment edit does not move):

- api `sha256:c966a65a6c3ab126a908e9a7db55905323686e50077441f52f5675752c9ff8ea`

## Updating

1. Pick the tapes release tag you are bumping to and download its contract
   asset:

   ```sh
   tag=v0.34.0   # the new tag
   base="https://github.com/papercomputeco/tapes/releases/download/${tag}"
   curl -fsSL -o tapes-api.yaml "${base}/tapes-api-${tag}.yaml"
   ```

2. Copy the YAML here verbatim — never hand-edit it. To confirm the copy
   matches the release before touching this file, run
   `TAPES_CONTRACT_TAG=<tag> make contracts-check` now: while the override
   names a tag other than the recorded pin, the fingerprint gate reports its
   expected mismatch informationally and only the release-asset byte diff
   decides.
3. Update the pin above (tag, commit, asset URL) and every fingerprint: the
   file-byte sha256 (`shasum -a 256`), the prose-included fingerprint (printed
   by `tapes dev openapi`, or served as the ETag), and the seal (tapes
   `api/CONTRACT` at the tag).
4. Run `make contracts-check` (strict again now that the pin matches) and
   `cargo test`. Then re-pin each consumer: their coverage gates will list any
   operation the new contract added so it can be mapped or deliberately
   allow-listed. A bump that adds an operation is expected to fail every
   consumer's build until each has decided about it — that is the gate working.

For offline work, or when developing against an unreleased tapes commit,
`scripts/contracts-check.sh` can instead re-emit the contract from a local tapes
checkout (`TAPES_REPO=/path/to/tapes`) — but a vendoring bump that lands here
must always pin a published tag and its asset.
