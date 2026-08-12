//! Scripted one-shot recipes: how to run each registry harness for exactly one
//! turn against the mock pair.
//!
//! # What a one-shot recipe adds to a launch recipe
//!
//! [`crate::recipe::OneShotRecipe`] is not a replacement for
//! [`tapes_harnesses::launch::LaunchRecipe`] — it *wraps* one. A launch recipe
//! answers "how is this harness pointed at a capture proxy", which is the
//! question the matrix wants tested rather than re-implemented, so every
//! harness that has one goes through it. What a launch recipe deliberately does
//! not answer is "how do I make this harness do one turn and exit", because
//! that is a test's question, not a consumer's. This module answers it, and
//! nothing else.
//!
//! # Isolation is a correctness property, not tidiness
//!
//! Every plan relocates `HOME` into a sandbox directory, and individual recipes
//! add whatever further variables their harness needs. This is load-bearing:
//! these harnesses persist state on launch, and a matrix cell that ran against a
//! developer's real configuration would both pollute it and read credentials the
//! cell is supposed to be proving are unnecessary.
//!
//! `HOME` is the *primary* mechanism rather than a per-harness config variable,
//! and that choice was forced by a finding rather than picked on taste. A
//! harness's own config-relocation knob and the attribution lane that reads its
//! session files do not always agree on what they honour: Claude Code writes its
//! state under `$CLAUDE_CONFIG_DIR` when that is set, while the lane that reads
//! it resolves `$HOME/.claude/sessions` unconditionally
//! ([`tapes_harnesses::harness::TranscriptSource::ClaudeProjects`], and the
//! sessions-dir resolver beside it). Relocating with `CLAUDE_CONFIG_DIR` therefore
//! isolates the harness *and silently breaks its attribution* — the turn is
//! captured and files under `unknown`. Codex is the other way round: its lane
//! honours `$CODEX_HOME` explicitly. Moving `HOME` is the one relocation both
//! halves follow, so it is the one this module leans on.
//!
//! That asymmetry is worth knowing about beyond this crate: any user who sets
//! `CLAUDE_CONFIG_DIR` loses Claude attribution the same way, and nothing tells
//! them.
//!
//! # A recipe exists even when the harness cannot run
//!
//! Every registry harness has an entry here, including the ones that cannot be
//! launched for a turn at all. `codex-app` is a desktop application a consumer
//! configures rather than starts, so its entry carries an
//! [`OneShotRecipe::unsupported`] reason and the runner reports it as a visible
//! skip. The alternative — leaving it out of the table — is the failure mode
//! this matrix exists to remove: a cell that is absent looks exactly like a cell
//! that passed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use snafu::{ResultExt as _, Snafu};
use tapes_harnesses::harness::{self, Harness};
use tapes_harnesses::launch::{
    ClaudeRecipe, CodexAuth, CodexRecipe, LaunchRecipe, OpenCodeProvider, OpenCodeRecipe,
    ProxyEndpoint,
};

use crate::upstream;

/// The prompt every scripted turn sends.
///
/// Phrased to make a compliant model answer in one short turn without reaching
/// for a tool: a turn that triggers tool use is a turn whose request count stops
/// being one, and the matrix's assertions are about the first model call.
pub const SCRIPTED_PROMPT: &str = "Reply with exactly: ok";

/// The placeholder an argv template substitutes the prompt for.
const PROMPT_SLOT: &str = "{prompt}";

/// The credential handed to a harness that insists on one.
///
/// The mock upstream never checks it. It exists because several harnesses
/// refuse to start without *some* credential and would otherwise fall through
/// to an interactive login — which in a matrix cell reads as an inexplicable
/// hang rather than as a configuration gap.
pub const SCRIPTED_API_KEY: &str = "mock-upstream-not-a-real-key";

/// Which mock upstream surface a harness's turn is expected to land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Anthropic Messages.
    AnthropicMessages,
    /// OpenAI Responses.
    OpenAiResponses,
    /// OpenAI chat-completions.
    OpenAiChatCompletions,
    /// Any of the provider surfaces a capture proxy can front.
    ///
    /// This is not vagueness, it is the strongest claim pi's configuration
    /// supports. pi's capture extension registers *three* providers — Anthropic,
    /// OpenAI, and OpenAI Codex — at one base URL, and each composes its own
    /// path from it. Which one a launch selects depends on the model pi resolves
    /// from its own catalog, which is neither this crate's to pin nor stable
    /// across pi releases. A cell that demanded one specific path would be
    /// asserting a fact about pi's default model rather than about capture, and
    /// would go red on a catalog change that broke nothing.
    AnyCapturedProvider,
}

impl Surface {
    /// The paths on the mock upstream that satisfy this surface.
    ///
    /// A set rather than one path because a capture proxy serving per-provider
    /// routes prefixes the harness's own path, and because pi genuinely may
    /// arrive on any of three. Matching is by suffix, so both shapes work.
    #[must_use]
    pub const fn paths(self) -> &'static [&'static str] {
        const CAPTURED: &[&str] = &[
            upstream::PATH_ANTHROPIC_MESSAGES,
            upstream::PATH_OPENAI_RESPONSES,
            upstream::PATH_PI_CODEX_RESPONSES,
            upstream::PATH_OPENAI_CHAT_COMPLETIONS,
        ];
        match self {
            Self::AnthropicMessages => &[upstream::PATH_ANTHROPIC_MESSAGES],
            Self::OpenAiResponses => &[
                upstream::PATH_OPENAI_RESPONSES,
                upstream::PATH_CODEX_BACKEND_RESPONSES,
            ],
            Self::OpenAiChatCompletions => &[upstream::PATH_OPENAI_CHAT_COMPLETIONS],
            Self::AnyCapturedProvider => CAPTURED,
        }
    }

    /// Does `path` land on this surface?
    #[must_use]
    pub fn accepts(self, path: &str) -> bool {
        self.paths().iter().any(|known| path.ends_with(known))
    }
}

/// How a harness is pointed at the mock upstream.
///
/// A tagged shape rather than a uniform "set this variable", for the same reason
/// the registry holds no [`LaunchRecipe`] instances: the harnesses genuinely do
/// not agree. Claude takes one environment variable, codex takes a provider
/// declared through repeated `-c` flags, opencode takes a config document, and
/// the self-attributing harnesses take the gateway environment contract and an
/// extension. Flattening those into one field would mean a union of every
/// harness's configuration — the shape this repository already decided against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointing {
    /// Through [`ClaudeRecipe`]: one base-URL variable, no path suffix.
    Claude,
    /// Through [`CodexRecipe`] in API-key mode. Codex appends `/responses` to
    /// the provider base URL, so the endpoint carries a `/v1` suffix.
    Codex,
    /// Through [`OpenCodeRecipe`], declaring one captured provider. The
    /// endpoint carries `/v1` because opencode's provider SDKs expect it to.
    OpenCode {
        /// The opencode provider name to route.
        provider: &'static str,
    },
    /// Through the capture-gateway environment contract, read by an extension
    /// running inside the harness.
    GatewayEnv,
    /// Not pointable — the harness has no launch story at all.
    None,
}

/// A non-zero exit that a harness's one-shot mode produces legitimately.
///
/// This exists so that "the harness finished successfully" can be a real
/// requirement of a matrix cell. A cell asserts two things — the expected
/// interaction happened, *and* the harness completed — and dropping the second
/// is how a harness that sends its request and then dies goes on reporting
/// green. Some harnesses do exit non-zero from a headless mode for reasons that
/// are not failures; that is a fact about one harness, so it is stated on that
/// harness's recipe with a reason a reviewer can check, rather than being a
/// blanket tolerance every cell inherits.
///
/// The code is exact. Tolerating "any non-zero" would readmit the whole class:
/// a harness that started segfaulting would land on the same tolerance as the
/// documented exit, and nothing would go red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToleratedExit {
    /// The one exit code that is not a failure for this harness.
    pub code: i32,
    /// Why this harness exits that way, in words a reader can check against the
    /// harness's own documentation or behaviour.
    pub reason: &'static str,
}

/// How to run one registry harness for a single scripted turn.
#[derive(Debug, Clone)]
pub struct OneShotRecipe {
    /// The registry harness this recipe launches.
    pub harness_id: &'static str,
    /// The program name, resolved on `PATH`.
    pub binary: &'static str,
    /// Arguments that make the binary print its version and exit — the version
    /// manifest's input.
    pub version_args: &'static [&'static str],
    /// The argv that runs one non-interactive turn. Exactly one element must be
    /// [`PROMPT_SLOT`], which is replaced by the prompt.
    pub argv: &'static [&'static str],
    /// The upstream surface the turn lands on.
    pub surface: Surface,
    /// How the harness is pointed at the upstream.
    pub pointing: Pointing,
    /// Environment variables that relocate the harness's configuration into the
    /// sandbox, as `(variable, sandbox-relative directory)`.
    ///
    /// These are *in addition to* the `HOME` relocation every plan applies. A
    /// recipe adds one only when its harness needs a location `HOME` does not
    /// already move — and must not add one the attribution lane will not follow.
    pub sandbox_env: &'static [(&'static str, &'static str)],
    /// Additional fixed environment, chiefly credentials the harness insists on.
    pub extra_env: &'static [(&'static str, &'static str)],
    /// Why this harness cannot be launched for a turn, when it cannot. `Some`
    /// makes the cell a visible skip rather than a silent absence.
    pub unsupported: Option<&'static str>,
    /// The one non-zero exit this harness's one-shot mode is allowed to finish
    /// with, when it has one. `None` — the ordinary case, and every recipe here
    /// today — means the cell requires a clean exit.
    pub tolerated_exit: Option<ToleratedExit>,
}

/// Claude Code: one environment variable, one headless flag.
pub const CLAUDE_ONE_SHOT: OneShotRecipe = OneShotRecipe {
    harness_id: harness::CLAUDE.id(),
    binary: "claude",
    version_args: &["--version"],
    argv: &["-p", PROMPT_SLOT],
    surface: Surface::AnthropicMessages,
    pointing: Pointing::Claude,
    // Deliberately empty, and deliberately *not* `CLAUDE_CONFIG_DIR`. Claude
    // honours that variable; the lane that reads Claude's session files does
    // not, so setting it isolates the harness and silently un-attributes every
    // turn it produces. The `HOME` relocation every plan applies moves both
    // halves together. See the module docs.
    sandbox_env: &[],
    // Both spellings: which one Claude honours depends on whether it considers
    // itself on a first-party or a proxied endpoint, and a cell that guessed
    // wrong would fall through to an interactive login.
    extra_env: &[
        ("ANTHROPIC_API_KEY", SCRIPTED_API_KEY),
        ("ANTHROPIC_AUTH_TOKEN", SCRIPTED_API_KEY),
        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ],
    unsupported: None,
    tolerated_exit: None,
};

/// Codex CLI: a custom provider declared through `-c`, plus `exec`.
pub const CODEX_ONE_SHOT: OneShotRecipe = OneShotRecipe {
    harness_id: harness::CODEX.id(),
    binary: "codex",
    version_args: &["--version"],
    // `--skip-git-repo-check` so the sandbox directory need not be a repository,
    // and a sandbox mode that cannot reach the network or the filesystem: the
    // turn under test is the model call, not whatever the model asks for.
    argv: &[
        "exec",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        PROMPT_SLOT,
    ],
    surface: Surface::OpenAiResponses,
    pointing: Pointing::Codex,
    sandbox_env: &[("CODEX_HOME", "codex")],
    extra_env: &[("OPENAI_API_KEY", SCRIPTED_API_KEY)],
    unsupported: None,
    tolerated_exit: None,
};

/// The Codex desktop app: configurable, not launchable.
pub const CODEX_APP_ONE_SHOT: OneShotRecipe = OneShotRecipe {
    harness_id: harness::CODEX_APP.id(),
    binary: "codex-app",
    version_args: &[],
    argv: &[],
    surface: Surface::OpenAiResponses,
    pointing: Pointing::None,
    sandbox_env: &[],
    extra_env: &[],
    unsupported: Some(
        "the Codex desktop app is a long-lived host a consumer configures rather than starts; \
         it has no one-shot invocation and no headless mode, so a Tier-1 cell cannot drive it. \
         Its capture path is covered by the lifecycle-hook attribution tests instead.",
    ),
    tolerated_exit: None,
};

/// opencode: a planned config document, plus `run`.
pub const OPENCODE_ONE_SHOT: OneShotRecipe = OneShotRecipe {
    harness_id: harness::OPENCODE.id(),
    binary: "opencode",
    version_args: &["--version"],
    argv: &["run", PROMPT_SLOT],
    surface: Surface::AnthropicMessages,
    pointing: Pointing::OpenCode {
        provider: "anthropic",
    },
    // The recipe itself sets XDG_CONFIG_HOME; these cover the state opencode
    // keeps outside its config root, which the recipe has no reason to know
    // about but a test does.
    sandbox_env: &[
        ("XDG_DATA_HOME", "opencode-data"),
        ("XDG_CACHE_HOME", "opencode-cache"),
        ("XDG_STATE_HOME", "opencode-state"),
    ],
    extra_env: &[("ANTHROPIC_API_KEY", SCRIPTED_API_KEY)],
    unsupported: None,
    tolerated_exit: None,
};

/// pi: the gateway environment contract, plus an explicitly loaded extension.
///
/// pi is [`tapes_harnesses::harness::LaunchSupport::ConsumerOwned`], so there is
/// no launch recipe to wrap. What there *is* is the extension this crate ships
/// and the environment contract it reads, which together are the whole capture
/// story — so the recipe writes the extension into the sandbox and loads it with
/// `--extension`, which is exactly the argv a consumer's recipe would have to
/// plan if one existed.
pub const PI_ONE_SHOT: OneShotRecipe = OneShotRecipe {
    harness_id: harness::PI.id(),
    binary: "pi",
    version_args: &["--version"],
    // No `--provider`: pi resolves a model from its own catalog, and naming a
    // provider whose catalog entry has moved fails the launch outright. The
    // extension redirects every captured provider to the same base URL, so
    // whichever one pi picks still lands on the mock — which is why the surface
    // below is the whole captured set.
    argv: &[
        "--print",
        "--mode",
        "text",
        "--no-session",
        "--no-tools",
        PROMPT_SLOT,
    ],
    surface: Surface::AnyCapturedProvider,
    pointing: Pointing::GatewayEnv,
    sandbox_env: &[
        ("XDG_CONFIG_HOME", "pi-config"),
        ("XDG_DATA_HOME", "pi-data"),
    ],
    extra_env: &[("ANTHROPIC_API_KEY", SCRIPTED_API_KEY)],
    unsupported: None,
    tolerated_exit: None,
};

/// Every one-shot recipe, one per registry harness.
pub const RECIPES: &[OneShotRecipe] = &[
    CLAUDE_ONE_SHOT,
    CODEX_ONE_SHOT,
    CODEX_APP_ONE_SHOT,
    OPENCODE_ONE_SHOT,
    PI_ONE_SHOT,
];

/// The one-shot recipe for `harness`.
///
/// Returns `None` only if the registry grew a harness this table has not been
/// taught — which an invariant test in this module turns into a build failure
/// rather than a quietly missing matrix row.
#[must_use]
pub fn for_harness(harness: &Harness) -> Option<&'static OneShotRecipe> {
    RECIPES
        .iter()
        .find(|recipe| recipe.harness_id == harness.id())
}

/// Everything needed to turn a recipe into a runnable command.
#[derive(Debug, Clone)]
pub struct OneShotContext {
    /// The base URL the harness should send model traffic to. This is the mock
    /// upstream directly for a harness-vs-mock cell, or a capture proxy's
    /// address for a CLI-composition cell — the recipe does not care which,
    /// which is what makes the same recipe serve both column types.
    pub endpoint: String,
    /// A private directory the recipe may relocate configuration into and write
    /// config documents beneath. The caller owns its lifetime.
    pub sandbox: PathBuf,
    /// The per-launch capture nonce, when the caller is acting as a capture
    /// client. `None` omits the variable, which a plugin must treat as "the
    /// launching client predates the nonce contract".
    pub nonce: Option<String>,
}

/// A runnable one-shot launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneShotPlan {
    /// The program to run.
    pub program: String,
    /// Its arguments, prompt already substituted.
    pub args: Vec<String>,
    /// The environment overlay to apply.
    pub env: BTreeMap<String, String>,
    /// The working directory to run in.
    pub cwd: PathBuf,
}

/// Failure modes for [`OneShotRecipe::plan`].
#[derive(Debug, Snafu)]
#[snafu(module, visibility(pub(crate)))]
#[non_exhaustive]
pub enum RecipeError {
    /// The harness has no one-shot launch at all.
    #[snafu(display("{harness_id} cannot be launched for one turn: {reason}"))]
    Unsupported {
        /// The harness that cannot be launched.
        harness_id: &'static str,
        /// Why not, verbatim from the recipe.
        reason: &'static str,
    },

    /// The wrapped launch recipe refused to plan.
    #[snafu(display("{harness_id}: the launch recipe could not plan"))]
    Launch {
        /// The harness whose recipe refused.
        harness_id: &'static str,
        /// The underlying refusal.
        source: tapes_harnesses::launch::LaunchError,
    },

    /// A directory or config document could not be written into the sandbox.
    #[snafu(display("{harness_id}: could not materialise {}", path.display()))]
    Materialise {
        /// The harness being prepared.
        harness_id: &'static str,
        /// The path that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
}

impl OneShotRecipe {
    /// Build a runnable plan, materialising every config document the harness
    /// needs beneath `ctx.sandbox`.
    ///
    /// This writes to disk, unlike the pure [`LaunchRecipe::plan`] it wraps.
    /// That difference is deliberate and is exactly the consumer responsibility
    /// the launch recipes decline to take: somebody has to own the temporary
    /// tree and its cleanup, and for the matrix that somebody is the test.
    ///
    /// # Errors
    ///
    /// [`RecipeError::Unsupported`] when the harness has no one-shot launch,
    /// [`RecipeError::Launch`] when the wrapped recipe refuses to plan, and
    /// [`RecipeError::Materialise`] when the sandbox cannot be written.
    pub fn plan(&self, ctx: &OneShotContext) -> Result<OneShotPlan, RecipeError> {
        if let Some(reason) = self.unsupported {
            return recipe_error::UnsupportedSnafu {
                harness_id: self.harness_id,
                reason,
            }
            .fail();
        }

        let mut env: BTreeMap<String, String> = BTreeMap::new();
        let mut args: Vec<String> = Vec::new();

        // The universal relocation, applied before anything a recipe adds. Both
        // the harness and the attribution lane that reads its session files
        // follow `HOME`, which is the property no per-harness config variable
        // reliably has.
        let home = ctx.sandbox.join("home");
        self.create_dir(&home)?;
        env.insert("HOME".to_owned(), home.display().to_string());

        // Sandbox relocations next, so a `pointing` arm that sets the same
        // variable (opencode's XDG_CONFIG_HOME) wins over the generic one.
        for (variable, subdir) in self.sandbox_env {
            let path = ctx.sandbox.join(subdir);
            self.create_dir(&path)?;
            env.insert((*variable).to_owned(), path.display().to_string());
        }
        for (variable, value) in self.extra_env {
            env.insert((*variable).to_owned(), (*value).to_owned());
        }

        match self.pointing {
            Pointing::Claude => {
                let plan = ClaudeRecipe::new(ProxyEndpoint::new(&ctx.endpoint))
                    .plan()
                    .context(recipe_error::LaunchSnafu {
                        harness_id: self.harness_id,
                    })?;
                self.absorb(&plan, &mut args, &mut env)?;
            }
            Pointing::Codex => {
                // Codex appends `/responses`, so the provider base URL ends at
                // the `/v1` segment the OpenAI Responses surface lives under.
                let endpoint =
                    ProxyEndpoint::new(&format!("{}/v1", ctx.endpoint.trim_end_matches('/')));
                let plan = CodexRecipe::new(endpoint, CodexAuth::ApiKey, "tapes-matrix")
                    .with_display_name("tapes matrix mock")
                    .plan()
                    .context(recipe_error::LaunchSnafu {
                        harness_id: self.harness_id,
                    })?;
                self.absorb(&plan, &mut args, &mut env)?;
            }
            Pointing::OpenCode { provider } => {
                let config_root = ctx.sandbox.join("opencode-config");
                self.create_dir(&config_root)?;
                let endpoint =
                    ProxyEndpoint::new(&format!("{}/v1", ctx.endpoint.trim_end_matches('/')));
                let plan = OpenCodeRecipe::new(
                    &config_root,
                    vec![OpenCodeProvider::new(provider, endpoint).with_api_key(SCRIPTED_API_KEY)],
                )
                .plan()
                .context(recipe_error::LaunchSnafu {
                    harness_id: self.harness_id,
                })?;
                self.absorb(&plan, &mut args, &mut env)?;
            }
            Pointing::GatewayEnv => {
                // The self-attributing path: the extension does the pointing,
                // and this is the contract it reads.
                env.insert(
                    tapes_capture::gateway::GATEWAY_URL_ENV.to_owned(),
                    ctx.endpoint.clone(),
                );
                env.insert(
                    tapes_capture::gateway::GATEWAY_SCHEMA_ENV.to_owned(),
                    match self.surface {
                        Surface::AnthropicMessages => "anthropic".to_owned(),
                        Surface::OpenAiResponses | Surface::OpenAiChatCompletions => {
                            "openai".to_owned()
                        }
                        // The schema variable is a display hint a plugin may
                        // warn on but must not gate the redirect with, so a
                        // harness whose provider is not known until it resolves
                        // a model gets the proxy's default rather than a claim
                        // this recipe cannot make.
                        Surface::AnyCapturedProvider => "anthropic".to_owned(),
                    },
                );

                // pi loads an extension from a path, so the artifact this crate
                // ships is written into the sandbox and named on the argv.
                args.extend(self.stage_plugin_artifacts(&ctx.sandbox)?);
            }
            Pointing::None => {}
        }

        if let Some(nonce) = &ctx.nonce {
            env.insert(
                tapes_capture::gateway::GATEWAY_NONCE_ENV.to_owned(),
                nonce.clone(),
            );
        }

        // The recipe's own argv comes after any config flags the launch plan
        // contributed, and the user-facing prompt is last — the same
        // last-flag-wins ordering `LaunchPlan::args` documents.
        for token in self.argv {
            args.push(if *token == PROMPT_SLOT {
                SCRIPTED_PROMPT.to_owned()
            } else {
                (*token).to_owned()
            });
        }

        let cwd = ctx.sandbox.join("cwd");
        self.create_dir(&cwd)?;

        Ok(OneShotPlan {
            program: self.binary.to_owned(),
            args,
            env,
            cwd,
        })
    }

    /// Write this harness's capture artifacts into the sandbox, and return the
    /// argv that loads each one.
    ///
    /// Empty for a harness that needs no artifact, which is the ordinary case
    /// and not an error: it says capture needs no cooperation from inside the
    /// harness.
    fn stage_plugin_artifacts(&self, sandbox: &Path) -> Result<Vec<String>, RecipeError> {
        let Some(entry) = harness::find(self.harness_id) else {
            return Ok(Vec::new());
        };
        let home = sandbox.join("plugin-home");
        let mut args = Vec::new();
        for artifact in entry.plugin_artifacts() {
            let path = artifact.install_path(&home);
            if let Some(parent) = path.parent() {
                self.create_dir(parent)?;
            }
            std::fs::write(&path, artifact.contents()).context(recipe_error::MaterialiseSnafu {
                harness_id: self.harness_id,
                path: path.clone(),
            })?;
            args.push("--extension".to_owned());
            args.push(path.display().to_string());
        }
        Ok(args)
    }

    /// Fold a launch plan's argv, environment, and config documents in.
    fn absorb(
        &self,
        plan: &tapes_harnesses::launch::LaunchPlan,
        args: &mut Vec<String>,
        env: &mut BTreeMap<String, String>,
    ) -> Result<(), RecipeError> {
        args.extend(plan.args.iter().cloned());
        for (name, value) in &plan.env {
            env.insert(name.clone(), value.clone());
        }
        for file in &plan.config_files {
            if let Some(parent) = file.path.parent() {
                self.create_dir(parent)?;
            }
            std::fs::write(&file.path, &file.contents).context(recipe_error::MaterialiseSnafu {
                harness_id: self.harness_id,
                path: file.path.clone(),
            })?;
        }
        Ok(())
    }

    /// Create a directory and every missing parent.
    fn create_dir(&self, path: &Path) -> Result<(), RecipeError> {
        std::fs::create_dir_all(path).context(recipe_error::MaterialiseSnafu {
            harness_id: self.harness_id,
            path: path.to_path_buf(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn context(sandbox: &Path) -> OneShotContext {
        OneShotContext {
            endpoint: "http://127.0.0.1:9".to_owned(),
            sandbox: sandbox.to_path_buf(),
            nonce: Some("matrix-nonce".to_owned()),
        }
    }

    /// Every registry harness has a recipe. This is the invariant that keeps a
    /// harness added to the registry from silently acquiring no matrix row: the
    /// build fails here rather than the matrix quietly covering one fewer
    /// harness than it claims to.
    #[test]
    fn every_registry_harness_has_a_one_shot_recipe() {
        for harness in harness::REGISTRY {
            assert!(
                for_harness(harness).is_some(),
                "{} is in the registry with no one-shot recipe — add one to RECIPES",
                harness.id(),
            );
        }
        assert_eq!(RECIPES.len(), harness::REGISTRY.len());
    }

    /// And no recipe names a harness the registry does not have.
    #[test]
    fn no_recipe_names_an_unregistered_harness() {
        for recipe in RECIPES {
            assert!(
                harness::find(recipe.harness_id).is_some(),
                "{} has a recipe but is not in the registry",
                recipe.harness_id,
            );
        }
    }

    /// Every launchable recipe has exactly one prompt slot, so a turn is one
    /// turn. A template with none would run the harness interactively and hang.
    #[test]
    fn every_launchable_recipe_has_exactly_one_prompt_slot() {
        for recipe in RECIPES.iter().filter(|r| r.unsupported.is_none()) {
            let slots = recipe.argv.iter().filter(|a| **a == PROMPT_SLOT).count();
            assert_eq!(
                slots, 1,
                "{} needs exactly one prompt slot",
                recipe.harness_id
            );
        }
    }

    /// Every launchable recipe relocates `HOME` into the sandbox. A cell
    /// without this would run against the developer's real configuration —
    /// polluting it, and reading credentials the cell is meant to prove
    /// unnecessary.
    #[test]
    fn every_launchable_plan_relocates_home_into_the_sandbox() {
        for recipe in RECIPES.iter().filter(|r| r.unsupported.is_none()) {
            let sandbox = tempfile::tempdir().unwrap();
            let plan = recipe.plan(&context(sandbox.path())).unwrap();
            let home = plan
                .env
                .get("HOME")
                .unwrap_or_else(|| panic!("{} must relocate HOME", recipe.harness_id));
            assert!(
                home.starts_with(&sandbox.path().display().to_string()),
                "{}: HOME must point inside the sandbox, got {home}",
                recipe.harness_id,
            );
            assert!(
                Path::new(home).is_dir(),
                "{}: HOME must exist",
                recipe.harness_id
            );
        }
    }

    /// No recipe relocates Claude's config directory. It is the one variable
    /// that isolates the harness while breaking the lane that reads its
    /// sessions, so a cell that set it would capture turns and attribute none —
    /// green plumbing over a broken result.
    #[test]
    fn no_recipe_sets_the_claude_config_dir() {
        for recipe in RECIPES {
            assert!(
                !recipe
                    .sandbox_env
                    .iter()
                    .any(|(variable, _)| *variable == "CLAUDE_CONFIG_DIR"),
                "{} must not relocate CLAUDE_CONFIG_DIR — the attribution lane does not follow it",
                recipe.harness_id,
            );
        }
    }

    /// A tolerated exit is a stated exception, not a loophole. A `code` of zero
    /// would be a tolerance for success — meaningless — and an empty reason
    /// would be a tolerance nobody can review, which is exactly the blanket
    /// "any exit is fine" this field replaced.
    #[test]
    fn a_tolerated_exit_names_a_real_code_and_says_why() {
        for recipe in RECIPES {
            let Some(tolerated) = recipe.tolerated_exit else {
                continue;
            };
            assert_ne!(
                tolerated.code, 0,
                "{}: a tolerated exit of 0 tolerates success",
                recipe.harness_id,
            );
            assert!(
                !tolerated.reason.trim().is_empty(),
                "{}: a tolerated exit must say why",
                recipe.harness_id,
            );
        }
    }

    /// A recipe for an unlaunchable harness refuses with its stated reason
    /// rather than producing a plan nothing can run.
    #[test]
    fn an_unsupported_harness_refuses_with_its_reason() {
        let sandbox = tempfile::tempdir().unwrap();
        let error = CODEX_APP_ONE_SHOT
            .plan(&context(sandbox.path()))
            .expect_err("codex-app has no one-shot launch");
        assert!(error.to_string().contains("codex-app"));
        assert!(error.to_string().contains("configures rather than starts"));
    }

    /// Claude's plan carries the base-URL variable from the shared launch
    /// recipe, not a copy spelled here.
    #[test]
    fn the_claude_plan_points_through_the_shared_launch_recipe() {
        let sandbox = tempfile::tempdir().unwrap();
        let plan = CLAUDE_ONE_SHOT.plan(&context(sandbox.path())).unwrap();
        assert_eq!(
            plan.env
                .get(tapes_harnesses::launch::ANTHROPIC_BASE_URL_ENV)
                .map(String::as_str),
            Some("http://127.0.0.1:9"),
        );
        assert_eq!(plan.args.last().map(String::as_str), Some(SCRIPTED_PROMPT));
        // Isolation rides on HOME, which both Claude and the lane that reads
        // its session files follow.
        let home = plan.env.get("HOME").unwrap();
        assert!(home.starts_with(&sandbox.path().display().to_string()));
    }

    /// Codex's provider base URL ends at `/v1`, because codex appends
    /// `/responses` and the mock serves `/v1/responses`.
    #[test]
    fn the_codex_plan_ends_its_provider_endpoint_at_v1() {
        let sandbox = tempfile::tempdir().unwrap();
        let plan = CODEX_ONE_SHOT.plan(&context(sandbox.path())).unwrap();
        assert!(
            plan.args
                .iter()
                .any(|arg| arg.contains("base_url=\"http://127.0.0.1:9/v1\"")),
            "codex args: {:?}",
            plan.args,
        );
        assert!(plan.env.contains_key("CODEX_HOME"));
    }

    /// opencode's plan writes a config document and the capture plugin beside
    /// it, both inside the sandbox.
    #[test]
    fn the_opencode_plan_materialises_its_config_and_plugin() {
        let sandbox = tempfile::tempdir().unwrap();
        let plan = OPENCODE_ONE_SHOT.plan(&context(sandbox.path())).unwrap();
        let config_root = sandbox.path().join("opencode-config");
        let config = config_root.join("opencode").join("opencode.json");
        assert!(config.is_file(), "opencode config was not written");

        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            document["provider"]["anthropic"]["options"]["baseURL"],
            "http://127.0.0.1:9/v1",
        );
        assert_eq!(
            plan.env.get("XDG_CONFIG_HOME").map(String::as_str),
            Some(config_root.display().to_string().as_str()),
        );
    }

    /// pi's plan sets the gateway contract and loads the shipped extension from
    /// the sandbox — the whole of its capture story, since it has no recipe.
    #[test]
    fn the_pi_plan_sets_the_gateway_contract_and_loads_the_extension() {
        let sandbox = tempfile::tempdir().unwrap();
        let plan = PI_ONE_SHOT.plan(&context(sandbox.path())).unwrap();
        assert_eq!(
            plan.env
                .get(tapes_capture::gateway::GATEWAY_URL_ENV)
                .map(String::as_str),
            Some("http://127.0.0.1:9"),
        );
        assert_eq!(
            plan.env
                .get(tapes_capture::gateway::GATEWAY_NONCE_ENV)
                .map(String::as_str),
            Some("matrix-nonce"),
        );

        let extension = plan
            .args
            .iter()
            .position(|arg| arg == "--extension")
            .map(|index| &plan.args[index + 1])
            .expect("pi loads an extension");
        assert!(
            Path::new(extension).is_file(),
            "the extension was not written to {extension}",
        );
    }

    /// Omitting the nonce omits the variable rather than setting it empty: an
    /// installed plugin reads an unset variable as "the client predates the
    /// contract" and an empty one as a nonce that will never match.
    #[test]
    fn no_nonce_means_no_nonce_variable() {
        let sandbox = tempfile::tempdir().unwrap();
        let mut ctx = context(sandbox.path());
        ctx.nonce = None;
        let plan = PI_ONE_SHOT.plan(&ctx).unwrap();
        assert!(
            !plan
                .env
                .contains_key(tapes_capture::gateway::GATEWAY_NONCE_ENV),
        );
    }
}
