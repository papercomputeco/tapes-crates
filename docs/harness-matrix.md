---
title: The harness regression matrix
description: The CI tier that launches each real harness binary through a mock provider and a mock ingest, and asserts on what actually crossed the wire.
sidebar:
  order: 6
---

Five harnesses, two capture clients, and — until this — nothing that launched a
real harness binary through a real client automatically.

That gap has a particular shape. Every layer had tests and every layer was
green: launch recipes are unit-tested as pure functions, the `X-Tapes-*` envelope
is checked against a shared fixture corpus, attribution lanes are exercised with
synthetic session files. None of that starts a harness. The breakages that
reached users lived in the *composition* — a harness release moves a wire detail,
a recipe points at a path the proxy does not serve, a configuration knob isolates
the harness from the very lane that reads its session files — and a composition
nothing exercises is a composition that breaks quietly.

Tier 1 is the floor under that: **CI, every change, no cluster.** For each
registry harness, start a mock provider and a mock ingest, launch the actual
binary, and assert on what crossed the wire.

## The two columns

| column | what it needs | what it proves |
| --- | --- | --- |
| **harness → mock** | the harness binary | The launch recipe produces a configuration the harness accepts, and the resulting turn lands on the provider surface the recipe declared. |
| **harness → client → mock** | the harness binary *and* a capture client binary | Launched implies attributed; the turn is filed under the harness that was actually launched; the per-launch capture nonce did not travel upstream. |

The second column is where the assertions that matter live, because it is the
only configuration in which any of them are meaningful — with no client in the
path there is nothing doing attribution and no nonce to strip.

The clients live in other repositories, so their binaries are supplied by path:

```bash
make harness-matrix                                              # column one only
make harness-matrix TAPESCTL_BIN=../tapesctl/target/debug/tapesctl   # both columns
```

`TAPESCTL_BIN` and `PAPER_BIN` are read. Unset means the composition cells skip
*with that stated* — they do not vanish.

## Running it

```bash
nix develop .#matrix          # a shell with the packaged harnesses on PATH
make harness-matrix
```

`--nocapture` is baked into the make target and is not decoration. The run's
value is the table of which cells ran and why the rest did not; a captured run
prints `ok` and shows none of it.

Every run writes a version manifest next to the test binary
(`target/debug/deps/harness-matrix-manifest.json`) and prints its path.

## The honest table

What actually runs depends on what is installed, and the answer differs by
place. This is the current state, not an aspiration:

| harness | harness → mock | harness → client → mock | note |
| --- | --- | --- | --- |
| `claude` | runs wherever `claude` is installed | runs with a client binary | packaged in the pinned nixpkgs, so it runs in CI |
| `codex` | runs wherever `codex` is installed | runs with a client binary | packaged in the pinned nixpkgs, so it runs in CI |
| `opencode` | runs wherever `opencode` is installed | **skips** — `tapesctl` does not list `opencode` among its supported harnesses | packaged in the pinned nixpkgs; the client-side gap is real and visible |
| `pi` | runs wherever `pi` is installed | runs with a client binary, after the capture plugin is installed into the sandbox | not in the flake's `matrix` shell, so both cells skip in CI — though the pinned nixpkgs does carry `pi-coding-agent` (see the follow-ups) |
| `codex-app` | **skips**, always | **skips**, always | a long-lived host a consumer configures rather than starts: no one-shot invocation exists |

In CI the composition column skips entirely: both clients live in other
repositories, and wiring a cross-repository build is follow-up rather than
something to fake with a stale published binary.

## What a run found

Two findings came straight out of standing this up, which is roughly the point:

**Claude attribution does not survive `CLAUDE_CONFIG_DIR`.** Claude Code writes
its state under that directory when it is set; the lane that reads Claude's
session files resolves `$HOME/.claude/sessions` unconditionally. Setting the
variable therefore isolates the harness *and silently un-attributes every turn it
produces* — captured, and filed under `unknown`. Codex is the other way round:
its lane honours `$CODEX_HOME` explicitly. The matrix isolates with `HOME`
instead, because that is the one relocation both halves follow, and a recipe test
now forbids `CLAUDE_CONFIG_DIR` outright so the trap cannot be re-entered. The
underlying asymmetry affects any user who sets that variable, and nothing tells
them.

**The registry and the client disagree about `opencode`.** The registry declares
it launchable through a recipe; `tapesctl` refuses it as unsupported. That is
exactly the class of divergence this matrix exists to surface, and it now surfaces
as a named skip quoting the client's own message rather than as nothing at all.

## The recipe format

One `OneShotRecipe` per registry harness, in
[`crates/tapes-mock-upstream/src/recipe.rs`](https://github.com/papercomputeco/tapes-crates/blob/main/crates/tapes-mock-upstream/src/recipe.rs).
Declarative, with a small tagged field for the one thing that genuinely differs:

| field | meaning |
| --- | --- |
| `harness_id` | the registry harness this launches |
| `binary` / `version_args` | the program, and how to ask it its version |
| `argv` | the argv for one non-interactive turn; exactly one `{prompt}` slot |
| `surface` | which provider surface the turn is expected to land on |
| `pointing` | how the harness is pointed at the upstream — through the real launch recipe, or through the capture-gateway environment contract |
| `sandbox_env` | extra relocations beyond the `HOME` every plan applies |
| `extra_env` | fixed environment, chiefly credentials a harness insists on |
| `unsupported` | why this harness cannot run a cell at all, when it cannot |

`pointing` is a tagged enum rather than a uniform "set this variable" for the
same reason the registry holds no recipe instances: Claude takes one environment
variable, codex takes a provider declared through repeated `-c` flags, opencode
takes a config document, and the self-attributing harnesses take an environment
contract and an extension. Flattening those would mean a union of every harness's
configuration.

Two invariants are enforced by test rather than by review: every registry harness
has a recipe (so a harness added to the registry cannot quietly acquire no matrix
row), and no recipe names a harness the registry does not have.

## The version manifest

A green cell says the composition worked *with the harness version that happened
to be installed*. Harnesses ship constantly, so a green run whose versions are
unrecorded cannot distinguish "we still work against the current release" from
"we still work against the release that was current six weeks ago".

```json
{
  "schema": 1,
  "harnesses": [
    { "name": "claude", "status": "ran", "version": "2.1.219 (Claude Code)", "path": "/nix/store/…/bin/claude" },
    { "name": "codex-app", "status": "skipped", "reason": "the Codex desktop app is a long-lived host …" }
  ],
  "clis": [
    { "name": "tapesctl", "status": "skipped", "reason": "TAPESCTL_BIN is unset; the tapesctl CLI lives in another repository …" }
  ]
}
```

Skips are in the document for the same reason they are in the printed table: a
manifest listing only successes would let a harness fall out of the matrix and
still produce a plausible-looking record. `VersionManifest::drift_from` reports
three kinds of change — a version that moved, a participant that started running,
and one that *stopped* — and the third is the one a diff of successes alone would
miss entirely.

This is the drift watch's input and deliberately only its input: the manifest
records and compares, and takes no view on what should happen when a version
moves. That policy belongs with whatever runs on a schedule.

## The version record

A manifest says what one run ran against. It cannot say whether those were the
*current* releases, because a single run has nothing to compare itself to — and
comparing to the previous run does not help either, since two runs on the same
runner agree perfectly while both sit six weeks behind upstream.

So the answer is written down, in [`harness-versions.json`](https://github.com/papercomputeco/tapes-crates/blob/main/harness-versions.json)
at the repository root: per harness, the version the matrix last **passed**
against, and how to find out what upstream is serving today.

```json
{
  "name": "claude",
  "version": "2.1.220 (Claude Code)",
  "upstream_version": "2.1.220",
  "discovery": {
    "kind": "npm",
    "source": "@anthropic-ai/claude-code",
    "note": "…why this source, and anything needed to trust the answer"
  }
}
```

JSON rather than TOML, for three reasons that all point the same way. The
manifest it is compared against is already JSON, so one serde type reads both
and there is no second parser to keep in step. The scheduled watcher has to
*rewrite* the file on a runner, and `jq` is present everywhere while a TOML
editor is not. And the thing TOML would buy — comments explaining each entry —
is bought better by the mandatory `note` field, because the workflow can print a
note and cannot print a comment.

**Two versions per harness, deliberately.** A harness prints its version however
it likes (`2.1.220 (Claude Code)`, `codex-cli 0.145.0`, a bare `1.18.4`) while a
registry answers with a bare semver. `version` is compared to a run's manifest by
exact equality; `upstream_version` is compared to discovery. Nothing parses
anything, so a harness that rewords its `--version` output surfaces as ordinary
drift a human reads rather than as a watch that quietly stopped matching.

Every matrix run now prints where it stands against the record, whether or not
anything moved:

```text
--- drift from the version record ---
  DRIFT  claude: the record passed against 2.1.220 (Claude Code); this run ran 2.1.229 (Claude Code)
  (drift is stated, not failed: the record moves through a reviewed pull request)
```

Drift never fails a run — the record is what the matrix last passed against, and
a newer harness on a laptop is news, not a regression. A record that cannot be
*read* does fail, for the same reason a manifest that cannot be written does:
every later run would report "no drift" on the strength of a comparison that
never happened. A harness the record knows but this run did not have is reported
in its own bucket, with the run's own skip reason, and does not count as drift.

## The drift watch

[`.github/workflows/harness-drift-watch.yml`](https://github.com/papercomputeco/tapes-crates/blob/main/.github/workflows/harness-drift-watch.yml),
daily at 06:37 UTC:

1. **Discover.** `scripts/harness-latest-versions.sh` asks each watched entry's
   source for its current version. Unreachable registry, renamed package,
   unimplemented discovery kind — all exit non-zero. The one answer it will never
   give is a quiet "nothing is newer".
2. **Verify.** For each moved harness, install that exact version, put it on PATH
   ahead of the one the flake pins, and run the whole matrix. The run's manifest
   must resolve the harness *from the installed candidate* — checked, not
   assumed, because a PATH that failed to take would otherwise produce a green
   run against the old version and a pull request claiming the new one passed.
3. **Propose.** Green: push `automation/harness-drift/<harness>` and open a pull
   request carrying old → new and a link to the run. Red, or a version that could
   not be installed at all: open an issue naming the harness, the version, and
   the failing cells — and no pull request, so the record keeps naming the
   version that last passed.

Automation moves the record, CI decides, humans merge. Nothing is auto-merged and
nothing is written to the default branch.

**Dedupe** is structural rather than best-effort. One stable branch per harness,
force-updated, so there is exactly one open bump per harness and no older one to
merge later and walk the record backwards. Issues are deduped on an exact title
per harness and version, so a nightly schedule cannot file the same breakage
every morning.

**One repository setting is required**, and until it is enabled the watch pushes
branches but cannot open pull requests:

> Settings → Actions → General → Workflow permissions →
> ☑ **Allow GitHub Actions to create and approve pull requests**

The workflow catches exactly that failure and reports it by name, having already
pushed the branch, so the run is recoverable by hand. No secret beyond the
default `GITHUB_TOKEN` is needed — everything it writes, it writes here.

The job that installs and executes harness releases holds no credential at all,
and the job that pushes runs nothing but `jq` and `git` in its own fresh
checkout. A record bump must be able to say that the tree it came from never
executed a harness release.

### What the watch does not cover

- **Only version-discoverable harnesses.** A harness is watched because the
  record names a source that can be asked. Everything else is in the record as
  `unwatched`, with its reason, and appears in the run's log rather than being
  omitted from it.
- **`codex-app` is manual, permanently.** The desktop app self-updates on its own
  schedule, out of band from anything this repository could pin, and it has no
  one-shot invocation for a cell to drive.
- **`pi`'s provenance is its registry's.** It is not in the flake's `matrix`
  shell, so the per-change matrix skips it and its baseline was seeded from a
  developer machine. The watch installs it from npm, which is the only reason its
  cells run there at all.
- **A candidate run runs the whole matrix, not one cell.** The runner has no
  per-harness filter, so a failure attributed to a candidate could belong to
  another row; the issue body carries the actual failing cell names for exactly
  that reason.

Both scripts run by hand, which is how the watch is debugged without waiting for
a schedule:

```bash
scripts/harness-latest-versions.sh                                  # what has moved
scripts/harness-record-bump.sh claude '2.1.229 (Claude Code)' 2.1.229
```

To see the drift path work while the record is up to date, point a run at a
doctored copy:

```bash
jq '(.harnesses[] | select(.name == "claude") | .version) = "2.1.100 (Claude Code)"' \
  harness-versions.json >/tmp/doctored.json
HARNESS_VERSIONS_RECORD=/tmp/doctored.json make harness-matrix
```

## Follow-ups, named

Left undone deliberately, rather than sketched:

- **A pinned CI image holding every harness at a known version.** Today the
  versions are whatever the nixpkgs pin carries, which makes a run current but
  not reproducible. The manifest records exactly what they were, and the version
  record says which of those the matrix has actually passed against, which is the
  honest intermediate.
- **The composition column in CI**, which needs a cross-repository client build.
- **Forged-envelope refusal as a live cell.** The refusal *semantics* are pinned
  here — the envelope reader is checked against the corpus's malformed and
  partial cases, and a harness id without a session id never counts as
  attributed. Driving a forgery through a running client's proxy additionally
  needs the client to advertise its proxy address, which none does yet.
- **Exercising each client's own `plugin install`.** The matrix installs a
  capture plugin through this repository's `PluginArtifact::install` so the cell
  stays portable across clients whose installer command lines differ.
- **`pi` in the per-change matrix.** The flake's `matrix` shell does not carry it,
  which is why its cells skip in CI — but the pinned nixpkgs *does* package it as
  `pi-coding-agent`, so this is now a one-line addition to that shell rather than
  the packaging problem it was when the shell was written. Doing it would move
  pi's recorded baseline from a developer machine to the pin, which is a better
  provenance than the drift watch's registry install and worth taking together.
  The watch covers pi meanwhile, and a nightly watch is not the same guarantee as
  a cell on every change.
