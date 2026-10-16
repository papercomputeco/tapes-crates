//! Claude Code launch recipe.
//!
//! Claude Code is the simplest harness to capture: it reads one environment
//! variable for its API base URL and appends its own path to it. Ported from a
//! daemon client's Claude launch arm, which agrees byte-for-byte with the Go
//! `tapes start claude` arm in `cmd/tapes/start/start.go` — both set
//! `ANTHROPIC_BASE_URL` and nothing else. Two independent implementations
//! reaching the same one-liner is the strongest evidence available that this is
//! the whole contract.

use super::{LaunchError, LaunchPlan, LaunchRecipe, ProxyEndpoint, StartHint};

/// The environment variable Claude Code reads its API base URL from.
///
/// Claude appends `/v1/messages` (and the other Anthropic API paths) to this
/// value, which is why [`ProxyEndpoint`] trims trailing slashes.
pub const ANTHROPIC_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// The Claude Code flag that appends text to its default system prompt.
///
/// Appends rather than replaces (`--system-prompt` replaces), so a start hint
/// adds to the harness's own instructions instead of discarding them. The
/// flag is placed before the user's passthrough arguments, like every
/// [`LaunchPlan::args`] entry; a user who passes their own
/// `--append-system-prompt` gets Claude Code's own handling of the repeat.
pub const APPEND_SYSTEM_PROMPT_FLAG: &str = "--append-system-prompt";

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
    start_hint: Option<StartHint>,
}

impl ClaudeRecipe {
    /// Point Claude Code at `endpoint`.
    ///
    /// `endpoint` must already carry whatever route prefix the consumer's proxy
    /// expects for Anthropic traffic — this recipe appends nothing.
    pub fn new(endpoint: ProxyEndpoint) -> Self {
        Self {
            endpoint,
            start_hint: None,
        }
    }

    /// Append `hint` to Claude Code's system prompt, through
    /// [`APPEND_SYSTEM_PROMPT_FLAG`].
    ///
    /// Off unless called: a plan without a hint is byte-identical to one from
    /// a build that predates the feature.
    pub fn with_start_hint(mut self, hint: StartHint) -> Self {
        self.start_hint = Some(hint);
        self
    }
}

impl LaunchRecipe for ClaudeRecipe {
    fn harness(&self) -> &str {
        HARNESS_ID
    }

    fn plan(&self) -> Result<LaunchPlan, LaunchError> {
        let args = match &self.start_hint {
            Some(hint) => vec![
                APPEND_SYSTEM_PROMPT_FLAG.to_string(),
                hint.as_str().to_string(),
            ],
            None => Vec::new(),
        };
        Ok(LaunchPlan {
            args,
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

    /// A start hint rides `--append-system-prompt` as one argv pair, ahead
    /// of the user's own arguments, and changes nothing else.
    #[test]
    fn a_start_hint_is_appended_to_the_system_prompt() {
        let hint = StartHint::new("Past sessions:\n`tapesctl search <query>`").unwrap();
        let plan = ClaudeRecipe::new(ProxyEndpoint::new("127.0.0.1:1"))
            .with_start_hint(hint)
            .plan()
            .unwrap();
        assert_eq!(
            plan.args,
            vec![
                "--append-system-prompt".to_string(),
                "Past sessions: `tapesctl search <query>`".to_string(),
            ],
        );
        assert_eq!(plan.env.len(), 1);
        assert!(plan.config_files.is_empty());
    }

    /// Without a hint, no argument is added.
    #[test]
    fn no_start_hint_means_no_arguments() {
        let plan = ClaudeRecipe::new(ProxyEndpoint::new("127.0.0.1:1"))
            .plan()
            .unwrap();
        assert!(plan.args.is_empty());
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
