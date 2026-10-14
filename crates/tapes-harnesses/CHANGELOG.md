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

### Added

- `launch::PiRecipe`, `launch::PI_AGENT_DIR_ENV`, `launch::PI_CARRIED_FILES`,
  `launch::PLACEHOLDER_API_KEY`, `launch::pi_gateway_model`, and
  `launch::pi_user_agent_dir`: declare gateway-served models for one pi session.
  The plan relocates `PI_CODING_AGENT_DIR`, writes a `models.json` that adds
  each model under `openai` with `api: "openai-completions"` and a model-level
  `baseUrl` at the proxy, writes the capture extension into the relocated
  `extensions/`, and selects the first model with `--model openai/<id>`. Only
  files named in `PI_CARRIED_FILES` (settings, keybindings, user-level
  instructions) may be carried from the user's agent directory — never
  `auth.json`, `mcp.json`, or the user's `models.json`.
- `plugin::ALLOW_UNCAPTURED_ENV` (`TAPES_ALLOW_UNCAPTURED`) and
  `plugin::ALLOW_UNCAPTURED_ON`, and `plugin::pi::ROUTED_PROVIDERS`.
- The pi extension stamps `x-tapes-cwd` (percent-encoded with the producer's
  escape set) and `x-tapes-harness-version` (pi's own version), within the
  8 KiB envelope budget; cwd is dropped first when over it.

### Changed

- While routed through a gateway, the pi extension refuses a request on a
  model whose endpoint is not on the gateway — a provider it does not
  redirect, a `models.json` provider with its own `baseUrl`, any provider
  other than a launch-declared one under `TAPES_GATEWAY_PROVIDER_CONFIG` — with
  a one-line error naming the provider and host, instead of a UI warning and an
  uncaptured request. Selecting such a model reports the same line as an
  error. `TAPES_ALLOW_UNCAPTURED=1` restores pass-through with a warning.
  Without `TAPES_GATEWAY_URL` the extension stays inert.

## [0.2.0] - 2026-10-13

### Added

- The `hermes` harness (hermes-agent, alias `hermes-agent`): `Recipe`-launched,
  `SelfAttributing`, no transcript tree, and a bundled plugin. It now appears in
  `supported_agents()`, after `pi`.
- `launch::HermesRecipe` and `launch::HERMES_HOME_ENV`: plans a relocated
  `HERMES_HOME` whose `config.yaml` declares a `custom` provider at the proxy
  endpoint, enables the capture plugin, and carries the plugin's files. It
  copies nothing from the user's own hermes home — no `.env`, no `auth.json`.
- `plugin::HERMES_GATEWAY_PLUGIN`, `plugin::HERMES_GATEWAY_MANIFEST`, and
  `plugin::HERMES_GATEWAY_PLUGIN_NAME`: a hermes directory plugin whose LLM
  request middleware stamps the `X-Tapes-*` envelope (including a delegated
  subagent's parent session and `x-tapes-parent-harness-id`) and the
  capture-nonce echo on requests bound for `TAPES_GATEWAY_URL`.
- hermes auxiliary calls (title generation, compression, vision, web_extract,
  session_search, skills_hub, mcp) carry the same envelope as main turns, plus
  `x-tapes-harness-metadata` `{"aux_task": …}`. The plugin stamps them through
  hermes's shared auxiliary kwargs builder, guarded by version; the planned
  `config.yaml` also sets `model.default_headers` to a static
  `x-tapes-harness-id: hermes`. In a process running several concurrent
  sessions, an auxiliary call may attach to the most recent one.
- `LaunchError::SerializeHermesConfig`.

### Changed

- **Breaking**: requires `tapes-capture` 0.2, whose `TapesAttribution` this
  crate's attribution API returns.
- `REGISTRY` and `supported_agents()` include `hermes`; a consumer test that
  pins either list literally needs the new entry.

## [0.1.2] - 2026-09-29

### Added

- Add the `cursor` harness (alias `cursor-agent`) and `transcript::cursor_stream`
  helpers for argument planning, stream validation, spool discovery, and upload
  payloads. Consumers opt into `LaunchSupport::StructuredStdout`; Cursor is
  excluded from `supported_agents()`.

## [0.1.1] - 2026-09-13

### Added

- Launch-local Pi providers through `TAPES_GATEWAY_PROVIDER_CONFIG`, with a
  consumer-supplied model catalog and gateway-managed authentication. Ordinary
  provider definitions and user auth files remain unchanged.
- Runtime validation of required model fields before provider registration,
  with capture nonce and session attribution preserved across registration.

## [0.1.0] - 2026-08-13

The first release. `0.1.0` is the contents of the crate at publish rather than
a list of changes — the seams are listed in [`README.md`](README.md).

Adding a harness to the registry is an addition rather than a break, but it is
the change consumers most want to read about, because they derive their
supported-agent lists from that registry. Note the required `tapes-capture`
version here too when it moves: this crate cannot resolve on crates.io until
that release is live, so the requirement is part of what an upgrade costs.
