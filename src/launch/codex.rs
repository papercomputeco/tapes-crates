//! Codex launch recipe.
//!
//! Codex is the hardest of the three harnesses to capture, because pointing it
//! at a proxy means declaring a whole custom *model provider* rather than
//! overriding one URL. Ported from paper's `cli/start.rs` `launch_args` Codex arm
//! and `resolve_codex_auth`, with the Go `cmd/tapes/start` codex knowledge folded
//! into the documentation below.
//!
//! # Why a custom provider instead of `OPENAI_BASE_URL`
//!
//! Codex honours [`OPENAI_BASE_URL_ENV`] and [`OPENAI_API_BASE_ENV`] for its
//! *built-in* OpenAI provider, and the Go `tapes start` codex arm used exactly
//! that — two env vars, no config. It is the smaller change, and this recipe
//! deliberately does not use it, because the built-in provider gives no way to
//! set three things a capture proxy needs:
//!
//! * `wire_api = "responses"` — pinning the wire protocol, so capture does not
//!   have to guess which of codex's two request shapes it is parsing.
//! * a per-process attribution header — the proxy's only way to tell two
//!   concurrent `codex` processes apart on a shared loopback endpoint.
//! * `features.enable_request_compression = false` — a compressed request body is
//!   opaque to a capture proxy that does not decompress it.
//!
//! Declaring a provider via `-c` gets all three. The env-var form is kept
//! documented here rather than emitted, because it would only affect the
//! built-in provider this recipe overrides — dead config at best, and a
//! confusing second source of truth at worst.
//!
//! # Why we do not touch `~/.codex/auth.json`
//!
//! The Go implementation forced API-key mode by rewriting the user's
//! `~/.codex/auth.json` — injecting `OPENAI_API_KEY` and deleting the `tokens`
//! object so codex could not fall back to an OAuth credential lacking the
//! `api.responses.write` scope — then restoring the file on exit. That works,
//! but it mutates a file the user owns, and a crash between write and restore
//! leaves their credential state altered.
//!
//! This recipe expresses the same intent declaratively instead: `env_key` in
//! [`CodexAuth::ApiKey`] mode, `requires_openai_auth` in
//! [`CodexAuth::ChatGpt`] mode. Codex then picks the credential itself, from its
//! own store, and no file of the user's is ever written.
//! [`codex_auth_file`] is still exported for consumers that need to *read* that
//! state — the hazard is documented there.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use super::{LaunchError, LaunchPlan, LaunchRecipe, ProxyEndpoint, launch_error};

/// The `X-Tapes-Harness-Id` value for Codex traffic, taken from the registry
/// so the recipe and the declaration cannot disagree.
const HARNESS_ID: &str = crate::harness::CODEX.id();

/// The environment variable Codex reads its provider API key from, and the value
/// a custom provider's `env_key` should name.
///
/// Codex's ChatGPT-plan OAuth tokens are only honoured by
/// `chatgpt.com/backend-api/codex`, not by `api.openai.com` — so the two codex
/// credential kinds ride different routes (see [`CodexAuth`]). Either way the
/// credential passes through the capture proxy untouched in the normal
/// `Authorization` header.
pub const CODEX_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// Base-URL override for codex's *built-in* OpenAI provider.
///
/// Documented, not emitted — see the module docs for why this recipe declares a
/// custom provider instead. Retained because it is real harness knowledge that a
/// future consumer (or a debugging session) may need.
pub const OPENAI_BASE_URL_ENV: &str = "OPENAI_BASE_URL";

/// Older alias for [`OPENAI_BASE_URL_ENV`]; the Go `tapes start` codex arm set
/// both. Documented, not emitted.
pub const OPENAI_API_BASE_ENV: &str = "OPENAI_API_BASE";

/// Codex config key that disables request compression.
///
/// A compressed request body is opaque to a capture proxy that does not
/// decompress it, so every capture recipe must turn this off.
const FEATURE_DISABLE_COMPRESSION: &str = "features.enable_request_compression=false";

/// The wire protocol a Paper/Tapes-routed codex provider speaks.
const WIRE_API: &str = "responses";

/// How the launched Codex authenticates to its provider through the capture
/// proxy.
///
/// Both modes are pass-through — the proxy never holds the credential — but they
/// ride different routes because OpenAI honours API keys and ChatGPT-plan OAuth
/// tokens on different hosts. The consumer encodes that choice in the
/// [`ProxyEndpoint`] it supplies; this enum only decides which credential knobs
/// the provider config carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuth {
    /// [`CODEX_API_KEY_ENV`] from the environment, against a transparent
    /// `api.openai.com` route.
    ///
    /// Codex appends `/responses` to the provider's `base_url`, and
    /// `api.openai.com`'s responses path is `/v1/responses` — so an endpoint for
    /// this mode conventionally ends at a `/v1` segment. That is the consumer's
    /// route to build, not this recipe's.
    ApiKey,
    /// Codex's own login (ChatGPT plan), against a
    /// `chatgpt.com/backend-api/codex` route.
    ///
    /// Codex still appends `/responses`, but the ChatGPT backend's path has no
    /// `/v1` component — mirroring codex's native
    /// `https://chatgpt.com/backend-api/codex` default — so an endpoint for this
    /// mode conventionally ends at the backend segment. Codex supplies its OAuth
    /// bearer and its `ChatGPT-Account-Id` header itself.
    ChatGpt,
}

/// Launch Codex against a capture proxy by declaring a custom model provider.
///
/// Build with [`CodexRecipe::new`] and refine with the builder-style setters;
/// only the endpoint, auth mode, and provider id are required.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRecipe {
    endpoint: ProxyEndpoint,
    auth: CodexAuth,
    provider_id: String,
    provider_display_name: Option<String>,
    attribution_header: Option<String>,
    env_key_instructions: Option<String>,
}

impl CodexRecipe {
    /// Declare a codex provider at `endpoint` using `auth`, identified by
    /// `provider_id`.
    ///
    /// `provider_id` should be **unique per launched process**: it is echoed in
    /// the attribution header (see
    /// [`with_attribution_header`](Self::with_attribution_header)), which is how
    /// a capture proxy tells two concurrent `codex` processes apart on one
    /// loopback endpoint. Generating it — paper appends a UUID to a stable
    /// prefix — is left to the consumer so this crate needs no UUID dependency
    /// and no opinion about the prefix.
    pub fn new(endpoint: ProxyEndpoint, auth: CodexAuth, provider_id: impl Into<String>) -> Self {
        Self {
            endpoint,
            auth,
            provider_id: provider_id.into(),
            provider_display_name: None,
            attribution_header: None,
            env_key_instructions: None,
        }
    }

    /// Set the provider's human-readable name, shown in codex's own UI. Pure
    /// branding, so it has no default.
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.provider_display_name = Some(name.into());
        self
    }

    /// Have codex send `header: <provider_id>` on every request to this
    /// provider.
    ///
    /// This is how a capture proxy attributes a request to one specific codex
    /// process. The header *name* is the consumer's — it is that consumer's
    /// private channel to its own proxy, not part of the `X-Tapes-*` envelope —
    /// so there is no default.
    pub fn with_attribution_header(mut self, header: impl Into<String>) -> Self {
        self.attribution_header = Some(header.into());
        self
    }

    /// Set the hint codex prints when [`CODEX_API_KEY_ENV`] is missing in
    /// [`CodexAuth::ApiKey`] mode.
    ///
    /// Ignored in [`CodexAuth::ChatGpt`] mode, where codex supplies its own
    /// credential and never consults `env_key`.
    pub fn with_env_key_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.env_key_instructions = Some(instructions.into());
        self
    }

    /// Dotted-key prefix for this provider's config table.
    fn provider_key(&self) -> String {
        format!("model_providers.{}", self.provider_id)
    }
}

impl LaunchRecipe for CodexRecipe {
    fn harness(&self) -> &str {
        HARNESS_ID
    }

    fn plan(&self) -> Result<LaunchPlan, LaunchError> {
        if self.provider_id.trim().is_empty() {
            return launch_error::EmptyProviderIdSnafu.fail();
        }
        // The id becomes a bare (unquoted) dotted-key segment, so it must be
        // representable as one for the same reason a header name must.
        require_toml_bare_key("codex provider id", &self.provider_id)?;

        let provider_id = &self.provider_id;
        let provider = self.provider_key();
        let mut args = vec![
            "-c".to_string(),
            format!("model_provider={}", toml_quote_value(provider_id)),
        ];
        if let Some(name) = &self.provider_display_name {
            args.push("-c".to_string());
            args.push(format!("{provider}.name={}", toml_quote_value(name)));
        }
        args.push("-c".to_string());
        args.push(format!(
            "{provider}.base_url={}",
            toml_quote_value(&self.endpoint.to_string())
        ));
        args.push("-c".to_string());
        args.push(format!("{provider}.wire_api=\"{WIRE_API}\""));
        if let Some(header) = &self.attribution_header {
            let quoted = toml_quote_key("codex attribution header", header)?;
            args.push("-c".to_string());
            args.push(format!(
                "{provider}.http_headers.{quoted}={}",
                toml_quote_value(provider_id)
            ));
        }
        match self.auth {
            CodexAuth::ApiKey => {
                args.push("-c".to_string());
                args.push(format!("{provider}.env_key=\"{CODEX_API_KEY_ENV}\""));
                if let Some(instructions) = &self.env_key_instructions {
                    args.push("-c".to_string());
                    args.push(format!(
                        "{provider}.env_key_instructions={}",
                        toml_quote_value(instructions)
                    ));
                }
            }
            CodexAuth::ChatGpt => {
                // Codex supplies its ChatGPT OAuth bearer plus the
                // ChatGPT-Account-Id header itself; the capture proxy passes
                // both through untouched.
                args.push("-c".to_string());
                args.push(format!("{provider}.requires_openai_auth=true"));
            }
        }
        args.push("-c".to_string());
        args.push(FEATURE_DISABLE_COMPRESSION.to_string());

        Ok(LaunchPlan {
            args,
            env: Vec::new(),
            config_files: Vec::new(),
        })
    }
}

/// Pick the Codex auth mode from a launch environment.
///
/// An explicit [`CODEX_API_KEY_ENV`] selects API-key pass-through; otherwise
/// route to the ChatGPT-plan endpoint and let Codex supply its own login.
///
/// This deliberately does **not** try to detect whether the user is logged into
/// ChatGPT: Codex is the authority on its own credential state, which can live
/// in `~/.codex/auth.json`, a `CODEX_HOME`-relocated directory, or the OS keyring
/// depending on version and config. Probing one of those locations would falsely
/// block a logged-in user whose credential lives elsewhere. When the user has no
/// login at all, Codex itself prompts for `codex login`.
pub fn resolve_codex_auth(env: &HashMap<OsString, OsString>) -> CodexAuth {
    if env_has_value(env, CODEX_API_KEY_ENV) {
        CodexAuth::ApiKey
    } else {
        CodexAuth::ChatGpt
    }
}

/// True when `key` is present in a launch environment with a non-blank value.
///
/// Codex treats a set-but-empty `env_key` variable as missing, so callers gate on
/// the same condition rather than bare presence. Exposed because a consumer
/// deciding *which* credential to supply needs the same predicate codex uses.
pub fn env_has_value(env: &HashMap<OsString, OsString>, key: &str) -> bool {
    env.get(OsString::from(key).as_os_str())
        .and_then(|value| value.to_str())
        .is_some_and(|value| !value.trim().is_empty())
}

/// Path to codex's credential file, `~/.codex/auth.json`.
///
/// Returns `None` when no home directory can be resolved. This is a **read**
/// helper: launching does not require touching the file, and a capture client
/// should not rewrite it. The Go `tapes start` implementation did rewrite it to
/// force API-key mode (injecting `OPENAI_API_KEY`, deleting the `tokens` object
/// so codex could not fall back to an OAuth credential lacking the
/// `api.responses.write` scope) and restore it afterwards — which mutates a file
/// the user owns and leaves their credential state altered if the process dies
/// mid-flight. [`CodexRecipe`] achieves the same intent declaratively instead;
/// see the module docs.
///
/// Note that this path is not authoritative: codex also honours `CODEX_HOME`, and
/// newer versions may keep the credential in the OS keyring.
pub fn codex_auth_file() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join("auth.json"))
}

/// Quote `key` as a TOML basic string for use as a dotted-key segment.
///
/// Only safe for ASCII input without control characters, because Rust's `Debug`
/// escaping diverges from TOML's string escaping for broader Unicode — so those
/// inputs are rejected rather than mis-encoded.
///
/// Adapted from paper's `toml_key_ascii`, which asserted the same precondition
/// with `debug_assert!`. This crate denies panics, so the invariant becomes a
/// typed error — which is also the better contract: the input is a caller-
/// supplied header name, i.e. data, and data should not be able to trip an
/// assertion.
/// Quote `value` as a TOML basic string. Unlike keys, values may carry
/// arbitrary unicode; only what a basic string cannot hold verbatim is
/// escaped — quotes, backslashes, and control characters — so a display
/// name or instruction containing a quote configures codex instead of
/// restructuring the `-c` override it rides in.
fn toml_quote_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn toml_quote_key(what: &'static str, key: &str) -> Result<String, LaunchError> {
    if !key.is_ascii() || key.chars().any(char::is_control) {
        return launch_error::UnrepresentableTomlKeySnafu {
            what,
            value: key.to_string(),
        }
        .fail();
    }
    Ok(format!("{key:?}"))
}

/// Validate that `key` can appear as a *bare* dotted-key segment.
///
/// Stricter than [`toml_quote_key`]: bare keys cannot contain a quote, a dot, or
/// whitespace, since any of those would restructure the dotted path rather than
/// name a segment within it.
fn require_toml_bare_key(what: &'static str, key: &str) -> Result<(), LaunchError> {
    let bare = key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare {
        return Ok(());
    }
    launch_error::UnrepresentableTomlKeySnafu {
        what,
        value: key.to_string(),
    }
    .fail()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn values_with_quotes_and_backslashes_are_escaped_not_interpolated() {
        let quoted = toml_quote_value(r#"Paper "Prod" C:\paper"#);
        assert_eq!(quoted, r#""Paper \"Prod\" C:\\paper""#);
    }

    #[test]
    fn control_characters_cannot_smuggle_extra_toml() {
        // A newline in a value must become an escape inside the string,
        // never a literal break that terminates the -c override early.
        let quoted = toml_quote_value("line\nbreak\u{7f}");
        assert_eq!(quoted, r#""line\nbreak\u007F""#);
        assert!(!quoted.contains('\n'));
    }

    fn api_key_recipe() -> CodexRecipe {
        CodexRecipe::new(
            ProxyEndpoint::new("http://127.0.0.1:51539/v1/openai-responses/openai-transparent/v1"),
            CodexAuth::ApiKey,
            "paper-openai-test",
        )
        .with_display_name("Paper OpenAI")
        .with_attribution_header("X-Paper-Codex-Attribution")
        .with_env_key_instructions(
            "Set OPENAI_API_KEY to an OpenAI API key; Paper routes Codex through paperd with your own provider credential.",
        )
    }

    /// The full API-key-mode argument vector, in order.
    ///
    /// Carried over from paper's
    /// `launch_args_prefixes_codex_config_and_preserves_passthrough` — same
    /// expected arguments, minus the passthrough tail, which is now the
    /// consumer's to append (see [`LaunchPlan::args`]). Order is asserted
    /// exactly because codex's `-c` pairs are positional on the command line and
    /// paper's own test pins the same sequence.
    #[test]
    fn plan_emits_the_api_key_provider_config_in_order() {
        let plan = api_key_recipe().plan().unwrap();
        assert_eq!(
            plan.args,
            vec![
                "-c".to_string(),
                "model_provider=\"paper-openai-test\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.name=\"Paper OpenAI\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.base_url=\"http://127.0.0.1:51539/v1/openai-responses/openai-transparent/v1\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.wire_api=\"responses\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.http_headers.\"X-Paper-Codex-Attribution\"=\"paper-openai-test\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.env_key=\"OPENAI_API_KEY\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.env_key_instructions=\"Set OPENAI_API_KEY to an OpenAI API key; Paper routes Codex through paperd with your own provider credential.\"".to_string(),
                "-c".to_string(),
                "features.enable_request_compression=false".to_string(),
            ],
        );
        assert!(plan.env.is_empty(), "codex config rides argv, not the env");
        assert!(plan.config_files.is_empty());
    }

    /// ChatGPT mode swaps the two credential knobs for `requires_openai_auth`
    /// and must not mention `env_key` at all.
    ///
    /// Carried over from paper's
    /// `launch_args_chatgpt_mode_uses_codex_route_and_openai_auth`.
    #[test]
    fn plan_emits_the_chatgpt_provider_config_in_order() {
        let plan = CodexRecipe::new(
            ProxyEndpoint::new("http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex"),
            CodexAuth::ChatGpt,
            "paper-openai-test",
        )
        .with_display_name("Paper OpenAI")
        .with_attribution_header("X-Paper-Codex-Attribution")
        // Supplied but irrelevant in ChatGPT mode: codex never consults
        // env_key, so the instructions must not be emitted either.
        .with_env_key_instructions("ignored in chatgpt mode")
        .plan()
        .unwrap();

        assert_eq!(
            plan.args,
            vec![
                "-c".to_string(),
                "model_provider=\"paper-openai-test\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.name=\"Paper OpenAI\"".to_string(),
                "-c".to_string(),
                // No /v1 suffix: codex appends /responses directly in ChatGPT
                // mode, mirroring its native chatgpt.com default.
                "model_providers.paper-openai-test.base_url=\"http://127.0.0.1:51539/v1/openai-chatgpt/chatgpt-codex\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.wire_api=\"responses\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.http_headers.\"X-Paper-Codex-Attribution\"=\"paper-openai-test\"".to_string(),
                "-c".to_string(),
                "model_providers.paper-openai-test.requires_openai_auth=true".to_string(),
                "-c".to_string(),
                "features.enable_request_compression=false".to_string(),
            ],
        );
        assert!(
            !plan.args.iter().any(|arg| arg.contains("env_key")),
            "ChatGPT mode must not set env_key: {:?}",
            plan.args,
        );
    }

    /// The optional knobs are genuinely optional: a recipe with none of them set
    /// still produces valid, complete provider config.
    #[test]
    fn plan_omits_unset_optional_knobs() {
        let plan = CodexRecipe::new(
            ProxyEndpoint::new("http://localhost:9/v1"),
            CodexAuth::ApiKey,
            "tapes-openai",
        )
        .plan()
        .unwrap();
        assert_eq!(
            plan.args,
            vec![
                "-c".to_string(),
                "model_provider=\"tapes-openai\"".to_string(),
                "-c".to_string(),
                "model_providers.tapes-openai.base_url=\"http://localhost:9/v1\"".to_string(),
                "-c".to_string(),
                "model_providers.tapes-openai.wire_api=\"responses\"".to_string(),
                "-c".to_string(),
                "model_providers.tapes-openai.env_key=\"OPENAI_API_KEY\"".to_string(),
                "-c".to_string(),
                "features.enable_request_compression=false".to_string(),
            ],
        );
    }

    /// Compression must be off in every mode — a compressed body is opaque to a
    /// capture proxy.
    #[test]
    fn plan_always_disables_request_compression() {
        for auth in [CodexAuth::ApiKey, CodexAuth::ChatGpt] {
            let plan = CodexRecipe::new(ProxyEndpoint::new("http://localhost:9"), auth, "p")
                .plan()
                .unwrap();
            assert!(
                plan.args
                    .iter()
                    .any(|arg| arg == "features.enable_request_compression=false"),
                "{auth:?} must disable compression: {:?}",
                plan.args,
            );
        }
    }

    /// A blank provider id is refused rather than emitted as `model_provider=""`,
    /// which codex cannot resolve.
    #[test]
    fn plan_rejects_a_blank_provider_id() {
        let err = CodexRecipe::new(
            ProxyEndpoint::new("http://localhost:9"),
            CodexAuth::ApiKey,
            "  ",
        )
        .plan()
        .expect_err("blank provider id must be refused");
        assert!(matches!(err, LaunchError::EmptyProviderId), "{err:?}");
    }

    /// A provider id that is not a bare TOML key would restructure the dotted
    /// path (`model_providers.a.b.base_url` names a different table), so it is
    /// refused.
    #[test]
    fn plan_rejects_a_provider_id_that_is_not_a_bare_toml_key() {
        for bad in ["has.dot", "has space", "has\"quote"] {
            let err = CodexRecipe::new(
                ProxyEndpoint::new("http://localhost:9"),
                CodexAuth::ApiKey,
                bad,
            )
            .plan()
            .expect_err("non-bare provider id must be refused");
            assert!(
                matches!(err, LaunchError::UnrepresentableTomlKey { .. }),
                "{bad:?} -> {err:?}",
            );
        }
    }

    /// An attribution header with bytes TOML quoting cannot represent is refused
    /// at plan time. paper guarded this with a `debug_assert!`, which would have
    /// been a release-build silent mis-encode.
    #[test]
    fn plan_rejects_an_unrepresentable_attribution_header() {
        for bad in ["X-Café-Attribution", "X-Bad\u{7f}Header"] {
            let err = CodexRecipe::new(
                ProxyEndpoint::new("http://localhost:9"),
                CodexAuth::ApiKey,
                "p",
            )
            .with_attribution_header(bad)
            .plan()
            .expect_err("unrepresentable header must be refused");
            assert!(
                matches!(err, LaunchError::UnrepresentableTomlKey { what, .. } if what.contains("header")),
                "{bad:?} -> {err:?}",
            );
        }
    }

    /// `resolve_codex_auth` precedence: an explicit API key wins; absence routes
    /// to ChatGPT; a set-but-blank key counts as absent because that is how
    /// codex reads `env_key`.
    ///
    /// Carried over verbatim from paper's
    /// `resolve_codex_auth_prefers_api_key_then_chatgpt`.
    #[test]
    fn resolve_codex_auth_prefers_api_key_then_chatgpt() {
        let mut env: HashMap<OsString, OsString> = HashMap::new();
        assert_eq!(resolve_codex_auth(&env), CodexAuth::ChatGpt);

        env.insert(OsString::from("OPENAI_API_KEY"), OsString::from("sk-x"));
        assert_eq!(resolve_codex_auth(&env), CodexAuth::ApiKey);

        // A set-but-blank key is treated as absent → ChatGPT.
        env.insert(OsString::from("OPENAI_API_KEY"), OsString::from("   "));
        assert_eq!(resolve_codex_auth(&env), CodexAuth::ChatGpt);
    }

    /// The auth-file helper names codex's conventional location without reading
    /// it. Kept as a path-only helper precisely so no capture client is tempted
    /// to rewrite the user's credential.
    #[test]
    fn codex_auth_file_names_the_conventional_path() {
        if let Some(path) = codex_auth_file() {
            assert!(path.ends_with(".codex/auth.json"), "{path:?}");
        }
    }

    #[test]
    fn harness_id_matches_the_envelope_value() {
        let recipe = CodexRecipe::new(
            ProxyEndpoint::new("http://localhost:9"),
            CodexAuth::ApiKey,
            "p",
        );
        assert_eq!(recipe.harness(), crate::envelope::HARNESS_ID_CODEX);
        assert_eq!(recipe.harness(), "codex");
    }
}
