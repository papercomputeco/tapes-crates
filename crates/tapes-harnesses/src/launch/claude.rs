//! Claude Code launch recipe.
//!
//! Claude Code is the simplest harness to capture: it reads one environment
//! variable for its API base URL and appends its own path to it. Ported from a
//! daemon client's Claude launch arm, which agrees byte-for-byte with the Go
//! `tapes start claude` arm in `cmd/tapes/start/start.go` — both set
//! `ANTHROPIC_BASE_URL` and nothing else. Two independent implementations
//! reaching the same one-liner is the strongest evidence available that this is
//! the whole contract.

use super::{LaunchError, LaunchPlan, LaunchRecipe, ProxyEndpoint};

/// The environment variable Claude Code reads its API base URL from.
///
/// Claude appends `/v1/messages` (and the other Anthropic API paths) to this
/// value, which is why [`ProxyEndpoint`] trims trailing slashes.
pub const ANTHROPIC_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// The `X-Tapes-Harness-Id` value for Claude Code traffic, taken from the
/// registry so the recipe and the declaration cannot disagree.
const HARNESS_ID: &str = crate::harness::CLAUDE.id();

/// Launch Claude Code against a capture proxy.
///
/// The harness's own provider authentication is left entirely alone: Claude
/// keeps sending whatever `Authorization` / `X-Api-Key` it already had, and the
/// capture proxy forwards it upstream untouched. Redirecting the base URL is the
/// complete recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeRecipe {
    endpoint: ProxyEndpoint,
}

impl ClaudeRecipe {
    /// Point Claude Code at `endpoint`.
    ///
    /// `endpoint` must already carry whatever route prefix the consumer's proxy
    /// expects for Anthropic traffic — this recipe appends nothing.
    pub fn new(endpoint: ProxyEndpoint) -> Self {
        Self { endpoint }
    }
}

impl LaunchRecipe for ClaudeRecipe {
    fn harness(&self) -> &str {
        HARNESS_ID
    }

    fn plan(&self) -> Result<LaunchPlan, LaunchError> {
        Ok(LaunchPlan {
            args: Vec::new(),
            env: vec![(
                ANTHROPIC_BASE_URL_ENV.to_string(),
                self.endpoint.as_str().to_string(),
            )],
            config_files: Vec::new(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The recipe sets exactly `ANTHROPIC_BASE_URL` to the endpoint verbatim,
    /// adds no arguments, and writes no files.
    ///
    /// Carried over from the ported launch arm's base-URL test, which asserted
    /// the same env var against the same route-qualified URL through a launcher
    /// fixture. Here the assertion is on the plan directly.
    #[test]
    fn plan_sets_anthropic_base_url_and_nothing_else() {
        let recipe = ClaudeRecipe::new(ProxyEndpoint::new(
            "127.0.0.1:51539/v1/anthropic/anthropic-transparent",
        ));
        let plan = recipe.plan().unwrap();

        assert_eq!(
            plan.env,
            vec![(
                "ANTHROPIC_BASE_URL".to_string(),
                "http://127.0.0.1:51539/v1/anthropic/anthropic-transparent".to_string(),
            )],
        );
        assert!(plan.args.is_empty(), "claude takes no config arguments");
        assert!(plan.config_files.is_empty(), "claude needs no config file");
    }

    /// The recipe passes the endpoint through untouched — no route segments
    /// appended. A consumer whose proxy serves Anthropic at the root gets the
    /// root.
    #[test]
    fn plan_appends_no_route_segments() {
        let recipe = ClaudeRecipe::new(ProxyEndpoint::new("http://localhost:8080"));
        let plan = recipe.plan().unwrap();
        assert_eq!(plan.env[0].1, "http://localhost:8080");
    }

    /// The recipe's harness id must match the envelope's, so a launched session
    /// and its captured traffic agree on the harness name.
    #[test]
    fn harness_id_matches_the_envelope_value() {
        let recipe = ClaudeRecipe::new(ProxyEndpoint::new("127.0.0.1:1"));
        assert_eq!(recipe.harness(), tapes_capture::envelope::HARNESS_ID_CLAUDE);
        assert_eq!(recipe.harness(), "claude");
    }
}
