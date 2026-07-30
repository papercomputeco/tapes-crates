//! Launch recipes.
//!
//! A launch recipe knows how to run one harness so its LLM traffic is directed
//! through a capture proxy endpoint — parameterized by that endpoint, never
//! Paper-specific. Ported from paper's `cli/start.rs` (per-agent env/config)
//! and the Go opencode/codex config injection in tapes' `cmd/tapes/start/`.
//!
//! # Recipes are pure
//!
//! Every recipe is a **pure function of its inputs**: it returns a
//! [`LaunchPlan`] describing the argv prefix, the environment overlay, and any
//! config documents the harness needs — but it never spawns a process, writes a
//! file, creates a temporary directory, or reads the user's home. The consumer
//! owns all of that, and therefore owns cleanup.
//!
//! This matters because the two consumers differ. paperd launches through a
//! `tokio::process::Command` and keeps the parent alive so its tracing
//! subscriber stays attached; the Go `tapes start` shelled out and registered a
//! PID with its own daemon. Neither process model belongs in shared harness
//! knowledge, and a recipe that materialised its own temporary directory would
//! have to invent a cleanup contract for both. Purity also makes the interesting
//! part — the exact bytes a harness needs — testable offline with no filesystem
//! at all.
//!
//! # What a recipe owns, and what the consumer owns
//!
//! The dividing line is *harness* knowledge versus *deployment* knowledge:
//!
//! | Recipe (here) | Consumer |
//! | --- | --- |
//! | which env var carries the base URL | what that URL's path prefix is |
//! | codex's `-c` provider-config grammar | which route each auth mode maps to |
//! | opencode's `opencode.json` provider shape | where to materialise it, and cleanup |
//! | that codex treats a blank `env_key` as absent | which credential to supply |
//! | that provider-display and attribution knobs exist | their branding and header names |
//!
//! So a recipe never constructs a route: the consumer hands it a fully
//! qualified [`ProxyEndpoint`] that already names whatever backend or provider
//! segment its proxy expects. paperd passes
//! `http://127.0.0.1:51539/v1/anthropic/anthropic-transparent`; a standalone
//! client passes whatever its own proxy serves. The recipe only knows that
//! Claude will append `/v1/messages` to it.
//!
//! # What is deliberately *not* here
//!
//! * **pi.** paper's `start pi` arm injects a bundled JavaScript extension via
//!   `--extension` and points it at the gateway with `PAPER_PI_GATEWAY_URL` /
//!   `PAPER_GATEWAY_URL` / `PAPER_PI_ACTIVE_SCHEMA`. Both halves of that
//!   contract are Paper-branded — the extension asset lives in paper's repo and
//!   the env names carry the product's name — so there is no
//!   endpoint-parameterized recipe to extract yet. Generalising it means first
//!   defining a vendor-neutral pi extension contract; that is not this move.
//! * **Credential loading.** Which API key to hand a harness, and where it is
//!   stored, is a consumer concern: paperd passes the user's own credential
//!   through untouched, while the Go CLI read its own credential store. Recipes
//!   accept an already-resolved key and only know *where the harness expects
//!   it*.
//! * **Harness discovery and dispatch.** Resolving a binary on `PATH`, deciding
//!   which names are supported, and mapping a user-typed spelling to a recipe
//!   stay with the consumer for now; a shared registry is tracked separately.

pub mod claude;
pub mod codex;
pub mod opencode;

use std::path::PathBuf;

use snafu::Snafu;

pub use claude::{ANTHROPIC_BASE_URL_ENV, ClaudeRecipe};
pub use codex::{
    CODEX_API_KEY_ENV, CodexAuth, CodexRecipe, codex_auth_file, env_has_value, resolve_codex_auth,
};
pub use opencode::{
    OPENCODE_CONFIG_HOME_ENV, OpenCodeProvider, OpenCodeRecipe, opencode_auth_file,
    opencode_user_config_candidates,
};

/// How to launch a specific harness under a capture proxy.
///
/// Implementors carry their inputs as fields — including the
/// [`ProxyEndpoint`] — so this trait stays uniform across harnesses whose inputs
/// differ wildly. Claude needs one endpoint; codex needs an endpoint plus an
/// auth mode and a provider identity; opencode needs one endpoint *per provider*
/// plus a model selection. Threading all of that through a single method
/// signature would mean either a parameter most harnesses ignore or a different
/// signature per harness. Fields, then a nullary [`Self::plan`].
pub trait LaunchRecipe {
    /// The harness identifier this recipe handles (e.g. `"claude"`, `"codex"`).
    ///
    /// These match the `X-Tapes-Harness-Id` values in [`crate::envelope`], so a
    /// launched session and its captured traffic agree on the harness's name.
    fn harness(&self) -> &str;

    /// Build the launch plan.
    ///
    /// Fallible because some harness config grammars cannot express arbitrary
    /// input: codex's `-c` dotted TOML keys, for instance, cannot carry a header
    /// name containing a quote or a control character. Rejecting those at plan
    /// time gives the consumer a typed error instead of a harness that starts
    /// with silently malformed config.
    fn plan(&self) -> Result<LaunchPlan, LaunchError>;
}

/// Everything a consumer must apply to launch a harness under a capture proxy.
///
/// The three parts are independent, and a recipe may leave any of them empty.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// Arguments to place *before* the user's own passthrough arguments, in this
    /// order. Harness config flags (codex's `-c` pairs, opencode's `--model`) go
    /// here so a user-supplied flag later on the command line still wins
    /// wherever the harness honours last-flag-wins.
    pub args: Vec<String>,
    /// Environment variables to set for the launched process, as
    /// `(name, value)` pairs. These are an overlay: the consumer decides whether
    /// to merge them into the inherited environment or start from a cleared one.
    pub env: Vec<(String, String)>,
    /// Config documents the harness reads from disk rather than from argv or the
    /// environment. The consumer writes each one at its stated path and removes
    /// it when the harness exits.
    pub config_files: Vec<ConfigFile>,
}

/// A config document a harness reads from disk, and where it must live.
///
/// Content only — no mode bits, no ownership. A consumer materialising one of
/// these should keep it private (`0o600`) whenever it carries a credential,
/// which an [`OpenCodeRecipe`] plan does whenever an API key was supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    /// Absolute path to write the document to. Built from a root the consumer
    /// chose, so a recipe never picks a location in the user's home.
    pub path: PathBuf,
    /// The document's full contents.
    pub contents: String,
}

/// A capture-proxy endpoint a harness should send its LLM traffic to.
///
/// Normalises on construction so downstream URL building never produces a
/// double slash: a bare `host:port` gains an `http://` scheme, and trailing
/// slashes are trimmed. Both matter because harnesses append their own path —
/// Claude appends `/v1/messages`, codex appends `/responses` — and
/// `http://host:1//v1/messages` is a 404 in some setups.
///
/// The path is entirely the consumer's: this type carries whatever route prefix
/// the consumer's proxy expects, and no recipe appends path segments to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint(String);

impl ProxyEndpoint {
    /// Normalise `addr` into an endpoint. Accepts a bare `host:port` or a full
    /// URL with either scheme.
    pub fn new(addr: &str) -> Self {
        let trimmed = addr.trim().trim_end_matches('/');
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            Self(trimmed.to_string())
        } else {
            Self(format!("http://{trimmed}"))
        }
    }

    /// The normalised endpoint as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProxyEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Failure modes for [`LaunchRecipe::plan`].
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
#[non_exhaustive]
pub enum LaunchError {
    /// A codex provider id was blank. codex uses the id as a TOML table key and
    /// as the `model_provider` value, so an empty id yields config it cannot
    /// parse.
    #[snafu(display("codex provider id must not be empty"))]
    EmptyProviderId,

    /// A string destined for a codex `-c` dotted TOML key contained bytes the
    /// quoting helper cannot represent — non-ASCII, or an ASCII control
    /// character. Rust's `Debug` escaping diverges from TOML's for those inputs,
    /// so we refuse rather than emit config that means something else.
    #[snafu(display("{what} is not representable as a TOML key: {value:?}"))]
    UnrepresentableTomlKey {
        /// Which input was rejected, for the error message.
        what: &'static str,
        /// The offending value.
        value: String,
    },

    /// An opencode config document could not be serialised to JSON.
    #[snafu(display("could not serialise the opencode config document"))]
    SerializeOpenCodeConfig {
        /// Underlying serialisation failure.
        source: serde_json::Error,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// `ProxyEndpoint` must keep an existing scheme intact and add `http://`
    /// when missing. Stripping a trailing slash matters because the harness
    /// appends `/v1/messages`; `http://x:1//v1/...` is a 404 in some setups.
    ///
    /// Carried over from paper's
    /// `proxy_base_url_handles_schemes_and_trailing_slash`.
    #[test]
    fn proxy_endpoint_handles_schemes_and_trailing_slash() {
        assert_eq!(
            ProxyEndpoint::new("127.0.0.1:51539").as_str(),
            "http://127.0.0.1:51539",
        );
        assert_eq!(
            ProxyEndpoint::new("http://127.0.0.1:51539/").as_str(),
            "http://127.0.0.1:51539",
        );
        assert_eq!(
            ProxyEndpoint::new("https://proxy.internal/").as_str(),
            "https://proxy.internal",
        );
    }

    /// Repeated trailing slashes and surrounding whitespace both normalise away.
    /// paper's helper only ever saw a control-socket value; a standalone client
    /// may read this from a config file or a flag.
    #[test]
    fn proxy_endpoint_normalises_repeated_slashes_and_whitespace() {
        assert_eq!(
            ProxyEndpoint::new("  http://127.0.0.1:51539///  ").as_str(),
            "http://127.0.0.1:51539",
        );
        assert_eq!(
            ProxyEndpoint::new("127.0.0.1:51539/v1/anthropic/x/").as_str(),
            "http://127.0.0.1:51539/v1/anthropic/x",
        );
    }

    #[test]
    fn proxy_endpoint_displays_as_the_normalised_string() {
        assert_eq!(
            ProxyEndpoint::new("127.0.0.1:1").to_string(),
            "http://127.0.0.1:1",
        );
    }
}
