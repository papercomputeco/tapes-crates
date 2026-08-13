---
title: Releasing
description: How the three crates reach crates.io — dependency order, the tag scheme, what the release workflow checks, and the two-lock hold on the upload.
sidebar:
  order: 7
---

The three crates in this repository are published to crates.io independently.
Each has its own version, its own CHANGELOG, and its own release tag, because
each makes its own promise to the people who depend on it — a fix to the read
client is not a reason to renumber the capture protocol.

All three are live: `tapes-capture`, `tapes-harnesses`, and `tapes-client`
each published `0.1.0` on 2026-08-13, so `cargo add tapes-client` resolves
against the real index and every release after the first follows the
procedure below. The upload step is guarded by two locks — see
[The hold](#the-hold).

## Dependency order

Only one edge exists between these crates:

```text
tapes-capture  ──depended on by──>  tapes-harnesses
tapes-client                        (depends on neither, and neither depends on it)
```

`tapes-harnesses` declares `tapes-capture` with both a `path` and a `version`.
Cargo uses the path in this workspace and the version everywhere else, which is
what makes the crate publishable at all — but it also means **the required
`tapes-capture` version must already be on crates.io before `tapes-harnesses`
can publish**. Releasing them in the wrong order does not corrupt anything; the
`tapes-harnesses` publish simply fails to resolve its dependency.

So:

1. `tapes-capture` first, whenever its version has changed.
2. `tapes-harnesses` after, and only after that release is live.
3. `tapes-client` whenever — it is not on either side of the edge.

Bumping `tapes-capture` means bumping the `version` in the `tapes-capture`
dependency entry in `crates/tapes-harnesses/Cargo.toml` too, in the same change
that bumps the crate. A stale requirement there is not a build failure in this
workspace — the path dependency satisfies it locally — so nothing catches it
until a consumer resolves against crates.io.

## Cutting a release

1. Bump `version` in that crate's `crates/<crate>/Cargo.toml`.
2. Move its `CHANGELOG.md` entries from `Unreleased` into a version heading
   with the date.
3. If the crate is `tapes-capture` and the version changed, update the
   `tapes-capture` dependency version in `crates/tapes-harnesses/Cargo.toml`.
4. Land that as a normal PR. `cargo package --workspace --locked` runs on it
   like it runs on every PR.
5. Dispatch the **Cut Release** workflow (`.github/workflows/cut-release.yaml`)
   from `main`, choosing the crate. It reads the version the manifest already
   carries at main's tip, refuses a tag that already exists, and pushes
   `<crate>-v<version>` pointing at that tip — so the tag matches the manifest
   by construction, which is the mistake hand-typed tags invite. Tagging by
   hand remains the fallback:

```bash
git tag tapes-client-v0.1.0
git push origin tapes-client-v0.1.0
```

The tag scheme is `<crate>-v<version>`, one crate per tag. The release workflow
resolves the tag back to a crate, refuses a tag that does not name one of the
three, and **fails if the tagged version does not match that crate's manifest**
— a tag is a claim about the source, and an unchecked claim is how a version
number that exists nowhere in the tree ends up on crates.io.

## What the release workflow does

`.github/workflows/release.yaml`, on a `<crate>-v*` tag:

1. Resolves the tag to a crate and version.
2. Checks the version against the manifest.
3. Re-runs the gates against the tagged tree — fmt, clippy (default and all
   features), tests, the contract seal, and `cargo package --workspace`. A tag
   can point at a commit that never sat on `main`, so a green branch says
   nothing about these bytes.
4. Publishes that one crate — unless the upload is held.

## The hold

Two independent locks guard the upload, and the publish step runs only when
both are open:

| lock | opens when | applies to |
| --- | --- | --- |
| repository variable `PUBLISH_ENABLED` | it is set to exactly `true` | every run |
| workflow input `confirm` | it is retyped to match the tag | manual `workflow_dispatch` runs only |

While `PUBLISH_ENABLED` is unset, every step before the upload still runs, so
pushing a release tag is a full rehearsal: it tells you whether that crate
would have published, and changes nothing. The job summary names which lock is
closed. The hold was opened for the `0.1.0` releases; its current state is
visible in one place, the repository settings.

To lift the hold, create the repository variable `PUBLISH_ENABLED = true`
(Settings → Secrets and variables → Actions → Variables). Deleting it re-arms
the hold. It is a variable rather than a code change on purpose — the state of
the hold is then visible in one place in the repository settings, and flipping
it leaves an audit entry that editing a workflow file does not.

Publishing also needs the `CARGO_REGISTRY_TOKEN` secret, a crates.io API token
scoped to publishing these crates. Scoping that secret to a GitHub
Environment with required reviewers is worth doing: it adds a human approval
to each upload, which neither lock above provides.

## Keeping the crates publishable

`.github/workflows/release-checks.yaml` runs on every PR and every push to
`main`, so publishability cannot rot between releases:

- `cargo package --workspace --locked` packages all three crates and verifies
  each one compiles **standalone**, outside the workspace. `--workspace`
  matters: it builds a temporary registry from the packaged crates, which is
  the only way `tapes-harnesses` can be verified against a `tapes-capture` that
  is not yet on crates.io. A per-crate `cargo package -p tapes-harnesses`
  fails today for that reason, and that failure is about the invocation rather
  than about the crate.
- `cargo publish --dry-run` for `tapes-capture` and `tapes-client`, which
  exercises the upload-preparation path — manifest normalisation, the
  path-to-registry dependency rewrite, the index lookups — without uploading.
  `tapes-harnesses` joins this list once `tapes-capture` has a published
  release.
- An explicit check that the vendored data files are inside the packages.
  These files are `include_str!`d, so a missing one is a compile error rather
  than a silent hole — but the check names the file, which a compile error in a
  packaged tarball does not.

One thing worth knowing when reading a failure there: a packaged crate does not
carry the root manifest's `[patch.crates-io]`. The verification builds are the
only place in this repository that compiles `netsock -> libproc` against the
real crates.io release rather than the patched revision, so a problem the patch
exists to work around surfaces there first.
