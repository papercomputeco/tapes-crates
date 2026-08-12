# tapes-mock-upstream

**Internal test support. Not published, not a public API, no stability promise.**

A streaming mock provider upstream, a mock ingest, and the scripted one-shot
recipes and runner behind the harness regression matrix. Everything here exists
so a test can launch a *real* harness binary against a *real* HTTP server and
assert on what actually crossed the wire.

If you are looking for what the matrix covers and how to run it, that is
[`docs/harness-matrix.md`](../../docs/harness-matrix.md). This file is about why
the code is shaped the way it is.

## Why a crate, and not a feature or a binary

The mock had to live somewhere, and three shapes were plausible. The deciding
question turned out not to be taste but reachability.

**A cargo feature on `tapes-harnesses`** was the smallest change and does not
work. A feature-gated module is reachable from that crate's own tests, but the
matrix launches harnesses *through* the crate rather than inside it, and an
integration test in a sibling crate can only see items the gated crate exports —
so the gate would become part of `tapes-harnesses`' public surface. That crate
publishes a deliberate boundary (`README.md`, "The public API boundary"), and a
mock HTTP server is not on the right side of it. Feature unification makes it
worse: a consumer taking `tapes-harnesses` with the feature on anywhere in its
graph would link a test server into a production build.

**A bare binary** — run the mock, point a harness at it, assert externally —
inverts the problem. The interesting assertions are about correlating what the
upstream received with what ingest concluded, which means a test needs
programmatic access to both recorders. A binary would have to grow a control API
for that, and a control API is a second implementation to keep in step with the
one the tests use.

**A test-support crate** is what is here: the recorders are ordinary Rust values,
a test holds them directly, and the crate is invisible to anything that does not
name it. The cost is one more workspace member, and it is priced correctly —
`crates/*` is a member glob, so the crate joined every existing lint, build, and
test invocation without a line of configuration.

The thin binary still exists, but as a shell over the library rather than an
alternative to it: `cargo run -p tapes-mock-upstream` stands the pair up for
manual debugging. It contains no routing of its own, so the mock somebody
debugs by hand and the mock the matrix drives are the same code.

## Why the mock and the recipes are one crate

The mock pair and the matrix machinery are separable and are not separated. Two
crates would double the manifest, the CI surface, and the dependency edges for
roughly four hundred lines of code, and the second crate would have exactly one
consumer — the first one's test directory.

The split becomes worth making the moment a consumer wants the mock pair without
the recipes, which is a plausible future: `tapesctl`'s own tests would benefit
from a real streaming upstream. Until somebody actually does that, the seam is
the module boundary, and `upstream` / `ingest` depend on nothing in `recipe`.

## Why the HTTP server is hand-written

The mocks this consolidates were `wiremock::ResponseTemplate::set_body_string`
with a `text/event-stream` content-type attached: the entire "stream" arrived in
one write, complete, before the client read a byte. That satisfies a proxy that
tees bytes without looking at them. It does not satisfy a real harness, and real
harnesses are the whole premise here.

Regaining per-event flushes means owning the socket writes, and `wiremock` has
no streaming-body API to own them through. Reaching for axum or hyper to recover
control that then has to be fought for is a poor trade in a repository whose
entire dependency list is twelve lines — it would add an async runtime and a few
dozen transitive crates so that a test-support crate could do what three hundred
lines of `std::net` do directly.

Blocking I/O also buys something concrete: the matrix runner is a plain `#[test]`
with no runtime to spawn or leak, and a harness subprocess can be waited on
without an executor in the way.

## The one rule everything here follows

**A cell that cannot run says so, by name, with a reason.** Not a silent
omission, not an early `return`, not a `#[ignore]`. A matrix that quietly dropped
what it could not run would look identical whether it covered five harnesses or
one, and a coverage claim nobody can audit is worse than no claim at all.

That rule is why `manifest::Status::Skipped` carries a mandatory reason, why the
runner prints every skip whether or not the run failed, and why `codex-app` — a
harness that can never run a Tier-1 cell — still has a recipe entry.

## Layout

| module | owns |
| --- | --- |
| `http` | A small blocking HTTP/1.1 server, and the request/response types the mocks are written against. Chunked SSE with a flush per event. |
| `upstream` | The provider surfaces: Anthropic Messages, OpenAI Responses (three paths), OpenAI chat-completions. One recorder over all of them. |
| `ingest` | The turn sink, and the envelope reader — checked case by case against the vendored fixture corpus, so "attributed" means here what it means everywhere else. |
| `recipe` | One scripted one-shot recipe per registry harness, wrapping the real launch recipes rather than restating them. |
| `manifest` | What a run ran against, including what it skipped and why. |
