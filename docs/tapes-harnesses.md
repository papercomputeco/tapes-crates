---
title: tapes-harnesses
description: The harness-knowledge crate — the registry and its five axes, the three capture mechanisms, launch recipes, plugin artifacts, attribution lanes, and transcript discovery.
sidebar:
  order: 3
---

`tapes-harnesses` is the half of capture that changes *because* a harness was
added. Its membership test is one question: **would adding one more harness
change this?** If yes it belongs here; if no it belongs in
[tapes-capture](./tapes-capture.md), which this crate depends on and which may
never depend back.

One naming note before anything else: `tapes-harnesses` is a crate in the
`tapes-crates` repository, not a repository of its own — see
[the names](./introduction.md#the-names).

```bash
cargo add tapes-harnesses
```

Full API documentation is on
[docs.rs](https://docs.rs/tapes-harnesses/latest/tapes_harnesses/). The crate
has no feature flags: everything below is always compiled.

## The registry

`harness::REGISTRY` holds one declaration per harness, bundling its id,
aliases, User-Agent rule, launch support, attribution strategy, transcript
location, and plugin needs. Everything else derives: `find` resolves a
user-typed name through the aliases, `for_user_agent` resolves a request's
`User-Agent`, and `supported_agents` is the launchable subset. Consumers
derive their supported-agent lists from the registry rather than hard-coding
one, so a new entry appears in their surface without their doing anything —
adding a harness is additive.

This compiles and runs against the published crate:

```rust
use tapes_harnesses::harness;

fn main() {
    // Resolve a user-typed spelling through ids and aliases, case-insensitively.
    if let Some(h) = harness::find("Claude-Code") {
        println!("{}: launch={:?} attribution={:?}", h.id(), h.launch(), h.attribution());
    }

    // The launchable subset, derived from the registry rather than restated.
    println!("launchable: {}", harness::supported_agents().join(", "));
}
```

Output:

```text
claude: launch=Recipe attribution=SessionsDir
launchable: claude, codex, opencode, pi
```

### The five axes

A `Harness` declaration is a classification along five orthogonal enums, and
they are the vocabulary the rest of the crate speaks:

- **`UserAgentMatch`** — how a request's `User-Agent` identifies the harness:
  `None`, or a case-insensitive `Prefix`.
- **`LaunchSupport`** — whether this crate can plan a launch: `Recipe` (the
  crate ships one), `ConsumerOwned` (launchable, but the consumer plans the
  argv itself), or `Unsupported`.
- **`AttributionStrategy`** — how a capture client recovers a session's
  identity: `SessionsDir`, `OpenRollout`, `SelfAttributing`,
  `LifecycleHooks`, or `None`.
- **`TranscriptSource`** — where the harness's on-disk transcripts live:
  `ClaudeProjects`, `CodexRollouts`, or `None`.
- **`PluginDelivery`** — whether capture needs an artifact installed into the
  harness: `None`, `BundledExtension` (the crate ships the files), or
  `HookManifestTemplates` (the crate ships manifest templates with
  consumer-rendered slots).

All five are `#[non_exhaustive]` on purpose: a new harness can add a variant
without a breaking change.

### The registry today

| harness | User-Agent | launch | attribution | transcripts | plugin |
| --- | --- | --- | --- | --- | --- |
| `claude` | prefix `claude` | `Recipe` | `SessionsDir` | `ClaudeProjects` | `None` |
| `codex` | none | `Recipe` | `OpenRollout` | `CodexRollouts` | `None` |
| `codex-app` | none | `Unsupported` | `LifecycleHooks` | `CodexRollouts` | `HookManifestTemplates` |
| `opencode` | none | `Recipe` | `SelfAttributing` | `None` | `BundledExtension` |
| `pi` | none | `ConsumerOwned` | `SelfAttributing` | `None` | `BundledExtension` |

Only Claude is identified by User-Agent; the others are recognised by route,
launch marker, lifecycle report, or their own envelope. Note that `codex-app`
is a distinct harness, not an alias of `codex`: it shares Codex's wire
protocol and rollout tree, but it is a long-lived host a consumer configures
rather than launches, and its identity arrives through lifecycle hook reports.

## The three ways a harness gets captured

This is the distinction to hold on to, because it decides which modules apply
to a given harness. It correlates with `AttributionStrategy` but is not the
same axis: that enum says how a request acquires an identity, this says how
the traffic is reached at all.

| mechanism | when it applies | plan it with |
| --- | --- | --- |
| **Launch redirect** — point the harness's base-URL knob at a proxy | the harness has such a knob (`claude`, `codex`, `opencode`) | `launch` |
| **Installed plugin** — code runs *inside* the harness and stamps its own envelope | the harness has no such knob (`pi`) | `plugin` |
| **Lifecycle hooks** — a hook plugin reports allowlisted evidence at session boundaries | the harness is configured rather than launched (`codex-app`) | `plugin::codex_app` and `config` |

opencode deliberately has both of the first two: its provider endpoints live
in a config document a recipe can plan, but it publishes no session file a
client could attribute from, so the bundled extension both redirects and
stamps the envelope from inside. The registry declares the combination
because the two compose.

## Launch recipes

Recipes are **pure planners**: they plan an argv prefix, an environment
overlay, and any config documents a harness reads from disk. Spawning,
materialisation, and cleanup stay with the consumer. The registry records
*that* a harness has a recipe; the consumer constructs it with the arguments
only the consumer has — Claude needs one endpoint, codex needs an endpoint
plus an auth mode and a provider identity, opencode needs an endpoint per
provider plus a model.

## Plugins, and the gateway environment a launcher sets

For harnesses captured from inside, the crate ships the artifacts themselves —
`plugin install` in a consumer is a file copy over crate-owned bytes, so no
consumer carries its own drifting fork of the asset. Bundled artifacts must be
vendor-neutral: an asset that names a vendor stays in that consumer's
repository and gets no registry variant.

An installed artifact reads its instructions from the environment at launch
time. Seven variables make up that contract; the first four are protocol and
live in `tapes-capture`, the last three are presentation and live here:

| variable | set by | meaning |
| --- | --- | --- |
| `TAPES_GATEWAY_URL` | launcher | Where to send the harness's LLM traffic. **Unset means "not captured"** — the artifact leaves the harness's own endpoints alone. |
| `TAPES_GATEWAY_SCHEMA` | launcher | Which upstream schema the proxy fronts (`anthropic`, `openai`). A display and diagnostic hint; an artifact must not gate the redirect on it. |
| `TAPES_GATEWAY_NONCE` | launcher | The per-launch secret. Read once at load and deleted from the process environment immediately, so the harness's own subprocesses never receive it. |
| `TAPES_GATEWAY_PROVIDER_ROUTES` | launcher | Set to `1` when the proxy serves each provider on its own route. Unset is the single-upstream shape, which is what a launcher predating this variable gets. |
| `TAPES_GATEWAY_LABEL` | launcher | The product word shown in a harness's status entry. |
| `TAPES_GATEWAY_LABEL_SUFFIX` | launcher | Appended to the status label after the active schema. |
| `TAPES_GATEWAY_REMEDY` | launcher | The sentence appended to a schema-mismatch warning — the diagnosis is the asset's, the remedy is the launcher's, because only the launcher knows which command switches its proxy. |

## Attribution

`attribution::attribute` (and `attribute_with_evidence`) is the pipeline: one
request's facts in, one attribution outcome out, driving the per-harness
primitives in the order that was validated against real traffic. **This is
the call a capture client makes.** The primitives beneath — session-file
reads, fork-parent recovery, the rollout watcher — are exposed for tests and
unusual clients, grouped per harness (`attribution/claude/`,
`attribution/codex/`, `attribution/codex_app/`) with the harness-agnostic
pieces shared.

The per-harness lanes match the `AttributionStrategy` axis:

- **`SessionsDir`** (claude): the harness publishes a PID-indexed session
  file, so the peer PID of the accepted connection indexes straight to an
  identity.
- **`OpenRollout`** (codex): nothing is published by PID; identity is
  recovered from the rollout file a live process holds open, filtered by
  recency and by the provider the launch configured.
- **`SelfAttributing`** (pi, opencode): the harness stamps its own complete
  `X-Tapes-*` envelope from inside. The client's job is to *preserve* what
  arrives — overwriting a complete inbound envelope with `unknown` would
  silently re-file those sessions — and to demand the nonce echo before
  believing it.
- **`LifecycleHooks`** (codex-app): identity arrives as hook reports at
  session, prompt, stop, and subagent boundaries. There is no launched PID to
  anchor peer trust on, so the evidence is allowlisted instead.

## Transcripts

Wire capture yields a complete call inventory but no causal/fork skeleton —
that lives only in the harness's on-disk transcript trees, which the
transcript lane uploads. `transcript::sweep` discovers sessions under a
transcript root (including a startup sweep that recovers sessions which began
and ended while a client was down), `trigger` is the push policy, and
`payload` is the ingest payload shape. Delivery, auth, and retry are the
consumer's.

## Config patch grammars

`config` owns format-preserving patch grammars for harness config files — how
an installer patches a capture provider into a harness's *own* config,
idempotently and without disturbing the user's content. Where `launch` plans
per-process config that dies with the process, `config` owns the durable
install a desktop app or long-lived integration needs.

## Adding a harness

Teaching this crate about a new coding agent starts with one `const` in
`harness.rs`; invariant tests tell you what else the declaration implies.
[Adding a harness](./adding-a-harness.md) walks the whole path, and
[the harness regression matrix](./harness-matrix.md) is the CI floor every
entry has to stand on.
