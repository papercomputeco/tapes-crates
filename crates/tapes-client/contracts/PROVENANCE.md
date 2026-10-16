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

- Release tag: **v0.51.0** (papercomputeco/tapes at `fb2032f`, "feat(api): list
  subjects and make the trusted subject header opt-in" — the squash of
  tapes#397, which with tapes#394 and #395 adds the transcript and subjects
  reads).
- Asset: `https://github.com/papercomputeco/tapes/releases/download/v0.51.0/tapes-api-v0.51.0.yaml`,
  copied byte-for-byte.

## Fingerprints

Vendored file bytes (what `scripts/contracts-check.sh` verifies):

- `tapes-api.yaml` sha256 `d549d95d1bfeaf35d6e0112fe2a9e5fb8b84cd1d9d046eace1b7ec9b9d975c25`

Prose-included document fingerprint (`CompiledDoc.Fingerprint()` as printed by
`tapes dev openapi`; the ETag a server would serve for the same document):

- api `sha256:1d61f217844443c54e6eca99ee0684e657ef89a439bee2b7c0b94130925e8974`

Prose-stripped contract seal (the value in tapes `api/CONTRACT` at the pinned
commit; this changes only when the contract *shape* changes, so it is the
identity a doc-comment edit does not move):

- api `sha256:ce9c831bddb07cf6c282efdb877dae95443fea1c471202b69018fba546bf889e`

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
   `cargo test`. If the new contract publishes anything `pending.yaml` (the
   hand-written, unsealed fragment beside this file) still carries,
   `no_pending_entry_is_already_published` names it: delete those entries in
   this change, and fix any model the published shape disagrees with. Then re-pin each consumer: their coverage gates will list any
   operation the new contract added so it can be mapped or deliberately
   allow-listed. A bump that adds an operation is expected to fail every
   consumer's build until each has decided about it — that is the gate working.

For offline work, or when developing against an unreleased tapes commit,
`scripts/contracts-check.sh` can instead re-emit the contract from a local tapes
checkout (`TAPES_REPO=/path/to/tapes`) — but a vendoring bump that lands here
must always pin a published tag and its asset.
