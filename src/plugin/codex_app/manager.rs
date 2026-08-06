//! Codex's own plugin manager: the source layout it consumes and the `codex`
//! subcommands that register and install from it.
//!
//! [`super`] renders the two manifests a hook plugin *is*. Neither is
//! installable on its own: Codex installs a plugin from a **marketplace**, a
//! directory whose `.agents/plugins/marketplace.json` offers one or more
//! plugins as local sources, and it installs *by running its own CLI*. So a
//! consumer that only had the templates could write a tree and then had to
//! tell the user to finish the job by hand.
//!
//! Everything needed to finish it lives here, and it is all Codex knowledge
//! rather than any consumer's:
//!
//! * **The wrapper.** [`MARKETPLACE_MANIFEST_TEMPLATE`] and the paths under
//!   [`plugin_source_dir`] are the layout `codex plugin marketplace add`
//!   walks. A consumer supplies two names and gets the tree Codex reads.
//! * **The invocation.** [`PluginManager::register`] runs the two commands in
//!   order and interprets the answers.
//! * **The quirks.** Codex reports "this is already done" as a *failure* with
//!   a distinguishing phrase on stderr, and it refuses a same-named
//!   marketplace pointing at a different directory. Recognising those answers
//!   is the difference between an install that completes and one that reports
//!   a spurious error, and it is the bulk of what this module knows.
//!
//! # Observed behaviour, and why it is guarded
//!
//! The collision and refresh semantics below were verified against
//! codex-cli 0.146.0. They are matched on stderr *phrases* because the CLI
//! offers nothing better — no machine-readable status, no distinct exit code.
//! Every phrase check therefore only ever reinterprets a **failure**, and only
//! against the specific phrasings observed, so a CLI whose wording moves
//! degrades to an honest failure plus [`PluginManager::manual_commands`]
//! rather than to a silent wrong answer.
//!
//! # What stays with the consumer
//!
//! Bytes on disk and words on a terminal. This module never writes a file,
//! never reads one, and never prints: it takes a marketplace root that already
//! exists and hands back outcomes. Whether a consumer extracts an embedded
//! bundle or renders one, how it records what it has delivered, and how it
//! narrates any of that are its own.

use std::path::{Path, PathBuf};

use super::render_slots;

/// Slot in [`MARKETPLACE_MANIFEST_TEMPLATE`] for the marketplace name — the
/// name `codex plugin marketplace remove` takes and the right-hand side of a
/// `<plugin>@<marketplace>` spec.
pub const MARKETPLACE_NAME_SLOT: &str = "__TAPES_MARKETPLACE_NAME__";

/// Slot for the marketplace's display name, shown when the app lists sources.
pub const MARKETPLACE_DISPLAY_NAME_SLOT: &str = "__TAPES_MARKETPLACE_DISPLAY_NAME__";

/// Slot for the offered plugin's name. The same spelling
/// [`super::PLUGIN_MANIFEST_TEMPLATE`] uses, because it must hold the same
/// value: Codex resolves the offer against the plugin manifest's `name`.
pub const MARKETPLACE_PLUGIN_NAME_SLOT: &str = "__TAPES_PLUGIN_NAME__";

/// Slot for the offered plugin's source path, relative to the marketplace
/// root. Its own slot rather than text spliced around
/// [`MARKETPLACE_PLUGIN_NAME_SLOT`] so substitution stays whole-value and
/// JSON-escaped, exactly as every other slot in this crate is.
pub const MARKETPLACE_PLUGIN_SOURCE_PATH_SLOT: &str = "__TAPES_PLUGIN_SOURCE_PATH__";

/// The marketplace manifest template — [`MARKETPLACE_MANIFEST_PATH`] in the
/// packaged tree.
///
/// One local-source plugin, installable on request and authenticated when it
/// is installed. A marketplace may offer several plugins; this template offers
/// exactly one, which is the shape a capture client needs and the only shape
/// the path helpers here describe.
pub const MARKETPLACE_MANIFEST_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/codex-app/marketplace.json"
));

/// Where [`MARKETPLACE_MANIFEST_TEMPLATE`] is written, relative to the
/// marketplace root a consumer hands `codex plugin marketplace add`.
pub const MARKETPLACE_MANIFEST_PATH: &str = ".agents/plugins/marketplace.json";

/// The two names a marketplace manifest carries, plus the display string the
/// app shows for the source.
///
/// `#[non_exhaustive]` for the reason [`super::HookPluginIdentity`] is; build
/// one with [`MarketplaceIdentity::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MarketplaceIdentity<'a> {
    /// The marketplace name. Codex keys a registered source on it, so two
    /// consumers choosing the same name collide on one machine — see
    /// [`MarketplaceOutcome::Replaced`].
    pub name: &'a str,
    /// The offered plugin's name, which must equal the `name` in the plugin
    /// manifest rendered by [`super::render_plugin_manifest`].
    pub plugin_name: &'a str,
    /// Display name for the source in the app's marketplace list.
    pub display_name: &'a str,
}

impl<'a> MarketplaceIdentity<'a> {
    /// A marketplace offering exactly `plugin_name`, displayed under `name`
    /// until [`Self::with_display_name`] says otherwise.
    #[must_use]
    pub const fn new(name: &'a str, plugin_name: &'a str) -> Self {
        Self {
            name,
            plugin_name,
            display_name: name,
        }
    }

    /// Set the display name shown for the source.
    #[must_use]
    pub const fn with_display_name(mut self, display_name: &'a str) -> Self {
        self.display_name = display_name;
        self
    }
}

/// The plugin's source directory, relative to the marketplace root.
#[must_use]
pub fn plugin_source_dir(plugin_name: &str) -> PathBuf {
    Path::new("plugins").join(plugin_name)
}

/// Where [`super::render_plugin_manifest`]'s output is written, relative to
/// the marketplace root.
#[must_use]
pub fn plugin_manifest_path(plugin_name: &str) -> PathBuf {
    plugin_source_dir(plugin_name)
        .join(".codex-plugin")
        .join("plugin.json")
}

/// Where [`super::render_hooks_manifest`]'s output is written, relative to the
/// marketplace root. This is Codex's *default* hooks location, which is why a
/// rendered plugin manifest declares no `hooks` override.
#[must_use]
pub fn hooks_manifest_path(plugin_name: &str) -> PathBuf {
    plugin_source_dir(plugin_name)
        .join("hooks")
        .join("hooks.json")
}

/// The `<plugin>@<marketplace>` spec `codex plugin add` and
/// `codex plugin remove` take, and the key Codex records enablement under in
/// its `config.toml`.
#[must_use]
pub fn plugin_spec(plugin_name: &str, marketplace_name: &str) -> String {
    format!("{plugin_name}@{marketplace_name}")
}

/// Render the marketplace manifest around a consumer's names.
///
/// The source path is derived from the plugin name rather than accepted as a
/// parameter: it must agree with [`plugin_source_dir`], and a manifest whose
/// path points anywhere else installs nothing.
#[must_use]
pub fn render_marketplace_manifest(identity: &MarketplaceIdentity) -> String {
    let source_path = format!("./{}", plugin_source_dir(identity.plugin_name).display());
    render_slots(
        MARKETPLACE_MANIFEST_TEMPLATE,
        &[
            (MARKETPLACE_NAME_SLOT, identity.name),
            (MARKETPLACE_DISPLAY_NAME_SLOT, identity.display_name),
            (MARKETPLACE_PLUGIN_NAME_SLOT, identity.plugin_name),
            (MARKETPLACE_PLUGIN_SOURCE_PATH_SLOT, &source_path),
        ],
    )
}

/// Whether Codex's `config.toml` marks `plugin_spec` explicitly disabled.
///
/// Deliberately forgiving: unparseable text, an absent table, or an absent key
/// all read as "not disabled", because the question this answers is only
/// "would installing override a choice the user made in the app", and the
/// cost of guessing wrong toward `false` is an install the user asked for.
///
/// Text in, no filesystem — resolving `$CODEX_HOME` and reading the file stay
/// with the consumer, as they do for [`crate::config::codex`].
#[must_use]
pub fn plugin_disabled_in_config(config_text: &str, plugin_spec: &str) -> bool {
    use toml_edit::{Document, Item};

    let Ok(document) = config_text.parse::<Document>() else {
        return false;
    };
    document
        .get("plugins")
        .and_then(Item::as_table_like)
        .and_then(|plugins| plugins.get(plugin_spec))
        .and_then(Item::as_table_like)
        .and_then(|plugin| plugin.get("enabled"))
        .and_then(Item::as_bool)
        == Some(false)
}

/// What a registration run must accomplish on Codex's side.
///
/// The distinction exists because an "already installed" answer is only
/// trustworthy when the caller knows the *current* bytes are what Codex
/// cached. Which of these applies is the consumer's bookkeeping; what each
/// one makes the CLI do is this module's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InstallGoal {
    /// Nothing is known to have been delivered. An existing install is of
    /// unknown provenance — a copy from someone's source checkout, or an
    /// older release — so "already installed" cannot be believed and the
    /// cached copy is forced fresh.
    Install,
    /// A *different* set of bytes was delivered before. Codex's cached copy
    /// is known stale and must be re-copied.
    Refresh,
    /// These exact bytes were delivered and confirmed. "Already installed" is
    /// trustworthy and nothing is forced.
    Verify,
}

/// Outcome of registering the marketplace source.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MarketplaceOutcome {
    /// `codex plugin marketplace add` exited 0.
    Added,
    /// It failed, saying this source is already registered.
    AlreadyAdded,
    /// A marketplace of the same name pointed at a **different** directory
    /// (the state of any machine that registered the plugin from a source
    /// checkout). It was removed and re-added from the caller's root.
    ///
    /// Replacing is safe in Codex's model: removing a marketplace does not
    /// uninstall the plugins installed from it, so the install survives and
    /// the following `plugin add` re-copies it from the new root.
    Replaced {
        /// The name whose previous registration was replaced.
        marketplace_name: String,
    },
    /// The step failed for a real reason.
    Failed {
        /// The CLI's own words, flattened and bounded.
        detail: String,
    },
}

impl MarketplaceOutcome {
    /// One line naming what happened, for a consumer's summary.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Added => "added".to_owned(),
            Self::AlreadyAdded => "already added".to_owned(),
            Self::Replaced { marketplace_name } => format!(
                "replaced an existing '{marketplace_name}' marketplace that pointed at a \
                 different source"
            ),
            Self::Failed { detail } => format!("failed: {detail}"),
        }
    }

    /// Whether the plugin step must be skipped: there is no source to install
    /// from.
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// Outcome of installing or refreshing the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InstallOutcome {
    /// `codex plugin add` succeeded with nothing known to be stale.
    Installed,
    /// The caller knew the current bytes were delivered and the CLI agrees an
    /// install exists.
    AlreadyInstalled,
    /// Codex's cached copy was re-copied from the marketplace root.
    Refreshed,
    /// The step failed; whatever was installed before is still installed.
    Failed {
        /// The CLI's own words, flattened and bounded.
        detail: String,
    },
    /// The forced refresh removed the untrusted install and then failed to
    /// re-add it: the plugin is currently **not** installed, which is the one
    /// outcome a consumer must say out loud.
    RemovedNotReinstalled {
        /// The CLI's own words, flattened and bounded.
        detail: String,
    },
    /// The step never ran because the marketplace step failed.
    Skipped,
    /// The plugin is disabled in Codex's config and the caller said so.
    /// `codex plugin add` sets `enabled = true`, so installing would override
    /// a choice the user made in the app.
    SkippedDisabled,
}

impl InstallOutcome {
    /// One line naming what happened, for a consumer's summary.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Installed => "installed".to_owned(),
            Self::AlreadyInstalled => "already installed".to_owned(),
            Self::Refreshed => "refreshed to the new bundled version".to_owned(),
            Self::Failed { detail } | Self::RemovedNotReinstalled { detail } => {
                format!("failed: {detail}")
            }
            Self::Skipped => "skipped (marketplace registration failed)".to_owned(),
            Self::SkippedDisabled => "skipped: the plugin is disabled in Codex config; \
                                      enable it in the app, then install again (installing \
                                      now would force-re-enable it)"
                .to_owned(),
        }
    }

    /// Whether the consumer's summary should print
    /// [`PluginManager::manual_commands`].
    #[must_use]
    pub fn needs_manual_retry(&self) -> bool {
        matches!(
            self,
            Self::Failed { .. } | Self::RemovedNotReinstalled { .. } | Self::Skipped
        )
    }

    /// Whether this run **confirmed** that Codex's cache now holds the bytes
    /// under the marketplace root — the only outcomes a consumer may record as
    /// delivered.
    #[must_use]
    pub fn confirmed_delivery(&self) -> bool {
        matches!(self, Self::Installed | Self::Refreshed)
    }
}

/// What one [`PluginManager::register`] run found, distinguishing "there is no
/// CLI here at all" from per-step outcomes so a summary never fakes success.
///
/// Deliberately *not* `#[non_exhaustive]`, unlike the outcome enums it holds:
/// this is a closed dichotomy — either the CLI ran or there was none — and
/// every consumer must branch on it. Forcing a wildcard arm here would only
/// invite one that silently swallowed a third case that will never exist. The
/// outcomes inside stay open, because Codex can always give a new answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerRun {
    /// The `codex` program does not exist. Nothing ran and nothing is known;
    /// a consumer prints [`PluginManager::manual_commands`] and leaves its
    /// own delivery bookkeeping untouched.
    CliAbsent,
    /// The CLI ran. Each step reports its own outcome.
    Steps {
        /// Registering the marketplace source.
        marketplace: MarketplaceOutcome,
        /// Installing or refreshing the plugin.
        install: InstallOutcome,
    },
}

/// One packaged plugin, and the `codex` binary that manages it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManager {
    codex_program: PathBuf,
    marketplace_root: PathBuf,
    marketplace_name: String,
    plugin_name: String,
}

impl PluginManager {
    /// Manage `plugin_name`, offered by the marketplace at `marketplace_root`
    /// under `marketplace_name`, through the `codex` binary at
    /// `codex_program`.
    ///
    /// `codex_program` is a parameter rather than the literal `codex` so a
    /// test can inject a shim; production callers pass the bare name and let
    /// `PATH` resolve it. Child processes inherit the caller's environment, so
    /// `$CODEX_HOME` resolves the same for the CLI as for the caller — pinning
    /// it here would only risk desynchronising from a future CLI change.
    #[must_use]
    pub fn new(
        codex_program: impl Into<PathBuf>,
        marketplace_root: impl Into<PathBuf>,
        marketplace_name: impl Into<String>,
        plugin_name: impl Into<String>,
    ) -> Self {
        Self {
            codex_program: codex_program.into(),
            marketplace_root: marketplace_root.into(),
            marketplace_name: marketplace_name.into(),
            plugin_name: plugin_name.into(),
        }
    }

    /// The directory `codex plugin marketplace add` is pointed at.
    #[must_use]
    pub fn marketplace_root(&self) -> &Path {
        &self.marketplace_root
    }

    /// The `<plugin>@<marketplace>` spec this manager installs.
    #[must_use]
    pub fn plugin_spec(&self) -> String {
        plugin_spec(&self.plugin_name, &self.marketplace_name)
    }

    /// The two commands, in order, that [`Self::register`] runs — the exact
    /// text to print when there is no CLI to run them with, or when a step
    /// failed and the user must retry by hand.
    #[must_use]
    pub fn manual_commands(&self) -> [String; 2] {
        [
            format!(
                "codex plugin marketplace add {}",
                self.marketplace_root.display()
            ),
            self.install_command(),
        ]
    }

    /// Just the install command — what a consumer prints to recover from
    /// [`InstallOutcome::RemovedNotReinstalled`].
    #[must_use]
    pub fn install_command(&self) -> String {
        format!("codex plugin add {}", self.plugin_spec())
    }

    /// Register the marketplace and install (or refresh) the plugin.
    ///
    /// `plugin_disabled` is the caller's answer from
    /// [`plugin_disabled_in_config`], passed in rather than read here because
    /// locating `config.toml` is deployment.
    #[must_use]
    pub fn register(&self, goal: InstallGoal, plugin_disabled: bool) -> ManagerRun {
        let Some(marketplace) = self.register_marketplace() else {
            return ManagerRun::CliAbsent;
        };
        let install = if plugin_disabled {
            InstallOutcome::SkippedDisabled
        } else if marketplace.failed() {
            InstallOutcome::Skipped
        } else {
            self.install(goal)
        };
        ManagerRun::Steps {
            marketplace,
            install,
        }
    }

    /// Register the marketplace, replacing a same-named one that points
    /// elsewhere. `None` means the `codex` program does not exist.
    ///
    /// The collision check runs before the generic "already" check because the
    /// collision error *also* contains "already added": ordering them the
    /// other way would report a stale registration as a success and install
    /// from the wrong directory.
    fn register_marketplace(&self) -> Option<MarketplaceOutcome> {
        match self.run_marketplace_add() {
            Invocation::Missing => None,
            Invocation::Ran { success: true, .. } => Some(MarketplaceOutcome::Added),
            Invocation::Ran { detail, .. } => {
                let lowered = detail.to_ascii_lowercase();
                if lowered.contains("different source") {
                    Some(self.replace_marketplace())
                } else if says_already(&lowered) {
                    Some(MarketplaceOutcome::AlreadyAdded)
                } else {
                    Some(MarketplaceOutcome::Failed { detail })
                }
            }
        }
    }

    fn replace_marketplace(&self) -> MarketplaceOutcome {
        let name = &self.marketplace_name;
        match self.run(&["plugin", "marketplace", "remove", name]) {
            Invocation::Missing => {
                return MarketplaceOutcome::Failed {
                    detail: CLI_VANISHED.to_owned(),
                };
            }
            Invocation::Ran {
                success: false,
                detail,
            } => {
                return MarketplaceOutcome::Failed {
                    detail: format!(
                        "an existing '{name}' marketplace points at a different source and \
                         `codex plugin marketplace remove {name}` failed: {detail}"
                    ),
                };
            }
            Invocation::Ran { success: true, .. } => {}
        }
        match self.run_marketplace_add() {
            Invocation::Ran { success: true, .. } => MarketplaceOutcome::Replaced {
                marketplace_name: name.clone(),
            },
            Invocation::Ran { detail, .. } => MarketplaceOutcome::Failed {
                detail: format!(
                    "removed the previous '{name}' marketplace but re-adding the managed one \
                     failed: {detail}"
                ),
            },
            Invocation::Missing => MarketplaceOutcome::Failed {
                detail: CLI_VANISHED.to_owned(),
            },
        }
    }

    /// Install the plugin, forcing a cache refresh whenever the goal says the
    /// cached copy cannot be trusted.
    ///
    /// codex-cli 0.146.0 has no `plugin update` subcommand, and
    /// `plugin marketplace upgrade` only refreshes Git-sourced snapshots — but
    /// `plugin add` against a *local* marketplace exits 0 and re-copies the
    /// cached plugin on every run, so a plain re-`add` is the native refresh.
    /// The remove-then-re-add fallback below exists for CLI versions that
    /// instead report the existing install without re-copying.
    fn install(&self, goal: InstallGoal) -> InstallOutcome {
        match self.run_plugin_add() {
            Invocation::Missing => InstallOutcome::Failed {
                detail: CLI_VANISHED.to_owned(),
            },
            Invocation::Ran { success: true, .. } => {
                if goal == InstallGoal::Refresh {
                    InstallOutcome::Refreshed
                } else {
                    InstallOutcome::Installed
                }
            }
            Invocation::Ran { detail, .. } => {
                if says_already(&detail.to_ascii_lowercase()) {
                    match goal {
                        InstallGoal::Verify => InstallOutcome::AlreadyInstalled,
                        InstallGoal::Install | InstallGoal::Refresh => self.force_refresh(),
                    }
                } else {
                    InstallOutcome::Failed { detail }
                }
            }
        }
    }

    /// Remove the untrusted install, then re-add it from the marketplace root.
    ///
    /// Failure ordering carries the whole meaning: a failed *remove* leaves
    /// the stale plugin installed and is a plain failure, while a successful
    /// remove followed by a failed *re-add* leaves the plugin uninstalled —
    /// strictly worse than doing nothing, and the only case a consumer must
    /// hand the user a recovery command for.
    fn force_refresh(&self) -> InstallOutcome {
        let spec = self.plugin_spec();
        match self.run(&["plugin", "remove", &spec]) {
            Invocation::Missing => {
                return InstallOutcome::Failed {
                    detail: CLI_VANISHED.to_owned(),
                };
            }
            Invocation::Ran {
                success: false,
                detail,
            } => {
                // Nothing to remove is a fine starting point for the re-add.
                if !says_nothing_to_remove(&detail.to_ascii_lowercase()) {
                    return InstallOutcome::Failed {
                        detail: format!(
                            "the installed plugin is stale and `codex plugin remove` failed: \
                             {detail}"
                        ),
                    };
                }
            }
            Invocation::Ran { success: true, .. } => {}
        }
        match self.run_plugin_add() {
            Invocation::Ran { success: true, .. } => InstallOutcome::Refreshed,
            Invocation::Ran { detail, .. } => {
                if says_already(&detail.to_ascii_lowercase()) {
                    InstallOutcome::Failed {
                        detail: "codex plugin add still reports an existing install after \
                                 remove; refresh manually"
                            .to_owned(),
                    }
                } else {
                    InstallOutcome::RemovedNotReinstalled { detail }
                }
            }
            Invocation::Missing => InstallOutcome::RemovedNotReinstalled {
                detail: CLI_VANISHED.to_owned(),
            },
        }
    }

    fn run_marketplace_add(&self) -> Invocation {
        let root = self.marketplace_root.clone();
        let mut command = std::process::Command::new(&self.codex_program);
        command.args(["plugin", "marketplace", "add"]).arg(root);
        run_invocation(command)
    }

    fn run_plugin_add(&self) -> Invocation {
        self.run(&["plugin", "add", &self.plugin_spec()])
    }

    fn run(&self, args: &[&str]) -> Invocation {
        let mut command = std::process::Command::new(&self.codex_program);
        command.args(args);
        run_invocation(command)
    }
}

/// Detail for the narrow window where the `codex` binary existed for one
/// command and not the next.
const CLI_VANISHED: &str = "codex CLI disappeared between commands";

/// One `codex` invocation, uninterpreted.
enum Invocation {
    /// The `codex` program does not exist.
    Missing,
    /// It ran; exit status plus flattened output.
    Ran { success: bool, detail: String },
}

/// Whether a **failed** invocation's lowercased output says the work was
/// already done.
///
/// Matches the specific phrasings the CLI uses rather than a bare "already",
/// so unrelated errors that happen to contain the word (a file "already in
/// use") stay failures.
fn says_already(lowered_detail: &str) -> bool {
    ["already added", "already installed", "already exists"]
        .iter()
        .any(|phrase| lowered_detail.contains(phrase))
}

/// Whether a **failed** remove's lowercased output says there was nothing to
/// remove.
fn says_nothing_to_remove(lowered_detail: &str) -> bool {
    ["not installed", "not configured", "already removed"]
        .iter()
        .any(|phrase| lowered_detail.contains(phrase))
}

/// Run one invocation with stdin closed and output captured.
///
/// Stdin is closed because a plugin manager that decides to prompt would
/// otherwise hang a non-interactive install forever. Only
/// [`std::io::ErrorKind::NotFound`] is [`Invocation::Missing`]; any other
/// spawn failure is a failed run carrying the OS error, so a permission
/// problem reads as a failure rather than as an absent CLI.
fn run_invocation(mut command: std::process::Command) -> Invocation {
    let output = match command.stdin(std::process::Stdio::null()).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Invocation::Missing;
        }
        Err(error) => {
            return Invocation::Ran {
                success: false,
                detail: error.to_string(),
            };
        }
    };
    if output.status.success() {
        return Invocation::Ran {
            success: true,
            detail: String::new(),
        };
    }
    Invocation::Ran {
        success: false,
        detail: summarize_output(&output),
    }
}

/// Flatten a failed invocation's stderr and stdout into one bounded line.
///
/// Bounded because the result is both matched on and printed: an unbounded
/// CLI dump would push a consumer's own summary off the screen.
fn summarize_output(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut detail = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if detail.chars().count() > MAX_DETAIL_CHARS {
        detail = detail.chars().take(MAX_DETAIL_CHARS).collect::<String>() + "…";
    }
    if detail.is_empty() {
        detail = format!("exited with {}", output.status);
    }
    detail
}

/// Cap on a flattened CLI detail, in characters.
const MAX_DETAIL_CHARS: usize = 200;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::plugin::codex_app::{HookPluginIdentity, render_plugin_manifest};

    fn identity() -> MarketplaceIdentity<'static> {
        MarketplaceIdentity::new("acme", "acme-codex").with_display_name("Acme")
    }

    fn manager(codex_program: PathBuf, root: &Path) -> PluginManager {
        PluginManager::new(
            codex_program,
            root.join("marketplace"),
            "acme",
            "acme-codex",
        )
    }

    fn missing_codex(root: &Path) -> PathBuf {
        root.join("codex-not-installed")
    }

    /// A `codex` shim that appends its arguments to `invocations.log` and
    /// scripts per-subcommand behaviour, so a test asserts exact invocations
    /// without touching `PATH`.
    #[cfg(unix)]
    fn write_codex_shim(root: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let log = root.join("invocations.log");
        let path = root.join("codex");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$@\" >> \"{}\"\n{body}\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn shim_log(root: &Path) -> Vec<String> {
        std::fs::read_to_string(root.join("invocations.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[cfg(unix)]
    fn add_marketplace(root: &Path) -> String {
        format!(
            "plugin marketplace add {}",
            root.join("marketplace").display()
        )
    }

    #[test]
    fn the_rendered_marketplace_offers_the_plugin_at_the_path_the_helpers_name() {
        let rendered = render_marketplace_manifest(&identity());
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert!(!rendered.contains("__TAPES_"), "{rendered}");
        assert_eq!(parsed["name"], "acme");
        assert_eq!(parsed["interface"]["displayName"], "Acme");
        let plugins = parsed["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["name"], "acme-codex");
        assert_eq!(plugins[0]["source"]["source"], "local");

        // The offered path must cover the files the path helpers place, or the
        // marketplace advertises a plugin Codex cannot find.
        let offered = plugins[0]["source"]["path"].as_str().unwrap();
        let offered = Path::new(offered.trim_start_matches("./"));
        assert_eq!(offered, plugin_source_dir("acme-codex"));
        for path in [
            plugin_manifest_path("acme-codex"),
            hooks_manifest_path("acme-codex"),
        ] {
            assert!(
                path.starts_with(offered),
                "{} escapes {offered:?}",
                path.display()
            );
        }
    }

    /// The marketplace's plugin name and the plugin manifest's `name` are how
    /// Codex resolves an offer to a directory; a drift between them installs
    /// nothing. The spec a consumer hands `plugin add` is built from the same
    /// pair.
    #[test]
    fn the_offered_name_the_manifest_name_and_the_spec_agree() {
        let marketplace: serde_json::Value =
            serde_json::from_str(&render_marketplace_manifest(&identity())).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&render_plugin_manifest(
            &HookPluginIdentity::new("acme-codex", "1.0.0"),
        ))
        .unwrap();

        assert_eq!(marketplace["plugins"][0]["name"], manifest["name"]);
        assert_eq!(
            plugin_spec("acme-codex", "acme"),
            format!(
                "{}@{}",
                marketplace["plugins"][0]["name"].as_str().unwrap(),
                marketplace["name"].as_str().unwrap()
            )
        );
    }

    /// A minimal identity leaves no slot behind, and the display name falls
    /// back to the marketplace name rather than to an empty string.
    #[test]
    fn a_minimal_marketplace_identity_fills_every_slot() {
        let rendered = render_marketplace_manifest(&MarketplaceIdentity::new("bare", "bare-codex"));
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert!(!rendered.contains("__TAPES_"), "{rendered}");
        assert_eq!(parsed["interface"]["displayName"], "bare");
    }

    /// The de-branding bar every crate-owned asset meets.
    #[test]
    fn the_marketplace_template_carries_no_vendor_branding() {
        let lowered = MARKETPLACE_MANIFEST_TEMPLATE.to_ascii_lowercase();
        for token in ["paper", "papercompute", "tapesctl"] {
            assert!(!lowered.contains(token), "the template mentions {token:?}");
        }
    }

    #[test]
    fn every_marketplace_slot_is_filled_and_none_is_unknown() {
        for slot in [
            MARKETPLACE_NAME_SLOT,
            MARKETPLACE_DISPLAY_NAME_SLOT,
            MARKETPLACE_PLUGIN_NAME_SLOT,
            MARKETPLACE_PLUGIN_SOURCE_PATH_SLOT,
        ] {
            assert!(
                MARKETPLACE_MANIFEST_TEMPLATE.contains(&format!("\"{slot}\"")),
                "template is missing slot {slot}"
            );
        }
        assert_eq!(MARKETPLACE_MANIFEST_TEMPLATE.matches("__TAPES_").count(), 4);
    }

    #[test]
    fn an_absent_cli_is_reported_rather_than_failed() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(missing_codex(root.path()), root.path());

        for goal in [
            InstallGoal::Install,
            InstallGoal::Refresh,
            InstallGoal::Verify,
        ] {
            assert_eq!(manager.register(goal, false), ManagerRun::CliAbsent);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_clean_run_adds_the_marketplace_then_the_plugin() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(write_codex_shim(root.path(), "exit 0"), root.path());

        let run = manager.register(InstallGoal::Install, false);

        assert_eq!(
            run,
            ManagerRun::Steps {
                marketplace: MarketplaceOutcome::Added,
                install: InstallOutcome::Installed,
            }
        );
        assert_eq!(
            shim_log(root.path()),
            vec![
                add_marketplace(root.path()),
                "plugin add acme-codex@acme".to_owned(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn already_wording_is_trusted_only_when_the_caller_confirmed_delivery() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(
                root.path(),
                "echo 'error: marketplace already exists' >&2\nexit 1",
            ),
            root.path(),
        );

        let run = manager.register(InstallGoal::Verify, false);

        assert_eq!(
            run,
            ManagerRun::Steps {
                marketplace: MarketplaceOutcome::AlreadyAdded,
                install: InstallOutcome::AlreadyInstalled,
            }
        );
    }

    /// Unrecognised failure wording must stay a failure: reinterpreting it
    /// would report an install that never happened.
    #[cfg(unix)]
    #[test]
    fn unrecognised_failure_wording_skips_the_install() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(root.path(), "echo 'boom: no permission' >&2\nexit 2"),
            root.path(),
        );

        let run = manager.register(InstallGoal::Install, false);

        assert_eq!(
            run,
            ManagerRun::Steps {
                marketplace: MarketplaceOutcome::Failed {
                    detail: "boom: no permission".to_owned()
                },
                install: InstallOutcome::Skipped,
            }
        );
        assert_eq!(
            shim_log(root.path()).len(),
            1,
            "the plugin add must not run"
        );
    }

    /// codex-cli 0.146.0's own refresh path: `plugin add` exits 0 and
    /// re-copies, so the fallback must not fire.
    #[cfg(unix)]
    #[test]
    fn a_cooperative_add_refreshes_without_the_remove_fallback() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(write_codex_shim(root.path(), "exit 0"), root.path());

        let run = manager.register(InstallGoal::Refresh, false);

        assert_eq!(
            run,
            ManagerRun::Steps {
                marketplace: MarketplaceOutcome::Added,
                install: InstallOutcome::Refreshed,
            }
        );
        assert_eq!(
            shim_log(root.path()),
            vec![
                add_marketplace(root.path()),
                "plugin add acme-codex@acme".to_owned(),
            ]
        );
    }

    #[cfg(unix)]
    fn add_is_sticky_until_removed(root: &Path) -> PathBuf {
        write_codex_shim(
            root,
            &format!(
                "case \"$*\" in\n  \
                 *'plugin remove'*) touch \"{removed}\"; exit 0 ;;\n  \
                 *'plugin add'*) if [ -f \"{removed}\" ]; then exit 0; \
                 else echo 'plugin is already installed' >&2; exit 1; fi ;;\n  \
                 *) exit 0 ;;\nesac",
                removed = root.join("removed.sentinel").display()
            ),
        )
    }

    #[cfg(unix)]
    #[test]
    fn an_uncooperative_add_falls_back_to_remove_then_re_add() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(add_is_sticky_until_removed(root.path()), root.path());

        let run = manager.register(InstallGoal::Refresh, false);

        assert_eq!(
            run,
            ManagerRun::Steps {
                marketplace: MarketplaceOutcome::Added,
                install: InstallOutcome::Refreshed,
            }
        );
        assert_eq!(
            shim_log(root.path()),
            vec![
                add_marketplace(root.path()),
                "plugin add acme-codex@acme".to_owned(),
                "plugin remove acme-codex@acme".to_owned(),
                "plugin add acme-codex@acme".to_owned(),
            ],
            "fallback order must be add, remove, re-add"
        );
    }

    /// An install of unknown provenance is forced fresh even though nothing is
    /// known to be stale: the cached copy could be anyone's.
    #[cfg(unix)]
    #[test]
    fn an_unconfirmed_existing_install_is_forced_fresh() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(add_is_sticky_until_removed(root.path()), root.path());

        let run = manager.register(InstallGoal::Install, false);

        assert!(
            matches!(
                run,
                ManagerRun::Steps {
                    install: InstallOutcome::Refreshed,
                    ..
                }
            ),
            "{run:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_remove_keeps_the_stale_install_in_place() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(
                root.path(),
                "case \"$*\" in\n  \
                 *'plugin remove'*) echo 'remove blew up' >&2; exit 2 ;;\n  \
                 *'plugin add'*) echo 'plugin is already installed' >&2; exit 1 ;;\n  \
                 *) exit 0 ;;\nesac",
            ),
            root.path(),
        );

        let run = manager.register(InstallGoal::Refresh, false);

        let ManagerRun::Steps { install, .. } = run else {
            panic!("expected steps");
        };
        let InstallOutcome::Failed { detail } = install else {
            panic!("expected a plain failure, got {install:?}");
        };
        assert!(detail.contains("codex plugin remove"), "{detail}");
        assert!(detail.contains("remove blew up"), "{detail}");
        assert_eq!(
            shim_log(root.path())
                .iter()
                .filter(|line| line.starts_with("plugin add"))
                .count(),
            1,
            "no re-add may follow a failed remove"
        );
    }

    /// Nothing-to-remove wording is not a failure: it is the state the re-add
    /// wants anyway.
    #[cfg(unix)]
    #[test]
    fn a_remove_that_had_nothing_to_remove_proceeds_to_the_re_add() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(
                root.path(),
                &format!(
                    "case \"$*\" in\n  \
                     *'plugin remove'*) touch \"{done}\"; echo 'plugin is not installed' >&2; \
                     exit 1 ;;\n  \
                     *'plugin add'*) if [ -f \"{done}\" ]; then exit 0; \
                     else echo 'plugin is already installed' >&2; exit 1; fi ;;\n  \
                     *) exit 0 ;;\nesac",
                    done = root.path().join("removed.sentinel").display()
                ),
            ),
            root.path(),
        );

        let run = manager.register(InstallGoal::Refresh, false);

        assert!(
            matches!(
                run,
                ManagerRun::Steps {
                    install: InstallOutcome::Refreshed,
                    ..
                }
            ),
            "{run:?}"
        );
    }

    /// The one outcome that leaves the machine worse off than doing nothing.
    #[cfg(unix)]
    #[test]
    fn a_failed_re_add_after_a_successful_remove_says_so() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(
                root.path(),
                &format!(
                    "case \"$*\" in\n  \
                     *'plugin remove'*) touch \"{removed}\"; exit 0 ;;\n  \
                     *'plugin add'*) if [ -f \"{removed}\" ]; then echo 'network exploded' >&2; \
                     exit 2; else echo 'plugin is already installed' >&2; exit 1; fi ;;\n  \
                     *) exit 0 ;;\nesac",
                    removed = root.path().join("removed.sentinel").display()
                ),
            ),
            root.path(),
        );

        let run = manager.register(InstallGoal::Refresh, false);

        let ManagerRun::Steps { install, .. } = run else {
            panic!("expected steps");
        };
        assert_eq!(
            install,
            InstallOutcome::RemovedNotReinstalled {
                detail: "network exploded".to_owned()
            }
        );
        assert!(install.needs_manual_retry());
        assert!(!install.confirmed_delivery());
    }

    /// Collision wording verified live against codex-cli 0.146.0. The
    /// collision error also contains "already added", so the ordering inside
    /// [`PluginManager::register_marketplace`] is what this pins.
    #[cfg(unix)]
    #[test]
    fn a_same_named_marketplace_at_another_source_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(
                root.path(),
                &format!(
                    "case \"$*\" in\n  \
                     *'plugin marketplace remove'*) touch \"{removed}\"; exit 0 ;;\n  \
                     *'plugin marketplace add'*) if [ -f \"{removed}\" ]; then exit 0; \
                     else echo \"Error: marketplace 'acme' is already added from a different \
                     source; remove it before adding this source\" >&2; exit 1; fi ;;\n  \
                     *) exit 0 ;;\nesac",
                    removed = root.path().join("mkt-removed.sentinel").display()
                ),
            ),
            root.path(),
        );

        let run = manager.register(InstallGoal::Install, false);

        assert_eq!(
            run,
            ManagerRun::Steps {
                marketplace: MarketplaceOutcome::Replaced {
                    marketplace_name: "acme".to_owned()
                },
                install: InstallOutcome::Installed,
            }
        );
        assert_eq!(
            shim_log(root.path()),
            vec![
                add_marketplace(root.path()),
                "plugin marketplace remove acme".to_owned(),
                add_marketplace(root.path()),
                "plugin add acme-codex@acme".to_owned(),
            ],
            "collision order must be add, remove, re-add, plugin add"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_collision_whose_removal_fails_is_reported_with_both_reasons() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(
                root.path(),
                "case \"$*\" in\n  \
                 *'plugin marketplace remove'*) echo 'permission denied' >&2; exit 2 ;;\n  \
                 *'plugin marketplace add'*) echo \"Error: marketplace 'acme' is already added \
                 from a different source; remove it before adding this source\" >&2; exit 1 ;;\n  \
                 *) exit 0 ;;\nesac",
            ),
            root.path(),
        );

        let run = manager.register(InstallGoal::Install, false);

        let ManagerRun::Steps {
            marketplace,
            install,
        } = run
        else {
            panic!("expected steps");
        };
        let MarketplaceOutcome::Failed { detail } = marketplace else {
            panic!("expected a failure, got {marketplace:?}");
        };
        assert!(detail.contains("different source"), "{detail}");
        assert!(detail.contains("permission denied"), "{detail}");
        assert_eq!(install, InstallOutcome::Skipped);
    }

    #[cfg(unix)]
    #[test]
    fn a_disabled_plugin_is_never_installed_over() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(write_codex_shim(root.path(), "exit 0"), root.path());

        let run = manager.register(InstallGoal::Install, true);

        assert_eq!(
            run,
            ManagerRun::Steps {
                marketplace: MarketplaceOutcome::Added,
                install: InstallOutcome::SkippedDisabled,
            }
        );
        assert!(
            !shim_log(root.path())
                .iter()
                .any(|line| line.starts_with("plugin add")),
            "`codex plugin add` would force-re-enable the plugin"
        );
    }

    /// A CLI that fails with no output at all still produces a detail a user
    /// can act on, rather than an empty "failed: ".
    #[cfg(unix)]
    #[test]
    fn a_silent_failure_still_carries_a_detail() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(write_codex_shim(root.path(), "exit 3"), root.path());

        let run = manager.register(InstallGoal::Install, false);

        let ManagerRun::Steps { marketplace, .. } = run else {
            panic!("expected steps");
        };
        let MarketplaceOutcome::Failed { detail } = marketplace else {
            panic!("expected a failure, got {marketplace:?}");
        };
        assert!(detail.contains("exited with"), "{detail}");
    }

    #[cfg(unix)]
    #[test]
    fn a_long_failure_detail_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(
            write_codex_shim(
                root.path(),
                "yes x | head -c 5000 | tr -d '\\n' >&2; exit 1",
            ),
            root.path(),
        );

        let run = manager.register(InstallGoal::Install, false);

        let ManagerRun::Steps { marketplace, .. } = run else {
            panic!("expected steps");
        };
        let MarketplaceOutcome::Failed { detail } = marketplace else {
            panic!("expected a failure, got {marketplace:?}");
        };
        assert_eq!(detail.chars().count(), MAX_DETAIL_CHARS + 1, "{detail}");
        assert!(detail.ends_with('…'), "{detail}");
    }

    #[test]
    fn the_manual_commands_are_the_commands_a_run_would_have_issued() {
        let root = tempfile::tempdir().unwrap();
        let manager = manager(missing_codex(root.path()), root.path());

        assert_eq!(
            manager.manual_commands(),
            [
                format!(
                    "codex plugin marketplace add {}",
                    manager.marketplace_root().display()
                ),
                "codex plugin add acme-codex@acme".to_owned(),
            ]
        );
        assert_eq!(manager.install_command(), manager.manual_commands()[1]);
    }

    #[test]
    fn only_an_explicit_false_reads_as_disabled() {
        let spec = plugin_spec("acme-codex", "acme");

        assert!(plugin_disabled_in_config(
            &format!("[plugins.\"{spec}\"]\nenabled = false\n"),
            &spec
        ));
        assert!(!plugin_disabled_in_config(
            &format!("[plugins.\"{spec}\"]\nenabled = true\n"),
            &spec
        ));
        // Absent table, absent key, another plugin's entry, and unparseable
        // text all mean "the user has not said no".
        assert!(!plugin_disabled_in_config("", &spec));
        assert!(!plugin_disabled_in_config(
            &format!("[plugins.\"{spec}\"]\n"),
            &spec
        ));
        assert!(!plugin_disabled_in_config(
            "[plugins.\"other@acme\"]\nenabled = false\n",
            &spec
        ));
        assert!(!plugin_disabled_in_config("not = [valid\n", &spec));
    }

    #[test]
    fn describes_cover_every_outcome_without_leaking_a_debug_shape() {
        for outcome in [
            MarketplaceOutcome::Added,
            MarketplaceOutcome::AlreadyAdded,
            MarketplaceOutcome::Replaced {
                marketplace_name: "acme".to_owned(),
            },
            MarketplaceOutcome::Failed {
                detail: "boom".to_owned(),
            },
        ] {
            let described = outcome.describe();
            assert!(!described.is_empty());
            assert!(!described.contains('{'), "{described}");
        }
        for outcome in [
            InstallOutcome::Installed,
            InstallOutcome::AlreadyInstalled,
            InstallOutcome::Refreshed,
            InstallOutcome::Failed {
                detail: "boom".to_owned(),
            },
            InstallOutcome::RemovedNotReinstalled {
                detail: "boom".to_owned(),
            },
            InstallOutcome::Skipped,
            InstallOutcome::SkippedDisabled,
        ] {
            let described = outcome.describe();
            assert!(!described.is_empty());
            assert!(!described.contains('{'), "{described}");
        }
    }

    /// Only the two outcomes that prove Codex re-copied the bytes may advance
    /// a consumer's delivered record; everything else must leave delivery
    /// pending so a later run retries.
    #[test]
    fn only_a_proven_copy_counts_as_delivered() {
        assert!(InstallOutcome::Installed.confirmed_delivery());
        assert!(InstallOutcome::Refreshed.confirmed_delivery());
        for outcome in [
            InstallOutcome::AlreadyInstalled,
            InstallOutcome::Failed {
                detail: String::new(),
            },
            InstallOutcome::RemovedNotReinstalled {
                detail: String::new(),
            },
            InstallOutcome::Skipped,
            InstallOutcome::SkippedDisabled,
        ] {
            assert!(!outcome.confirmed_delivery(), "{outcome:?}");
        }
    }
}
