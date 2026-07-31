# Adding a harness

This crate is client-side harness knowledge: how to launch a coding agent
under a capture proxy, how to work out which session a request belongs to,
where the agent writes its transcripts, and what goes in the `X-Tapes-*`
envelope. Teaching it about a new agent starts in one file — `src/harness.rs` —
and grows from there only as far as that agent's shape demands.

Start by reading `src/harness.rs`. The types there are the vocabulary this
document uses.

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
it up, so it appears in every consumer's `start` command without any consumer
being edited; `find()` resolves its name and aliases from a CLI argument;
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

## Before you open a pull request

```bash
nix develop
make check   # build + fmt-check + clippy + test
```

The crate denies `unwrap`, `expect`, and `panic` via `[lints]`; return `Result`
and surface errors through the crate error types.

Pull request titles use one of the repository's contribution labels — `✨ feat:`,
`🔧 fix:`, `🧹 chore:`, `📚 docs:` — and reference the relevant issue.
