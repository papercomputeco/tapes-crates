# Changelog for `tapes-harnesses`

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

## [0.1.0] - 2026-08-13

The first release. `0.1.0` is the contents of the crate at publish rather than
a list of changes — the seams are listed in [`README.md`](README.md).

Adding a harness to the registry is an addition rather than a break, but it is
the change consumers most want to read about, because they derive their
supported-agent lists from that registry. Note the required `tapes-capture`
version here too when it moves: this crate cannot resolve on crates.io until
that release is live, so the requirement is part of what an upgrade costs.
