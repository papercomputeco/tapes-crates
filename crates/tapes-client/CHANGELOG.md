# Changelog for `tapes-client`

Kept by hand in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) style,
grouped under Added / Changed / Deprecated / Removed / Fixed / Security. There
is no tooling behind this file: entries are written for somebody deciding
whether to upgrade, so each one says what changed for a caller rather than
which commit changed it. This crate versions independently of its siblings, so
a version here is a statement about this crate alone — releasing is described
in [`docs/releasing.md`](../../docs/releasing.md). Move the `Unreleased`
entries under a version heading with its date as part of the release change,
before the tag is cut.

Pre-1.0, `0.x` versions carry the usual Cargo meaning: a breaking change bumps
the minor (`0.2.0`), and anything compatible bumps the patch (`0.1.1`).

## [Unreleased]

Not yet published. The first release will be `0.1.0`, and it is the current
contents of the crate rather than a changelog entry — the seams are listed in
[`README.md`](README.md).

Two things here move for reasons outside a normal code change, and both belong
in this file when they do: a refresh of the vendored read contract under
`contracts/`, which can add or change operations without a line of Rust being
touched, and a change to which features are on by default, since `cli` and
`direct-http` decide whether the crate pulls in clap and reqwest at all.
