# The harness regression matrix

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
| `pi` | runs wherever `pi` is installed | runs with a client binary, after the capture plugin is installed into the sandbox | **not** packaged in nixpkgs, so both cells skip in CI |
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
[`crates/tapes-mock-upstream/src/recipe.rs`](../crates/tapes-mock-upstream/src/recipe.rs).
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

## Follow-ups, named

Left undone deliberately, rather than sketched:

- **A pinned CI image holding every harness at a known version.** Today the
  versions are whatever the nixpkgs pin carries, which makes a run current but
  not reproducible. The manifest records exactly what they were, which is the
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
- **`pi` in CI**, which needs it packaged or vendored.
