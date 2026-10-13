# Changelog for `tapes-capture`

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

Cross-harness lineage: an envelope can now say which harness its parent
session belongs to, and launchers share one environment contract for telling
a nested launch which launch it runs inside.

### Changed

- **Breaking**: `TapesAttribution` gains a public `parent_harness_id` field, so
  a struct literal that names every field no longer compiles. Add
  `parent_harness_id: None` — the value every existing producer emits — or
  build through a constructor and `with_parent_harness_id`.

### Added

- `envelope::X_TAPES_PARENT_HARNESS_ID` (`x-tapes-parent-harness-id`): the
  harness the parent session belongs to. Emitted only beside
  `x-tapes-parent-harness-session-id`, after it in the budget order; absent
  means "same harness as the request", so every envelope produced before it
  keeps its meaning and no existing constructor emits it.
  `TapesAttribution::with_parent_harness_id` sets it and `from_headers` reads
  it back. The shared fixture corpus does not carry a case for it yet; the
  fixture reader accepts a `parent_harness_id` field for when it does.
- `envelope::HARNESS_ID_HERMES` (`hermes`).
- `launch`: the launch-lineage contract. `LAUNCH_ID_ENV`
  (`TAPES_LAUNCH_ID`) and `LAUNCH_HARNESS_ID_ENV` (`TAPES_LAUNCH_HARNESS_ID`)
  are exported by every launcher to its child; `LaunchContext::from_env` reads
  the parent launch out of the launcher's own environment and
  `LaunchContext::child_env` writes this launch's values for the child.
  `ParentLaunch`, `LaunchContext::from_lookup`, and `new_launch_id` round it
  out. Mapping a parent launch to a parent session stays with the launcher.

## [0.1.0] - 2026-08-13

The first release. `0.1.0` is the contents of the crate at publish rather than
a list of changes — the seams are listed in [`README.md`](README.md).

Changes to the `X-Tapes-*` envelope belong here in particular: it is a
cross-language contract that Go parsers on the other side read, so an envelope
change is a change to something outside this crate's own compatibility story.
