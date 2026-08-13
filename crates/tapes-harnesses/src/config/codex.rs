//! The codex `config.toml` provider-patch grammar.
//!
//! Ported from a closed-source installer's codex integration — the merge, the
//! table-creation helper, the obsolete-override scrub, and the read-back half
//! of its intent inference. That is the code that has been configuring the
//! Codex app and CLI against a capture proxy in the field, and this module is
//! the harness-grammar part of it with the deployment parts left behind.
//! The `-c` argv spelling of the same provider declaration lives in
//! [`crate::launch::codex`]; this is its persistent form, written into
//! `$CODEX_HOME/config.toml` so the Codex *app* — which nobody launches with
//! argv overrides — routes through the proxy too.
//!
//! # What a patch does
//!
//! [`apply_provider`] makes the document say, in full:
//!
//! * `model_provider = "<id>"` — select the provider as codex's active one;
//! * `[model_providers.<id>]` — declare it: `name`, `base_url`,
//!   `wire_api = "responses"`, the optional attribution header, and the auth
//!   keys for the chosen [`CodexAuth`] mode (setting one mode's keys and
//!   *removing* the other's, so switching modes never leaves both behind).
//!   Attribution entries are a managed set, not append-only: an
//!   `http_headers` entry carrying the provider id as its value is this
//!   grammar's own shape, and any such entry that is not the current header
//!   is scrubbed, so renaming the header cannot accumulate stale ones;
//! * `features.enable_request_compression = false` — a compressed request
//!   body is opaque to a capture proxy, in this grammar exactly as in the
//!   launch recipe's;
//! * removal of any caller-named `shell_environment_policy.set` overrides —
//!   the hook a consumer uses to retire config *it* wrote in earlier versions.
//!   The installer this grammar came from scrubs an obsolete executable-path
//!   override through exactly this hook.
//!
//! Everything else in the file is the user's and survives byte-for-byte.
//!
//! # Fidelity: comments, ordering, and idempotence
//!
//! The grammar edits via `toml_edit`, as the installer it came from does, so
//! the user's comments, whitespace, and key order are preserved wherever the
//! patch does not rewrite a value. Keys the patch (re)sets keep their existing
//! position; keys and tables the patch introduces are appended in the positions
//! `toml_edit` chooses. That is the whole of the fidelity promise — no more:
//! formatting *inside* a value the patch owns is normalised to `toml_edit`'s
//! default rendering when the value changes, and a freshly created
//! `model_providers` container renders a bare `[model_providers]` header rather
//! than an explicit table (see the canonical fixture below).
//!
//! Idempotence is textual, and [`is_provider_applied`] is deliberately the same
//! probe a reconciling installer uses: apply the patch to the current text and
//! compare bytes. A config is "applied" exactly when reapplying would change
//! nothing, which is also the condition under which an installer can skip
//! telling the user to restart.
//!
//! # What is deliberately *not* here
//!
//! * **Route construction.** `base_url` arrives fully built. Which path
//!   prefix means ChatGPT-login pass-through versus API-key pass-through is
//!   the consumer's proxy topology (see [`CodexAuth`] in
//!   [`crate::launch::codex`] for the conventions).
//! * **Filesystem and environment.** Resolving `$CODEX_HOME`, reading the
//!   file, atomic writes, and restart messaging stay with the consumer.
//! * **Removal side-cars.** The installer this came from also writes its own
//!   handoff and intent files next to `config.toml`; those are deployment
//!   state, not harness grammar, and stay with that consumer.
//! * **An uninstall precedent.** No consumer had an uninstall path to port, so
//!   [`remove_provider`] is defined here — conservatively — rather than
//!   inherited, and every consumer grows the same one. See its docs for exactly
//!   what it leaves behind.

use snafu::{ResultExt, Snafu};
use toml_edit::{Document, Item, Table, value};

use crate::launch::{CODEX_API_KEY_ENV, CodexAuth};

/// The wire protocol every patched provider is pinned to, one spelling with
/// the launch recipe's (`wire_api = "responses"`) so the persistent and
/// per-process declarations cannot drift.
const WIRE_API: &str = "responses";

/// A caller-supplied description of the provider to patch into
/// `config.toml`.
///
/// Build with [`CodexProviderPatch::new`] and refine with the builder-style
/// setters, mirroring [`crate::launch::CodexRecipe`]. Identity, branding, and
/// endpoint are all the caller's: each consumer passes its own provider id,
/// its own display name, and a route its own proxy serves. Nothing here
/// supplies a default for any of the three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProviderPatch {
    provider_id: String,
    display_name: String,
    base_url: String,
    auth: CodexAuth,
    attribution_header: Option<String>,
    env_key_instructions: Option<String>,
    scrub_env_overrides: Vec<String>,
}

impl CodexProviderPatch {
    /// Describe a provider named `provider_id`, shown as `display_name`,
    /// routing to `base_url` with `auth`.
    ///
    /// `base_url` is written verbatim — no normalisation — because the
    /// consumer built the route and the grammar must not second-guess it.
    pub fn new(
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        base_url: impl Into<String>,
        auth: CodexAuth,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            display_name: display_name.into(),
            base_url: base_url.into(),
            auth,
            attribution_header: None,
            env_key_instructions: None,
            scrub_env_overrides: Vec::new(),
        }
    }

    /// Have the provider send `header: <provider_id>` on every request.
    ///
    /// Same contract as
    /// [`CodexRecipe::with_attribution_header`](crate::launch::CodexRecipe::with_attribution_header):
    /// the header name is the consumer's private channel to its own proxy, so
    /// there is no default.
    ///
    /// The grammar owns the attribution entries in the provider's
    /// `http_headers` table: on apply, any entry carrying the provider id as
    /// its value — the only shape this grammar writes — that is not the
    /// current header is removed, so renaming the header retires the old
    /// entry instead of accumulating both. Unset, the same scrub runs (and
    /// drops the container it emptied); user-added headers with any other
    /// value are provably not ours and always survive.
    pub fn with_attribution_header(mut self, header: impl Into<String>) -> Self {
        self.attribution_header = Some(header.into());
        self
    }

    /// Set the hint codex prints when [`CODEX_API_KEY_ENV`] is missing in
    /// [`CodexAuth::ApiKey`] mode.
    ///
    /// The wording names the consumer's product, so it is caller-supplied.
    /// Unset in API-key mode, any existing `env_key_instructions` is removed
    /// — the applied shape is canonical either way, which is what keeps the
    /// textual idempotence probe meaningful. Ignored (removed, like every
    /// API-key knob) in [`CodexAuth::ChatGpt`] mode.
    pub fn with_env_key_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.env_key_instructions = Some(instructions.into());
        self
    }

    /// Also remove `shell_environment_policy.set.<name>` if present.
    ///
    /// This is the consumer's hook for retiring environment overrides *it*
    /// wrote in earlier versions of its own installer; overrides the user
    /// wrote are never touched unless named here. Ported from an installer
    /// that scrubbed one hard-coded executable-path override, with the name
    /// moved to the caller so the grammar stays vendor-neutral.
    pub fn with_scrubbed_env_override(mut self, name: impl Into<String>) -> Self {
        self.scrub_env_overrides.push(name.into());
        self
    }
}

/// Patch `patch`'s provider into `config_text`, returning the new text.
///
/// An empty (or whitespace-only) input is a fresh document; the caller maps
/// "file does not exist" to `""`. The result is canonical for the patch:
/// applying it to its own output changes nothing.
pub fn apply_provider(
    config_text: &str,
    patch: &CodexProviderPatch,
) -> Result<String, CodexConfigError> {
    let provider_id = require_provider_id(&patch.provider_id)?;
    let mut doc = parse(config_text)?;

    doc["model_provider"] = value(provider_id);

    {
        let providers = ensure_table(doc.as_table_mut(), "model_providers", "model_providers")?;
        let provider = ensure_table(
            providers,
            provider_id,
            &format!("model_providers.{provider_id}"),
        )?;
        provider["name"] = value(&patch.display_name);
        provider["base_url"] = value(&patch.base_url);
        provider["wire_api"] = value(WIRE_API);
        scrub_stale_attribution_headers(provider, provider_id, patch.attribution_header.as_deref());
        if let Some(header) = &patch.attribution_header {
            let headers = ensure_table(
                provider,
                "http_headers",
                &format!("model_providers.{provider_id}.http_headers"),
            )?;
            headers[header.as_str()] = value(provider_id);
        }

        match patch.auth {
            CodexAuth::ChatGpt => {
                provider["requires_openai_auth"] = value(true);
                provider.remove("env_key");
                provider.remove("env_key_instructions");
            }
            CodexAuth::ApiKey => {
                provider["env_key"] = value(CODEX_API_KEY_ENV);
                match &patch.env_key_instructions {
                    Some(instructions) => {
                        provider["env_key_instructions"] = value(instructions);
                    }
                    None => {
                        provider.remove("env_key_instructions");
                    }
                }
                provider.remove("requires_openai_auth");
            }
        }
    }

    let features = ensure_table(doc.as_table_mut(), "features", "features")?;
    features["enable_request_compression"] = value(false);

    scrub_env_overrides(&mut doc, &patch.scrub_env_overrides);

    Ok(doc.to_string())
}

/// Remove `provider_id`'s declaration from `config_text`, returning the new
/// text.
///
/// No consumer had an uninstall path to port, so this is defined here —
/// conservatively — rather than inherited. It removes only what is
/// unambiguously the patch's:
///
/// * the `model_providers.<id>` table, and the `model_providers` container
///   itself when that removal leaves it empty;
/// * the root `model_provider` selection, only when it still names this
///   provider — a user who switched providers keeps their selection.
///
/// It deliberately leaves `features.enable_request_compression` and any
/// scrubbing already done: after the fact there is no way to know whether the
/// user also set those, and removal must never delete a setting the user
/// owns. A config the patch never touched is returned unchanged, byte-exact.
pub fn remove_provider(config_text: &str, provider_id: &str) -> Result<String, CodexConfigError> {
    let provider_id = require_provider_id(provider_id)?;
    let mut doc = parse(config_text)?;

    let selected = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .is_some_and(|selected| selected == provider_id);
    let declared = doc
        .get("model_providers")
        .and_then(Item::as_table_like)
        .is_some_and(|providers| providers.contains_key(provider_id));
    if !selected && !declared {
        return Ok(config_text.to_string());
    }

    if selected {
        doc.as_table_mut().remove("model_provider");
    }
    if declared {
        // `as_table_like_mut` on purpose: removal should succeed however the
        // user's container is spelled, even where `apply_provider` (like
        // paper) would refuse to patch an inline one.
        let emptied = doc
            .as_table_mut()
            .get_mut("model_providers")
            .and_then(Item::as_table_like_mut)
            .map(|providers| {
                providers.remove(provider_id);
                providers.is_empty()
            });
        if emptied == Some(true) {
            doc.as_table_mut().remove("model_providers");
        }
    }

    Ok(doc.to_string())
}

/// Is `patch` already fully applied to `config_text`?
///
/// The probe is textual, matching the reconciler this was ported from
/// byte-for-byte: applied means [`apply_provider`] would return the input
/// unchanged. A malformed config is an error, not `false` — an installer must
/// surface that rather than "helpfully" rewriting a file it could not parse.
pub fn is_provider_applied(
    config_text: &str,
    patch: &CodexProviderPatch,
) -> Result<bool, CodexConfigError> {
    Ok(apply_provider(config_text, patch)? == config_text)
}

/// What a `model_providers.<id>` table currently declares, read back for
/// consumers that infer prior install intent from it.
///
/// This is the neutral half of an installer's intent inference: the grammar
/// reads the keys back; deciding what a given `base_url` shape *means* (which
/// auth mode, which backend segment) is the consumer's route knowledge and
/// stays with it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct InstalledCodexProvider {
    /// The provider's `name`, when present and a string.
    pub name: Option<String>,
    /// The provider's `base_url`, when present and a string.
    pub base_url: Option<String>,
    /// The provider's `wire_api`, when present and a string.
    pub wire_api: Option<String>,
    /// The provider's `env_key`, when present and a string.
    pub env_key: Option<String>,
    /// The provider's `requires_openai_auth`, when present and a boolean.
    pub requires_openai_auth: Option<bool>,
}

/// Read back `provider_id`'s declaration from `config_text`, or `None` when
/// no such provider table exists (including when the file is empty).
pub fn installed_provider(
    config_text: &str,
    provider_id: &str,
) -> Result<Option<InstalledCodexProvider>, CodexConfigError> {
    let provider_id = require_provider_id(provider_id)?;
    let doc = parse(config_text)?;
    let Some(provider) = doc
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table_like)
    else {
        return Ok(None);
    };
    let read = |key: &str| provider.get(key).and_then(Item::as_str).map(str::to_string);
    Ok(Some(InstalledCodexProvider {
        name: read("name"),
        base_url: read("base_url"),
        wire_api: read("wire_api"),
        env_key: read("env_key"),
        requires_openai_auth: provider.get("requires_openai_auth").and_then(Item::as_bool),
    }))
}

/// Failure modes of the patch grammar.
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
#[non_exhaustive]
pub enum CodexConfigError {
    /// A blank provider id would select and declare a provider codex cannot
    /// resolve — refused here for the same reason
    /// [`crate::launch::LaunchError::EmptyProviderId`] exists.
    #[snafu(display("codex provider id must not be empty"))]
    EmptyProviderId,

    /// The config text is not valid TOML. Every entry point here surfaces this
    /// rather than rewriting a file it could not parse.
    #[snafu(display("could not parse codex config.toml"))]
    Parse { source: toml_edit::TomlError },

    /// A key the patch must write through is present but not a standard
    /// table (for example `model_providers = 3`, or an inline
    /// `model_providers = {}`). Ported limitation: the installer this came
    /// from refuses these identically rather than restructuring the user's
    /// document.
    #[snafu(display("codex config key `{key}` is not a table"))]
    NotATable { key: String },
}

fn require_provider_id(provider_id: &str) -> Result<&str, CodexConfigError> {
    if provider_id.trim().is_empty() {
        return codex_config_error::EmptyProviderIdSnafu.fail();
    }
    Ok(provider_id)
}

fn parse(config_text: &str) -> Result<Document, CodexConfigError> {
    if config_text.trim().is_empty() {
        return Ok(Document::new());
    }
    config_text
        .parse::<Document>()
        .context(codex_config_error::ParseSnafu)
}

/// Get-or-create `parent[key]` as a standard table, refusing any other
/// existing shape. Ported verbatim from the installer's table helper.
fn ensure_table<'a>(
    parent: &'a mut Table,
    key: &str,
    display_key: &str,
) -> Result<&'a mut Table, CodexConfigError> {
    if !parent.contains_key(key) {
        parent[key] = Item::Table(Table::new());
    }
    parent[key]
        .as_table_mut()
        .ok_or_else(|| CodexConfigError::NotATable {
            key: display_key.to_string(),
        })
}

/// Remove stale attribution entries from the provider's `http_headers`
/// table, so renaming (or dropping) the attribution header retires the old
/// entry instead of leaving codex sending both.
///
/// "Ours" is decided by the only shape this grammar ever writes:
/// `<header> = "<provider_id>"`. An entry whose value is the provider id is
/// indistinguishable from a patch-written one and is treated as managed; an
/// entry with any other value is provably not ours and survives, like every
/// other user byte. The container itself is dropped only when this scrub is
/// what emptied it — a user's own empty `http_headers` table is left alone.
fn scrub_stale_attribution_headers(
    provider: &mut Table,
    provider_id: &str,
    current_header: Option<&str>,
) {
    let Some(headers) = provider
        .get_mut("http_headers")
        .and_then(Item::as_table_like_mut)
    else {
        return;
    };
    let stale: Vec<String> = headers
        .iter()
        .filter(|(name, entry)| {
            entry.as_str() == Some(provider_id) && Some(*name) != current_header
        })
        .map(|(name, _)| name.to_string())
        .collect();
    if stale.is_empty() {
        return;
    }
    for name in &stale {
        headers.remove(name);
    }
    if headers.is_empty() && current_header.is_none() {
        provider.remove("http_headers");
    }
}

/// Remove each named override from `shell_environment_policy.set`, leaving
/// the rest — and both containers, whatever their spelling — alone. Ported
/// from an installer's obsolete-override scrub with the names made
/// caller-supplied.
fn scrub_env_overrides(doc: &mut Document, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let Some(policy) = doc
        .as_table_mut()
        .get_mut("shell_environment_policy")
        .and_then(Item::as_table_like_mut)
    else {
        return;
    };
    let Some(overrides) = policy.get_mut("set").and_then(Item::as_table_like_mut) else {
        return;
    };
    for name in names {
        overrides.remove(name);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A neutral stand-in for what a consumer passes: id, branding, a fully
    /// built route, an attribution header, and the obsolete override its
    /// installer used to write.
    fn chatgpt_patch() -> CodexProviderPatch {
        CodexProviderPatch::new(
            "acme-openai",
            "Acme OpenAI",
            "http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex",
            CodexAuth::ChatGpt,
        )
        .with_attribution_header("X-Acme-Codex-Attribution")
        .with_scrubbed_env_override("ACME_EXECUTABLE")
    }

    fn api_key_patch() -> CodexProviderPatch {
        CodexProviderPatch::new(
            "acme-openai",
            "Acme OpenAI",
            "http://127.0.0.1:51539/v1/openai-responses/openai-transparent/v1",
            CodexAuth::ApiKey,
        )
        .with_attribution_header("X-Acme-Codex-Attribution")
        .with_env_key_instructions("Set OPENAI_API_KEY to an OpenAI API key.")
        .with_scrubbed_env_override("ACME_EXECUTABLE")
    }

    /// Fixture: applying to an empty file (the not-yet-created
    /// `config.toml`) produces exactly this document — the canonical shape
    /// every other fixture converges on.
    ///
    /// The bare `[model_providers]` header is the ported installer's observed
    /// behaviour, pinned deliberately: its table helper inserts an explicit
    /// (non-implicit) table, so a freshly created container renders its own
    /// empty header. Suppressing it (`set_implicit`) would be a fidelity
    /// upgrade this port refuses to smuggle in.
    const APPLIED_TO_EMPTY: &str = r#"model_provider = "acme-openai"

[model_providers]

[model_providers.acme-openai]
name = "Acme OpenAI"
base_url = "http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex"
wire_api = "responses"
requires_openai_auth = true

[model_providers.acme-openai.http_headers]
X-Acme-Codex-Attribution = "acme-openai"

[features]
enable_request_compression = false
"#;

    #[test]
    fn applying_to_an_empty_config_produces_the_canonical_fixture() {
        for empty in ["", "   \n\t\n"] {
            assert_eq!(
                apply_provider(empty, &chatgpt_patch()).unwrap(),
                APPLIED_TO_EMPTY,
            );
        }
    }

    /// Fixture: a fresh file that already carries user content. Every user
    /// byte — comments, blank lines, their model choice, their own tables —
    /// survives; the patch's keys are appended in `toml_edit`'s positions.
    #[test]
    fn applying_preserves_user_content_and_comments() {
        let existing = r#"# Managed by hand — do not lose this comment.
model = "gpt-5-codex"
approval_policy = "on-request" # trailing comment

[tools]
web_search = true
"#;
        let applied = apply_provider(existing, &chatgpt_patch()).unwrap();
        assert_eq!(
            applied,
            r#"# Managed by hand — do not lose this comment.
model = "gpt-5-codex"
approval_policy = "on-request" # trailing comment
model_provider = "acme-openai"

[tools]
web_search = true

[model_providers]

[model_providers.acme-openai]
name = "Acme OpenAI"
base_url = "http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex"
wire_api = "responses"
requires_openai_auth = true

[model_providers.acme-openai.http_headers]
X-Acme-Codex-Attribution = "acme-openai"

[features]
enable_request_compression = false
"#,
        );
    }

    /// Fixture: idempotence. The canonical output re-applies to itself
    /// byte-exactly, and the probe agrees on both sides of the apply.
    #[test]
    fn applying_twice_is_a_byte_level_no_op_and_the_probe_agrees() {
        let patch = chatgpt_patch();
        assert!(!is_provider_applied("", &patch).unwrap());
        let once = apply_provider("", &patch).unwrap();
        assert!(is_provider_applied(&once, &patch).unwrap());
        assert_eq!(apply_provider(&once, &patch).unwrap(), once);
    }

    /// Fixture: apply-then-remove round trip over user content. Removal
    /// takes back the provider table and the selection, and — documented
    /// residue — leaves the `features` flag, because after the fact nothing
    /// can prove the user did not set it themselves.
    #[test]
    fn apply_then_remove_round_trips_user_content_with_documented_residue() {
        let existing = r#"# Keep me.
model = "gpt-5-codex"

[tools]
web_search = true
"#;
        let applied = apply_provider(existing, &chatgpt_patch()).unwrap();
        let removed = remove_provider(&applied, "acme-openai").unwrap();
        assert_eq!(
            removed,
            r#"# Keep me.
model = "gpt-5-codex"

[tools]
web_search = true

[features]
enable_request_compression = false
"#,
        );
        // And removing again is a no-op on the already-removed text.
        assert_eq!(remove_provider(&removed, "acme-openai").unwrap(), removed);
    }

    /// Fixture: the update path. A provider table already present with
    /// different values — including a user comment *inside* it — is updated
    /// in place: same position, comment kept, stale values replaced, and the
    /// stale ChatGPT auth key removed by the API-key patch.
    #[test]
    fn applying_over_a_stale_provider_updates_it_in_place() {
        let existing = r#"model_provider = "acme-openai"

[model_providers.acme-openai]
# The user annotated our table; the comment must survive.
name = "Old Name"
base_url = "http://old.invalid"
wire_api = "responses"
requires_openai_auth = true

[features]
enable_request_compression = false
"#;
        let applied = apply_provider(existing, &api_key_patch()).unwrap();
        assert_eq!(
            applied,
            r#"model_provider = "acme-openai"

[model_providers.acme-openai]
# The user annotated our table; the comment must survive.
name = "Acme OpenAI"
base_url = "http://127.0.0.1:51539/v1/openai-responses/openai-transparent/v1"
wire_api = "responses"
env_key = "OPENAI_API_KEY"
env_key_instructions = "Set OPENAI_API_KEY to an OpenAI API key."

[model_providers.acme-openai.http_headers]
X-Acme-Codex-Attribution = "acme-openai"

[features]
enable_request_compression = false
"#,
        );
    }

    /// Ported from the installer's own auth-mode tests: each auth mode sets
    /// its keys and removes the other mode's, in both directions.
    #[test]
    fn auth_modes_swap_credential_keys_in_both_directions() {
        let chatgpt = apply_provider("", &chatgpt_patch()).unwrap();
        assert!(chatgpt.contains("requires_openai_auth = true"));
        assert!(!chatgpt.contains("env_key"), "{chatgpt}");

        let to_api_key = apply_provider(&chatgpt, &api_key_patch()).unwrap();
        assert!(to_api_key.contains("env_key = \"OPENAI_API_KEY\""));
        assert!(!to_api_key.contains("requires_openai_auth"), "{to_api_key}");

        let back = apply_provider(&to_api_key, &chatgpt_patch()).unwrap();
        assert!(back.contains("requires_openai_auth = true"));
        assert!(!back.contains("env_key"), "{back}");
    }

    /// Unset instructions are removed, not left stale — the applied shape is
    /// canonical, which is what keeps the textual probe meaningful.
    #[test]
    fn unset_env_key_instructions_are_removed_from_a_prior_install() {
        let with_instructions = apply_provider("", &api_key_patch()).unwrap();
        let patch_without = CodexProviderPatch::new(
            "acme-openai",
            "Acme OpenAI",
            "http://127.0.0.1:51539/v1/openai-responses/openai-transparent/v1",
            CodexAuth::ApiKey,
        );
        let applied = apply_provider(&with_instructions, &patch_without).unwrap();
        assert!(!applied.contains("env_key_instructions"), "{applied}");
        assert!(is_provider_applied(&applied, &patch_without).unwrap());
    }

    /// Ported from the installer's obsolete-override test, with the override
    /// name caller-supplied: only the named override goes; the user's stays.
    #[test]
    fn scrubbed_env_overrides_are_removed_and_the_users_are_preserved() {
        let existing = r#"
[shell_environment_policy]
set = { EXISTING_FLAG = "keep-me", ACME_EXECUTABLE = "/obsolete/acme" }
"#;
        let applied = apply_provider(existing, &chatgpt_patch()).unwrap();
        assert!(applied.contains("EXISTING_FLAG = \"keep-me\""));
        assert!(!applied.contains("ACME_EXECUTABLE"), "{applied}");
    }

    /// Fixture: renaming the attribution header and reapplying leaves
    /// exactly one attribution entry — the old one is this grammar's own
    /// shape (value = provider id) and is scrubbed, not accumulated.
    #[test]
    fn renaming_the_attribution_header_retires_the_old_entry() {
        let first = apply_provider("", &chatgpt_patch()).unwrap();
        assert!(first.contains(r#"X-Acme-Codex-Attribution = "acme-openai""#));

        let renamed = CodexProviderPatch::new(
            "acme-openai",
            "Acme OpenAI",
            "http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex",
            CodexAuth::ChatGpt,
        )
        .with_attribution_header("X-Acme-Attribution")
        .with_scrubbed_env_override("ACME_EXECUTABLE");
        let second = apply_provider(&first, &renamed).unwrap();
        assert!(
            !second.contains("X-Acme-Codex-Attribution"),
            "the stale header accumulated:\n{second}"
        );
        assert_eq!(
            second.matches(r#" = "acme-openai""#).count(),
            2, // model_provider selection + the one current attribution entry
            "{second}"
        );
        assert!(second.contains(r#"X-Acme-Attribution = "acme-openai""#));
        assert!(is_provider_applied(&second, &renamed).unwrap());
    }

    /// Fixture: the managed-set boundary, pinned in both directions. A
    /// user-added header in our provider table with its own value is provably
    /// not this grammar's and survives; one whose value is the provider id is
    /// indistinguishable from ours and is scrubbed.
    #[test]
    fn user_headers_survive_unless_they_wear_the_grammars_own_shape() {
        let existing = r#"[model_providers.acme-openai]
name = "Acme OpenAI"
base_url = "http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex"
wire_api = "responses"
requires_openai_auth = true

[model_providers.acme-openai.http_headers]
X-User-Extra = "user-value"
X-Left-Over = "acme-openai"
"#;
        let applied = apply_provider(existing, &chatgpt_patch()).unwrap();
        assert!(
            applied.contains(r#"X-User-Extra = "user-value""#),
            "{applied}"
        );
        assert!(!applied.contains("X-Left-Over"), "{applied}");
        assert!(applied.contains(r#"X-Acme-Codex-Attribution = "acme-openai""#));
        assert!(is_provider_applied(&applied, &chatgpt_patch()).unwrap());
    }

    /// Fixture: a patch with no attribution header retires a previously
    /// installed one — and the container it emptied — while a container
    /// holding a user's own header keeps both the header and itself.
    #[test]
    fn dropping_the_attribution_header_removes_the_managed_entry_and_its_container() {
        let headerless = CodexProviderPatch::new(
            "acme-openai",
            "Acme OpenAI",
            "http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex",
            CodexAuth::ChatGpt,
        );
        let installed = apply_provider("", &chatgpt_patch()).unwrap();
        let applied = apply_provider(&installed, &headerless).unwrap();
        assert!(!applied.contains("http_headers"), "{applied}");
        assert!(is_provider_applied(&applied, &headerless).unwrap());

        // With a user header present, only the managed entry goes.
        let mixed = apply_provider(
            "[model_providers.acme-openai.http_headers]\nX-User-Extra = \"user-value\"\n",
            &chatgpt_patch(),
        )
        .unwrap();
        let applied = apply_provider(&mixed, &headerless).unwrap();
        assert!(applied.contains("[model_providers.acme-openai.http_headers]"));
        assert!(applied.contains(r#"X-User-Extra = "user-value""#));
        assert!(!applied.contains("X-Acme-Codex-Attribution"), "{applied}");
        assert!(is_provider_applied(&applied, &headerless).unwrap());
    }

    /// Fixture: adversarial-but-valid TOML. Literal strings, multi-line
    /// strings, arrays of tables, dotted keys, unicode values, a quoted
    /// exotic key, and a *sibling* provider spelled as an inline value — all
    /// must survive untouched around the patch.
    #[test]
    fn adversarial_valid_toml_survives_untouched() {
        let existing = r#"model = "gpt-5-codex"
notify = ["afplay", 'C:\literal\no-escapes.wav']
"weird key.with dots" = { nested = "inline" }

[projects."/Users/x/dev with spaces"]
trust_level = "trusted"

[[mcp_servers]]
name = "docs"
instructions = """
multi
line
"""

[model_providers]
other = { name = "Someone Else's", base_url = "http://other.example" }

[model_providers.acme-openai]
name = "stale"
base_url = "http://stale.invalid"
wire_api = "responses"
requires_openai_auth = true

[shell_environment_policy.set]
KEEP_UNICODE = "café ☕"
"#;
        let applied = apply_provider(existing, &chatgpt_patch()).unwrap();
        for preserved in [
            r#"notify = ["afplay", 'C:\literal\no-escapes.wav']"#,
            r#""weird key.with dots" = { nested = "inline" }"#,
            r#"[projects."/Users/x/dev with spaces"]"#,
            "[[mcp_servers]]",
            "multi\nline",
            r#"other = { name = "Someone Else's", base_url = "http://other.example" }"#,
            r#"KEEP_UNICODE = "café ☕""#,
        ] {
            assert!(
                applied.contains(preserved),
                "lost {preserved:?} in:\n{applied}"
            );
        }
        assert!(!applied.contains("http://stale.invalid"), "{applied}");
        assert!(is_provider_applied(&applied, &chatgpt_patch()).unwrap());

        // And removal takes back only ours; the sibling inline provider and
        // its container stay.
        let removed = remove_provider(&applied, "acme-openai").unwrap();
        assert!(removed.contains("other = { name"), "{removed}");
        assert!(!removed.contains("acme-openai"), "{removed}");
    }

    /// Removal leaves a selection the user has since pointed elsewhere, and
    /// returns untouched text byte-exact when there is nothing of ours in it.
    #[test]
    fn removal_respects_the_users_own_selection_and_untouched_configs() {
        let switched = r#"model_provider = "their-provider"

[model_providers.acme-openai]
name = "Acme OpenAI"

[model_providers.their-provider]
name = "Theirs"
"#;
        let removed = remove_provider(switched, "acme-openai").unwrap();
        assert!(removed.contains(r#"model_provider = "their-provider""#));
        assert!(removed.contains("[model_providers.their-provider]"));
        assert!(!removed.contains("acme-openai"), "{removed}");

        let untouched = "# nothing of ours\nmodel = \"gpt-5-codex\"\n";
        assert_eq!(
            remove_provider(untouched, "acme-openai").unwrap(),
            untouched
        );
        assert_eq!(remove_provider("", "acme-openai").unwrap(), "");
    }

    /// The read-back probe reports what the table declares and `None` when
    /// it does not exist — a consumer's intent inference keeps its URL-shape
    /// parsing on top of exactly this.
    #[test]
    fn installed_provider_reads_the_table_back() {
        assert_eq!(installed_provider("", "acme-openai").unwrap(), None);

        let applied = apply_provider("", &api_key_patch()).unwrap();
        let installed = installed_provider(&applied, "acme-openai")
            .unwrap()
            .unwrap();
        assert_eq!(installed.name.as_deref(), Some("Acme OpenAI"));
        assert_eq!(
            installed.base_url.as_deref(),
            Some("http://127.0.0.1:51539/v1/openai-responses/openai-transparent/v1"),
        );
        assert_eq!(installed.wire_api.as_deref(), Some(WIRE_API));
        assert_eq!(installed.env_key.as_deref(), Some(CODEX_API_KEY_ENV));
        assert_eq!(installed.requires_openai_auth, None);

        let chatgpt = apply_provider("", &chatgpt_patch()).unwrap();
        let installed = installed_provider(&chatgpt, "acme-openai")
            .unwrap()
            .unwrap();
        assert_eq!(installed.requires_openai_auth, Some(true));
        assert_eq!(installed.env_key, None);
    }

    /// Ported limitation, pinned: a `model_providers` (or provider) key that
    /// is not a standard table is refused, exactly as the installer's table
    /// helper refuses it — including the inline-table spelling.
    #[test]
    fn non_table_shapes_are_refused_not_restructured() {
        for existing in [
            "model_providers = 3\n",
            "model_providers = {}\n",
            "model_providers = { acme-openai = {} }\n",
        ] {
            let err = apply_provider(existing, &chatgpt_patch())
                .expect_err("non-table model_providers must be refused");
            assert!(
                matches!(err, CodexConfigError::NotATable { ref key } if key == "model_providers"),
                "{existing:?} -> {err:?}",
            );
        }
        let err = apply_provider("[model_providers]\nacme-openai = 3\n", &chatgpt_patch())
            .expect_err("a non-table provider entry must be refused");
        assert!(
            matches!(err, CodexConfigError::NotATable { ref key } if key == "model_providers.acme-openai"),
            "{err:?}",
        );
    }

    /// Malformed TOML is an error from every entry point; nothing rewrites a
    /// file it could not parse.
    #[test]
    fn malformed_toml_is_an_error_everywhere() {
        let malformed = "[invalid\npreserve = true\n";
        assert!(matches!(
            apply_provider(malformed, &chatgpt_patch()),
            Err(CodexConfigError::Parse { .. })
        ));
        assert!(matches!(
            remove_provider(malformed, "acme-openai"),
            Err(CodexConfigError::Parse { .. })
        ));
        assert!(matches!(
            is_provider_applied(malformed, &chatgpt_patch()),
            Err(CodexConfigError::Parse { .. })
        ));
        assert!(matches!(
            installed_provider(malformed, "acme-openai"),
            Err(CodexConfigError::Parse { .. })
        ));
    }

    /// A blank provider id is refused everywhere, mirroring the launch
    /// recipe's guard.
    #[test]
    fn a_blank_provider_id_is_refused_everywhere() {
        let blank = CodexProviderPatch::new("  ", "Name", "http://localhost:9", CodexAuth::ChatGpt);
        assert!(matches!(
            apply_provider("", &blank),
            Err(CodexConfigError::EmptyProviderId)
        ));
        assert!(matches!(
            remove_provider("", "  "),
            Err(CodexConfigError::EmptyProviderId)
        ));
        assert!(matches!(
            installed_provider("", ""),
            Err(CodexConfigError::EmptyProviderId)
        ));
    }

    /// The persistent grammar and the launch recipe's `-c` grammar pin the
    /// same wire protocol; a drift here would mean a launched codex and an
    /// installed one speak different request shapes to the same proxy.
    #[test]
    fn wire_api_matches_the_launch_recipes() {
        use crate::launch::{CodexRecipe, LaunchRecipe, ProxyEndpoint};
        let plan = CodexRecipe::new(
            ProxyEndpoint::new("http://localhost:9"),
            CodexAuth::ApiKey,
            "p",
        )
        .plan()
        .unwrap();
        assert!(
            plan.args
                .iter()
                .any(|arg| arg == &format!("model_providers.p.wire_api=\"{WIRE_API}\"")),
            "{:?}",
            plan.args,
        );
    }
}
