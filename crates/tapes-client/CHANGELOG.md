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

## [0.4.0] - 2026-08-18

### Changed

- **Breaking for deployments without the export and skills cassettes**: the
  export and skills operations now target their cassettes' routes, the same
  move `searchSpans` made in 0.3.0 — `exportSession`/`exportSessions` go to
  `/v1/cassettes/export/sessions[/{id}]`, and every skills operation goes to
  `/v1/cassettes/skills/...`. The reroute sits below every calling surface —
  named methods, the generic `call`/`stream` escape hatches, and
  `request_for` all agree. Parameters, bodies, and models are unchanged; a
  deployment not serving the cassette answers 404 where core's copy once
  answered.
- `listSessionSkills` additionally changes shape: core spelled it
  `GET /v1/sessions/{id}/skills`, and the skills cassette serves the same
  listing as `GET /v1/cassettes/skills?session_id={id}`, so the path
  parameter travels as a query parameter. The typed method and its response
  model are unchanged.

### Added

- `ops::GET_SKILL_MARKDOWN`: the one skills operation id consumers had been
  spelling as a local string literal.
- A completeness gate driven by the vendored contract: every operation whose
  sealed path lives on an extracted surface must be rerouted, so a contract
  refresh that adds an operation under a moved surface fails the build until
  it is routed.

### Fixed

- The named methods' doc titles now state the cassette routes their requests
  actually target; 0.3.0's rerouted `searchSpans` and this release's export
  and skills methods had kept their core-route titles.

## [0.3.0] - 2026-08-17

Span search moves to the search cassette's route, and the discovered
cassette surface can be loaded live-first. The minor bump is for the first
of these: no Rust API changes shape, but a caller's requests go somewhere
new.

### Changed

- **Breaking for deployments without the search cassette**:
  `CoreClient::search_spans` now targets `GET /v1/cassettes/search/spans`
  — the search cassette's serving of the identical request and response
  contract — instead of core's retirement-bound `/v1/search/spans`. A
  deployment that does not serve the search cassette answers 404 where the
  previous version still found core's route. The parameters, models, and
  every other operation are untouched.

### Added

- `cassettes::cache::load_live`: a live-first alternative to `load` for
  the shapes where the listing is the product (a `--help` that validates
  what a deployment is vending). Discovery runs under a caller-supplied
  deadline with ETag revalidation; the on-disk cache stands in only when
  the server cannot answer, labeled through the new `Provenance` enum
  (`Live`, `TimedOut`, `FetchFailed`) so a consumer can say so instead of
  silently serving stale. The cache-first `load` is unchanged.
- The `direct-http` feature now carries a `tokio` (time-only) dependency
  for `load_live`'s deadline; feature-less and `cli`-only builds are
  unaffected.

## [0.2.0] - 2026-08-14

A refresh of the vendored read contract, from tapes v0.34.0 to v0.36.0. No
Rust API is removed or renamed; the one addition is a new optional field.

### Added

- `StatsParams` gained `auth_subject`: `getStats` now takes the same
  gateway-stamped JWT subject filter the sessions listing takes, so a
  personal surface can show totals that agree with the rows beside them.
  Unset, it is omitted and the totals stay org-wide.

### Changed

- The vendored contract's `listSessions` filter semantics moved, and the
  `SessionListParams` field docs with them: `harness_session_id` alone is now
  accepted and matches across all harnesses (at most one row per harness),
  while `harness_id` alone is rejected with a 400 — it names a harness, not a
  session. Either combined with `cursor`, `sort`, `direction`, `since`, or
  `until` is refused with a 400 where the incompatibility was previously
  narrower. The typed structs are unchanged in shape; what moved is what the
  server accepts.
- The contract's `StatsResponse` prose now pins down `tool_calls`: the sum of
  the turn rollups' tool span counts, windowed on each turn's `started_at`
  like every other stats figure. Documentation only — the field's type and
  name are unchanged.

## [0.1.0] - 2026-08-13

The first release. `0.1.0` is the contents of the crate at publish rather than
a list of changes — the seams are listed in [`README.md`](README.md).

Two things here move for reasons outside a normal code change, and both belong
in this file when they do: a refresh of the vendored read contract under
`contracts/`, which can add or change operations without a line of Rust being
touched, and a change to which features are on by default, since `cli` and
`direct-http` decide whether the crate pulls in clap and reqwest at all.

### Added

- **Typed models for the sealed surface** (`core::models`). Every response and
  request shape the vendored contract declares, as Rust types, plus typed
  parameter structs for the operations that take parameters. `CoreClient`'s
  named methods return them; `list_all_sessions` / `list_all_skills` follow
  `next_cursor` to the end through the crate's one pagination convention.
  Request bodies are a caller's to build — nested components included — and the
  partial-update bodies (`UpdateSkillRequest`, `SessionUpdateRequest`) omit
  what a caller leaves unset, so a one-field update applies that field and
  leaves the rest of the record alone.
- **A schema-coverage gate** (`core::models::coverage`), the shape-level sibling
  of the operation gate. It synthesises a document from each schema, decodes and
  re-encodes it through the registered model, and reports by path anything the
  model does not carry — so a contract bump that adds a field fails the build
  instead of dropping data silently. It also holds the typed parameter sets and
  the closed value sets to what the document declares, in both directions.
- **`HttpAuth`, the credential seam** (`direct-http`). A consumer that needs an
  authorised request writes two small methods — `authorize` (headers for this
  attempt) and `on_unauthorized` (retry, surface, or fail with its own error) —
  instead of a whole `TapesTransport`. The engine owns the retry loop and caps
  it, so the policy is data rather than a loop rewritten per consumer.
- **`HttpEngine`**, the HTTP half that hook plugs into, with `.under_base()` for
  gateway-mounted deployments and `.with_client()` for a caller's own TLS, proxy,
  or connection pool.

### Changed

- `DirectHttp` is now `HttpEngine<NoAuth>` — the same name, the same
  `direct-http` feature, the same constructor and methods, and still no
  credential. Nothing that used it needs to change.
- `CoreClient`'s named methods return the models above rather than a type the
  caller names. The generic seam is unchanged and stays the escape hatch:
  `CoreClient::call` is still generic in its response type and still reaches
  every operation by `operationId` — which is what the fidelity reads want.
- `CoreClient` gained named methods for the rest of the sealed surface: session
  update and delete, session skills, stats, the skills operations, and the
  whole-deployment export.
