# Adding a harness

This crate is client-side harness knowledge: how to launch a coding agent
under a capture proxy, how to work out which session a request belongs to,
where the agent writes its transcripts, and what goes in the `X-Tapes-*`
envelope. Teaching it about a new agent starts in one file — `src/harness.rs` —
and grows from there only as far as that agent's shape demands.

Start by reading `src/harness.rs`. The types there are the vocabulary this
document uses.

The five steps below are the walkthrough. The sections after them are the
reference half: what each field actually causes in a consumer, what a partial
entry costs you, what a new attribution lane has to prove before it can be
trusted, and what breaks when the registry changes.

## Step 1 — declare it

Add a `const` to `src/harness.rs` and put it in `REGISTRY`:

```rust
/// Gemini CLI.
pub const GEMINI: Harness = Harness {
    id: HARNESS_ID_GEMINI,
    aliases: &["gemini-cli"],
    user_agent: UserAgentMatch::Prefix("gemini"),
    launch: LaunchSupport::Recipe,
    attribution: AttributionStrategy::None,
    transcripts: TranscriptSource::None,
    plugin: PluginDelivery::None,
};

pub const REGISTRY: &[Harness] = &[CLAUDE, CODEX, GEMINI, OPENCODE, PI];
```

That declaration is load-bearing, not documentation. `supported_agents()` picks
it up, so it appears in every consumer that derives its launchable list from
here rather than restating one — see "Partial entries and full ones" for which
consumers do that today and which still hardcode theirs; `find()` resolves its
name and aliases from a CLI argument;
`for_user_agent()` routes its traffic; and any launch recipe you add takes its
harness id from `GEMINI.id()` rather than spelling the string again.

`id` must be the `X-Tapes-Harness-Id` value, declared as a constant in
`src/envelope.rs` alongside the others. That constant is the on-wire name, and
the tapes deriver keys on it — see "The other two places" below.

The invariant tests at the bottom of `src/harness.rs` run against the whole
registry, so `cargo test` will tell you if a declaration is inconsistent: a
duplicated name, an id the envelope never emits, a transcript tree that cannot
be located, a User-Agent rule that claims traffic it should not.

A declaration alone is a real, useful state. `OPENCODE` is exactly this: it is
launchable, and it has no attribution lane yet. Its sessions capture and land
under `harness_id: unknown` until someone writes one. Land the declaration,
then add capability.

## Step 2 — a launch recipe, if the agent needs one

`LaunchSupport::Recipe` promises a `LaunchRecipe` in `src/launch/` whose
`harness()` returns your id. Recipes are **pure**: given a `ProxyEndpoint`,
`plan()` returns the argv prefix, the environment overlay, and any config
documents the agent reads from disk. It never spawns a process, writes a file,
creates a temporary directory, or reads the user's home — the consumer owns all
of that, and therefore owns cleanup.

`src/launch/claude.rs` is the smallest complete example (one environment
variable). `src/launch/codex.rs` shows a config-flag grammar with fallible
planning; `src/launch/opencode.rs` shows a recipe that emits a config document.

The line to hold is *harness* knowledge versus *deployment* knowledge. Which
environment variable carries the base URL is yours. What that URL's path prefix
is, which credential to supply, and where to materialise a config file are the
consumer's. A recipe never constructs a route: it receives a fully qualified
endpoint and appends nothing to it.

If the agent exposes no base-URL knob at all, there is nothing for a recipe to
set and capture needs code running *inside* the agent instead. Declare
`LaunchSupport::ConsumerOwned` and see the next step. `PI` is that case.

## Step 2b — a plugin artifact, if capture needs code inside the agent

An agent with no base-URL knob needs a file installed into it — an extension
that registers the agent's providers against the proxy from the inside. Those
files live in `assets/<harness>/` and are declared in `src/plugin.rs` as
`PluginArtifact`s, which the registry hands out through
`PluginDelivery::BundledExtension`. A consumer's `plugin install` is then a file
copy: it resolves the name, takes `harness.plugin_artifacts()`, and writes each
one beneath the user's home.

The bar for putting an asset here is that it names **no vendor**. It reads its
endpoint from `plugin::GATEWAY_URL_ENV` and nothing else — no product-branded
variable, no default endpoint, no "run `<product> ...`" hint in its copy. An
asset that cannot meet that bar stays in the consumer's repository and gets no
variant here; `src/plugin.rs`'s tests enforce the bar for the ones that do.

Two properties an artifact must have, both tested: it is **inert** until
`TAPES_GATEWAY_URL` is set, because it installs globally and would otherwise
change sessions nobody is capturing; and if it stamps an envelope itself, the
header names must match `src/envelope.rs`, since a rename there would otherwise
silently re-file the agent's sessions as `unknown`.

An agent whose plugin is installed by the agent's *own* plugin manager, and
whose hook command or identity strings are irreducibly the consumer's, cannot
ship as fixed artifacts. It ships as **templates** instead —
`PluginDelivery::HookManifestTemplates`, where the crate owns the JSON
structure and event set and the consumer renders its command and identity into
slots (see `src/plugin/codex_app.rs`). The vendor-neutrality bar is the same;
only the branding *slots* are consumer-filled.

## Step 3 — an attribution strategy

Attribution is how a captured session gets a real identity instead of a
synthetic one. Which strategy applies is a property of the agent, not a choice:

- **`SessionsDir`** — the agent publishes a PID-indexed session file it keeps
  current. The peer PID of the accepted loopback connection indexes straight to
  an identity. Claude Code works this way; see `src/attribution/claude/`.
- **`OpenRollout`** — the agent publishes nothing by PID, and identity must be
  recovered from a transcript file a live process holds open, filtered by
  recency and by the provider the launch configured. Codex works this way; see
  `src/attribution/codex/`.
- **`SelfAttributing`** — the agent stamps its own complete `X-Tapes-*`
  envelope from inside itself, through an extension a peer-PID lookup cannot
  see. There is no lane to write: the client's whole job is to *preserve* what
  arrives, which is what `Attributed::stamp` does. `PI` is this shape.
- **`LifecycleHooks`** — the agent is a long-lived host the consumer
  configures rather than launches, so no peer-PID lane can anchor on a
  launched process. Identity arrives instead as allowlisted lifecycle reports
  from a hook plugin installed into the agent; the crate owns the parsed
  shape of those reports, and the consumer owns receiving them. The Codex
  desktop app (`CODEX_APP`) is this shape; see `src/attribution/codex_app/`.
- **`None`** — no client-side attribution yet.

New attribution code goes in `src/attribution/<harness>/`, grouped by harness.
Anything genuinely harness-agnostic stays at the root of `src/attribution/`
next to `peer_pid`, which both existing lanes share.

The composition — the sequence that turns one request's facts into one outcome
— lives in `src/attribution/pipeline.rs` and stays there. It exists precisely so
that it does not exist twice: it was validated against real traffic, and a
second implementation would drift silently, mis-attributing sessions in ways
only a parity corpus would catch.

Two rules the existing lanes were built on, learned the hard way:

- **Refuse rather than guess.** A missed attribution heals when the transcript
  is reconciled. A wrong one is permanent and silently corrupts a session's
  shape. Where the evidence does not identify exactly one session, return
  nothing.
- **Absent means "no evidence", never "matches nothing".** A blank header that
  reaches a matcher will refuse every candidate. Filter it out at the edge.

Everything here is best-effort and time-budgeted. An absent field means
unknown, never a sentinel, and a client that cannot attribute a request still
emits a well-formed envelope.

## Step 4 — transcripts

Wire capture yields a complete call inventory but no causal or fork skeleton;
that lives only in the agent's on-disk transcripts, which the transcript lane
uploads. If your agent writes a tree this crate can locate, add a
`TranscriptSource` variant and teach `resolve()` to find it. Honour whatever
home-directory override the agent itself honours, in one place, the way
`CodexRollouts` delegates for `$CODEX_HOME`.

Discovery and packaging in `src/transcript/` are shared. Delivery, auth, and
retry are not — they belong to each consumer, and always will.

## Step 5 — the envelope

If the agent needs a metadata field no existing envelope header carries, stop
and read `src/envelope.rs` first. The `X-Tapes-*` envelope is a cross-language
contract: this crate produces it in Rust and the Go parsers in tapes read it
back, and both halves table-test against one shared fixture corpus vendored at
`vendor/tapes-envelope-fixtures/`.

Adding a harness id is routine. Changing producer behaviour is not: update the
fixture corpus in the tapes repository first, then re-vendor with
`scripts/sync-envelope-fixtures.sh`. The oracle in `src/envelope_fixtures.rs`
must stay green against the vendored corpus — if a change makes it fail, that
conversation happens in tapes, not by editing the fixtures here.

Most harness-specific metadata does not need a new header at all. It belongs in
the base64url metadata blob, which is where Codex puts its originator, source,
and rollout path.

## What each field buys you

Step 1 said the declaration is load-bearing rather than documentation. This is
the ledger behind that claim: what each field causes, and where the code that
acts on it lives. Read it as a checklist against your own entry — a field whose
behaviour you do not want is a field to leave at its inert variant.

One caveat first. A field only reaches a consumer that consults it, and not
every consumer does yet. Where a consumer still hardcodes what the registry now
describes, this section says so rather than describing the intent as though it
were the behaviour.

### `id`

The canonical name, and the most load-bearing string in the crate:

- It is the `X-Tapes-Harness-Id` value, so it must already exist as a
  `HARNESS_ID_*` const in `src/envelope.rs` — `HARNESS_ID_CLAUDE`,
  `HARNESS_ID_CODEX`, `HARNESS_ID_OPENCODE`, `HARNESS_ID_PI` today — rather
  than being spelled inline here. `registry_ids_are_the_envelope_ids` pins the
  two together.
- It is what `supported_agents()` returns, and so the name a consumer offers.
- It is what `LaunchRecipe::harness()` must return; each recipe pins that in
  its own test (`src/launch/claude.rs`, `src/launch/codex.rs`,
  `src/launch/opencode.rs`).
- It is the key the tapes deriver reads on the far side of the wire, which is
  why renaming one is a two-repository change and not a rename.

It must also differ from `HARNESS_ID_UNKNOWN`. `"unknown"` is the miss sentinel
`Attributed::UnknownHarness` stamps, so a harness answering to it would be
indistinguishable from a failed attribution; the registry test loops the whole
set asserting that.

### `aliases`

The extra spellings `find()` accepts, through `matches_name()`, which trims and
compares case-insensitively so a consumer can pass a CLI argument straight in.
Two consequences before you add one:

- Aliases are **accepted but never advertised**. `supported_agents()` maps over
  `id()` alone, so `claude-code` resolves while `claude` is the name consumers
  print. The asymmetry is deliberate — one name to display, several to forgive.
- Every id and alias must be unique across the registry, since `find()` returns
  the first match and a duplicate would make resolution depend on declaration
  order. `every_name_in_the_registry_is_unique` enforces it.

An empty list is the common case: `CODEX`, `OPENCODE`, and `PI` all carry none.

### `user_agent`

The routing rule `for_user_agent()` applies, and the gate on the Claude
attribution lane — `ua_matches_claude()` in `src/attribution/pipeline.rs` is a
registry lookup rather than a hand-written prefix test.

`UserAgentMatch::Prefix` is a **prefix, not a substring**: `some-claude-like`
must not be claimed by `claude`. Because `for_user_agent()` returns the first
entry that matches, two prefixes where one is a prefix of the other would
resolve by declaration order; `user_agent_rules_are_pairwise_disjoint` pins
that they never nest.

`UserAgentMatch::None` is not a gap. It says the harness is identified by other
evidence — a route the consumer recognises (`RequestFacts::codex_route`), a
launch marker, or the harness's own envelope. Codex declares `None` precisely
because its SDK's User-Agent is not harness-specific, so a prefix would either
miss real traffic or claim someone else's. If you cannot name a prefix that is
true of your harness's traffic and false of everyone else's, declare `None` and
identify it another way.

### `launch`

Read through `is_launchable()`, which is exactly the predicate
`supported_agents()` filters on. The variants differ in *who plans* the launch,
not in whether one happens:

- **`Recipe`** promises a `LaunchRecipe` in `src/launch/` whose `harness()`
  returns your id. The consumer constructs it — the registry deliberately holds
  no recipe instances, because recipes carry per-harness inputs (Claude needs an
  endpoint; Codex an endpoint plus an auth mode and a provider identity;
  opencode an endpoint per provider plus a model) and a registry that built them
  would need the union of every harness's configuration.
- **`ConsumerOwned`** means launchable, but the harness has no base-URL knob for
  a recipe to set, so capture depends on an installed extension plus whatever
  argv loads it — and that argv is not shared yet. `PI` is the only one.
- **`Unsupported`** keeps the harness out of every consumer's launchable list
  while leaving the rest of the entry — id, User-Agent rule, attribution
  strategy — fully in force. A harness you can capture but not start is a
  legitimate entry.

### `attribution`

The declarative statement of which shape the harness's identity recovery has,
and so which submodule under `src/attribution/` owns it. Step 3 covers what
each variant means; two behaviours hang off the *value* rather than off which
modules happen to exist:

- `SelfAttributing` is why `Attributed::stamp` preserves a complete inbound
  envelope instead of overwriting it with `harness_id: unknown`.
  `pi_is_the_self_attributing_variant` pins that exactly one harness is in that
  state, so a second one is a deliberate decision rather than a silent
  generalisation of that branch.
- `None` means traffic is still captured, under `harness_id: unknown`. It costs
  the session its identity, not its turns.

This field also reaches furthest downstream of any of them. paper's
`SUPPORTED_AGENTS` is defined as the registry's *attribution-capable* subset —
see "What a registry change sets off" — so this is the field that decides
whether your harness appears in `paper start` at all.

### `transcripts`

Where the harness's on-disk transcripts live, resolved by `transcript_root()`.
This matters more than its size suggests: wire capture yields a complete call
inventory but **no causal or fork skeleton**, and that skeleton exists only in
these files. A harness with `TranscriptSource::None` produces sessions whose
calls are all present and whose subagent structure is entirely absent.

`ClaudeProjects` resolves to `~/.claude/projects`; `CodexRollouts` delegates to
`codex::session::default_sessions_dir()` so `$CODEX_HOME` is honoured in one
place rather than re-implemented per caller. Honour whatever home-directory
override your harness itself honours, the same way.

`declared_transcript_trees_resolve_to_a_path` pins the pairing in both
directions: declaring a tree the crate cannot locate fails, and so does
`TranscriptSource::None` resolving to something.

### `plugin`

Whether capture needs a file installed **into** the harness.
`plugin_artifacts()` flattens the variants to a slice, and that slice is the
whole input to a `plugin install`: resolve the typed name through `find()`,
take the slice, write each artifact beneath the user's home.
`HookManifestTemplates` deliberately flattens to the empty slice — templates
carry un-rendered slots, so a file-copy installer must see nothing to copy;
an installer for that shape renders through `src/plugin/codex_app.rs` and
packages the result for the harness's own plugin manager instead.

The empty slice is the ordinary case and is not an error. An installer must be
able to tell "nothing to do" from "no such harness" — `tapesctl plugin install
claude` says the harness needs no plugin, writes nothing at all, and exits zero
(`crates/tapesctl/src/plugin.rs` in the tapesctl repository).

## Partial entries and full ones

The registry holds five entries and not all of them are complete:

| field | `CLAUDE` | `CODEX` | `CODEX_APP` | `OPENCODE` | `PI` |
| --- | --- | --- | --- | --- | --- |
| `user_agent` | `Prefix("claude")` | `None` | `None` | `None` | `None` |
| `launch` | `Recipe` | `Recipe` | `Unsupported` | `Recipe` | `ConsumerOwned` |
| `attribution` | `SessionsDir` | `OpenRollout` | `LifecycleHooks` | `None` | `SelfAttributing` |
| `transcripts` | `ClaudeProjects` | `CodexRollouts` | `CodexRollouts` | `None` | `None` |
| `plugin` | `None` | `None` | `HookManifestTemplates` | `None` | `BundledExtension` |

`OPENCODE` is the honest partial entry, and the shape most new harnesses should
start in. It is worth being precise about what that state does and does not buy,
because "it's in the registry" is easy to over-read.

**What a partial entry gets you.** `find()` resolves the name, so a consumer
answers "this harness needs no plugin" instead of "unknown harness".
`supported_agents()` includes it, so anything deriving its list from the
registry offers it without being edited. And with `LaunchSupport::Recipe` there
is a real, tested recipe in the crate — `src/launch/opencode.rs` plans the
config document that points opencode at the proxy — so the *crate* can plan the
launch that makes its traffic capturable.

**What it does not get you.** No attribution lane, which is the whole of the
difference. opencode traffic matches no User-Agent rule and arrives on no Codex
route, so the pipeline returns `Attributed::UnknownHarness` and the envelope is
stamped `harness_id: unknown`. The turns land; the session is anonymous. And
with `TranscriptSource::None` there is no fork skeleton to upload, so even a
session you later identify by hand has a complete call inventory and no
subagent structure.

**What it does not get you *yet*, for reasons outside this crate.** Neither
shipped consumer actually starts opencode today, despite `LaunchSupport::Recipe`
and `supported_agents()` both saying it is launchable:

- `tapesctl start` does not consult the registry at all. It keeps a local
  two-variant enum (`crates/tapesctl/src/start/mod.rs`) with a hand-rolled
  `parse()`, a hardcoded error string listing `claude, codex`
  (`crates/tapesctl/src/error.rs`), and a test asserting that `opencode` is
  *rejected*. Only `tapesctl plugin install` is registry-derived.
- paper's `SUPPORTED_AGENTS` is pinned to the registry's attribution-capable
  subset, and `AttributionStrategy::None` is exactly what that filter excludes —
  so a partial entry is invisible to `paper start` by construction, not by
  oversight.

Take that as the realistic bar rather than a discouragement: a declaration plus
a recipe is a genuine, landable contribution, and landing it is what makes the
attribution work reviewable in isolation afterwards. But if your goal is
`<consumer> start <your-harness>` working end to end, the registry entry is
necessary and not sufficient, and the remaining work is a pull request against
the consumer.

## Proving a new attribution lane

Attribution is the part of this crate you cannot review your way to confidence
in. Both bugs below shipped with unit tests that passed, and neither was caught
by them: the tests were right about the logic and wrong about the world. If you
are writing a lane, budget for the evidence below rather than treating it as
polish.

### Disambiguate by the harness's own thread identifier, never by recency

A harness running subagents is frequently **one process** holding the parent's
transcript and every child's open simultaneously. Neither the PID nor a launch
marker identifies a *thread*, so every candidate looks equally live and a
recency tie-break attaches a turn to whichever thread flushed last. This is the
worst class of attribution bug, because the result is a well-formed session with
a silently wrong shape.

- Find the harness's native per-thread identifier and confirm it is per-thread,
  not per-process. Codex stamps it on every inference call, and
  `CODEX_ROLLOUT_ID_HEADERS` reads `thread-id` **before** `session-id` on
  purpose: `session-id` stays pinned to the root session, so reading it first
  would attribute every subagent turn to the parent — the bug rather than the
  fix.
- Narrow candidates by that identifier *before* any tie-break, on **every**
  path — including timeout and discovery fallbacks. Those were fixed separately
  from the main lanes, because a discovery timeout is exactly the moment the
  named thread is the file the watcher has not surfaced yet, so the fallback is
  where a child's turn most easily lands on the lone visible parent.
- Treat "the request named a thread that none of the candidates are" as a
  **refusal**. The request is authoritative about its own identity, so "none of
  these" is information, not a licence to tie-break over the rest
  (`narrow_by_rollout_id`).
- Treat an **empty** candidate set as a plain miss instead. The file may still
  be appearing on disk, and the bounded poll is what waits for it.
- Treat "exactly one candidate matched" as evidence only when it matched *on the
  identifier*. A single most-recent candidate is a guess wearing an exact
  match's clothes, and it is indistinguishable from real evidence in a log line.
- Absent identifier means **no evidence**, never "matches nothing" — a blank
  value reaching a matcher refuses every candidate.

### Verify path encoding against the harness's real on-disk layout

Claude encodes a project directory by mapping **both** `/` and `.` to `-`:
cwd `/Users/x/.claude/jobs/…` lands at `-Users-x--claude-jobs-…`, with a double
dash where `/.` was. `encode_cwd` mapped only the slash, so every cwd containing
a dot resolved to a directory that does not exist.

The failure was **silent**. `session_files()` on a missing directory returns an
empty set (`src/transcript/files.rs`), so the affected sessions lost every
transcript turn *and* their fork-parent evidence with no error raised anywhere.
It was found by the first smoke run that happened to start from a dotted cwd.

- Derive the encoding from **real directories on disk**, not from the harness's
  documentation or your reading of its source. List the transcript root and
  compare it against the cwds you actually ran from.
- Test a dotted cwd explicitly, and pin it byte-for-byte — the double-dash case
  is pinned that way in `src/attribution/claude/fork_parent.rs` so a future
  encoding change is an obvious diff. Test whatever else your harness's users
  really have in paths: spaces, `@`, non-ASCII.
- Make a resolved-but-missing transcript root **warn loudly**. Today an empty
  upload set means both "this session wrote nothing" and "we computed the wrong
  path", and that ambiguity is what let a one-character bug survive. If your
  lane resolves a path it expects to exist, say so when it does not.
- Do not try to decode the encoding. It is not reversible — with dots mapped
  too, `/opt/my-project` and `/opt/my.project` now encode identically. `sweep`
  reads the true cwd out of the transcript's own records instead, which is the
  pattern to copy.

### Prove it end to end, from two different cwds

Unit tests cannot reach either bug above. The acceptance evidence is a real
session:

- Run a **real session through the real harness** — not a fixture, not a
  replayed cassette — and make it spawn subagents.
- Verify the captured session actually shows **transcript turns**. Zero turns
  with no error is the encoding failure, and it looks like a quiet success.
- Verify the **thread structure is correctly attributed**: each subagent's turns
  on that subagent's thread, the parent's on the parent's. A family collapsed
  onto one thread is the disambiguation failure, and it renders as a perfectly
  valid session.
- Do both **from a plain cwd and from a dotted cwd**. Only the second exercises
  the encoding path, and a lane proved from one is proved for half its users.
- Read the logs for refusals. A lane that never refuses anything is more
  suspicious than one that sometimes does — under the refuse-rather-than-guess
  rule, silence usually means the evidence was never consulted.

## What a registry change sets off

Adding or modifying an entry is a small diff with a wide blast radius. This is
the enumeration, so you can update deliberately rather than chase failures.

**In this crate**, the invariant tests at the bottom of `src/harness.rs` run
against the whole registry, so `cargo test` is the checklist:

- `supported_agents_is_the_launchable_subset_in_registry_order` asserts the full
  expected list literally. **Any** launchable addition fails it — that is the
  point, so update the expectation as a decision.
- `registry_ids_are_the_envelope_ids` asserts each id against its
  `src/envelope.rs` const by name; add a line for yours. Its loop over the miss
  sentinel covers new entries automatically.
- `every_name_in_the_registry_is_unique` fails on a duplicated id or alias.
- `user_agent_rules_are_pairwise_disjoint` fails if your prefix nests with an
  existing one.
- `harnesses_without_a_user_agent_rule_claim_nothing` and
  `declared_transcript_trees_resolve_to_a_path` both loop the registry, so a new
  entry is covered without being named.
- `pi_is_the_self_attributing_variant` fails if a second `SelfAttributing`
  harness appears — deliberately, since the envelope-preserving branch needs
  revisiting rather than silently generalising.
- `names_resolve_case_insensitively_through_aliases` asserts `find("gemini")` is
  `None`. Step 1's example is not idle: `gemini` is currently pinned as *absent*
  here, and separately in tapesctl's plugin tests, so that particular name
  breaks two test suites the day it becomes real.
- Outside the registry module: a new `HARNESS_ID_*` const in `src/envelope.rs`,
  and a `harness()` test in your `src/launch/` recipe if you add one.

**Downstream, but only when a consumer bumps its pin.** Both consumers depend on
this crate by git revision, so nothing breaks in paper or tapesctl the moment
you land here — it breaks for whoever bumps the pin next, which is why the
failures below are worth naming in your pull request description.

In paper (`platform/paper`):

- `crates/paper/src/cli/start.rs` — the test
  `supported_agents_match_registry_attribution_set` asserts that
  `SUPPORTED_AGENTS` equals the registry's attribution-capable ids
  **in registry order**. So a harness with any strategy other than
  `AttributionStrategy::None` breaks it, and a partial entry does not. Fixing it
  means updating the const *and* the `AgentKind` enum, `agent_kind()`, and the
  launch dispatch that the const gates — all in the same file.
- Prose lists in the same file (the "agent is required" message) and the
  shell-init tests in `crates/paper/src/cli/shell.rs` restate the agent names by
  hand and will not fail loudly.
- `crates/paper-daemon/src/proxy/session_recording/real.rs` hand-rolls the
  `claude` User-Agent prefix as a literal rather than calling
  `for_user_agent()`, so a change to a `user_agent` rule here does not reach it.

In tapesctl (`telemetry/tapesctl`), everything registry-derived lives in
`crates/tapesctl/src/plugin.rs`:

- `a_name_the_registry_does_not_know_is_refused_with_the_ones_it_does` resolves
  `gemini` and expects failure, and asserts the derived "known" list still
  contains `claude` and `pi`.
- `a_name_resolves_through_the_registrys_aliases_and_casing` pins the
  `claude-code` alias and the `pi` id.
- `installing_for_a_harness_with_no_plugin_succeeds_and_writes_nothing` pins
  `claude` at `PluginDelivery::None`; giving it an artifact breaks the test.
- `crates/tapesctl/src/start/mod.rs` and `crates/tapesctl/src/error.rs` restate
  the launchable set by hand and will not fail when the registry grows.

## The other two places

This crate is one of three places that hold harness knowledge. Full support for
a new agent also needs:

1. **The tapes deriver** — turns captured wire traffic into the session model.
   It keys on the `harness_id` you declared here.
2. **The envelope spec and fixtures** — only if the wire contract itself
   changes.

So a new harness is normally two pull requests: this crate, then the deriver.
A declaration-only entry (step 1, plus a launch recipe) is useful on its own
and can land first.

Add one more for each consumer whose `start` you want your harness to appear
in, until those consumers derive their lists from the registry rather than
restating them — "Partial entries and full ones" says where that stands.

## Before you open a pull request

```bash
nix develop
make check   # build + fmt-check + clippy + test
```

The crate denies `unwrap`, `expect`, and `panic` via `[lints]`; return `Result`
and surface errors through the crate error types.

Pull request titles use one of the repository's contribution labels — `✨ feat:`,
`🔧 fix:`, `🧹 chore:`, `📚 docs:` — and reference the relevant issue.
