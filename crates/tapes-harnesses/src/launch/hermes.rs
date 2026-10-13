//! hermes-agent launch recipe.
//!
//! hermes (Nous Research's `hermes-agent`) has no base-URL environment knob a
//! recipe could set and leave the rest alone: `OPENAI_BASE_URL` is no longer
//! consulted, and `CUSTOM_BASE_URL` / `HERMES_INFERENCE_PROVIDER` only apply
//! when the user's own `config.yaml` names no provider. Its endpoint lives in
//! `$HERMES_HOME/config.yaml` — `model.provider: custom` plus `model.base_url`
//! — so redirecting it means writing that file into a home the consumer
//! chose, and pointing [`HERMES_HOME_ENV`] at it.
//!
//! # Why a relocated home, and what it costs
//!
//! `HERMES_HOME` is hermes's own variable (it is also what `hermes -p
//! <profile>` sets), so relocating it is far narrower than opencode's
//! `XDG_CONFIG_HOME`: nothing outside hermes reads it. It is still the whole
//! hermes home, not just the config. Inside a planned home hermes has **no**
//! `.env`, no `auth.json`, no memories, skills, or session history from the
//! user's `~/.hermes`. That is deliberate on the secrets side — this recipe never
//! reads the user's home and never copies a credential: the capture proxy (or
//! the key the consumer passes to [`HermesRecipe::with_api_key`]) is the only
//! credential path. On the state side it is a real cost, and the lever is the
//! consumer's choice of root: a **persistent** per-consumer directory (say
//! `~/.cache/<consumer>/hermes-home`) keeps sessions, memories, and skills
//! across captured launches, where a temporary one starts fresh each time.
//! Rewriting the planned files over a persistent root on every launch is
//! intended — they are the redirect, and hermes's own state lives beside them.
//!
//! The planned `config.yaml` is emitted as JSON. JSON is YAML, hermes loads the
//! file with a YAML safe loader, and emitting JSON keeps this crate free of a
//! YAML serializer and its quoting rules.
//!
//! # Why the plan carries the capture plugin
//!
//! For opencode's reason, sharper. Relocating the home moves hermes's user
//! plugin directory, so a plugin installed under `~/.hermes/plugins` is not on
//! any path a recipe-launched session scans — and hermes loads a user plugin
//! only when `config.yaml`'s `plugins.enabled` lists it, which an installed
//! copy alone never arranges. This recipe owns `config.yaml`, so it writes the
//! plugin into the planned home *and* enables it there; the plan is
//! self-contained and `plugin install` is not a precondition.
//!
//! The plugin attributes; it does not redirect (hermes middleware rewrites a
//! request's kwargs, not the client's base URL), so this config is the one
//! pointing mechanism. The plan also sets [`crate::plugin::GATEWAY_URL_ENV`] to
//! the endpoint: the plugin is inert without it, and stamps only requests whose
//! base URL lies beneath it. A consumer whose gateway address is a *root* the
//! endpoint sits under may override that variable with the root; the plugin's
//! check accepts either. The nonce ([`crate::plugin::GATEWAY_NONCE_ENV`]) and
//! the launch-lineage variables ([`tapes_capture::launch`]) remain the
//! consumer's to set.
//!
//! # What escapes capture
//!
//! Only traffic through the `custom` provider is redirected. The planned
//! config declares no other provider, no fallback chain, and no per-task
//! auxiliary endpoint, so nothing in it routes around the proxy, and a user
//! who switches provider inside the session leaves capture — worth a
//! consumer's warning.
//!
//! hermes's auxiliary calls (title generation, compression, vision,
//! web_extract, session_search, skills_hub, mcp) skip request middleware; the
//! plugin stamps them separately, tagging each with
//! `x-tapes-harness-metadata` `{"aux_task": …}`. Their session id is the one
//! the main loop last used — per execution context, falling back to the
//! process's most recent for a background thread — so in a process running
//! several concurrent sessions an auxiliary call may attach to the most
//! recent one. The config also sets `model.default_headers` to a static
//! `x-tapes-harness-id: hermes`, so if the plugin's auxiliary hook cannot
//! install on some hermes release those calls still name the harness. That
//! floor alone is not a complete envelope (it has no session id); a capture
//! client applying tapes-capture's completeness rule still files such a call
//! as unattributed.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use snafu::ResultExt;

use super::{ConfigFile, LaunchError, LaunchPlan, LaunchRecipe, ProxyEndpoint, launch_error};

/// The `X-Tapes-Harness-Id` value for hermes traffic, taken from the registry.
const HARNESS_ID: &str = crate::harness::HERMES.id();

/// The environment variable hermes resolves its home directory from.
pub const HERMES_HOME_ENV: &str = "HERMES_HOME";

/// hermes's config document, relative to its home.
const CONFIG_FILE_NAME: &str = "config.yaml";

/// The provider id hermes uses for an arbitrary OpenAI-compatible (or
/// Anthropic-compatible, with `api_mode`) endpoint.
const CUSTOM_PROVIDER: &str = "custom";

/// Launch hermes against a capture proxy by planning its home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HermesRecipe {
    home: PathBuf,
    endpoint: ProxyEndpoint,
    model: Option<String>,
    api_key: Option<String>,
    api_mode: Option<String>,
}

impl HermesRecipe {
    /// Plan a hermes home at `home` whose `custom` provider points at
    /// `endpoint`.
    ///
    /// `home` is what [`HERMES_HOME_ENV`] will be set to. The consumer creates
    /// it — see the module docs on choosing a persistent root — and owns its
    /// cleanup. `endpoint` is written to `model.base_url` verbatim: hermes's
    /// OpenAI client appends `/chat/completions`, so any `/v1` belongs in it.
    pub fn new(home: impl Into<PathBuf>, endpoint: ProxyEndpoint) -> Self {
        Self {
            home: home.into(),
            endpoint,
            model: None,
            api_key: None,
            api_mode: None,
        }
    }

    /// The model hermes starts on (`model.default`). Without one, hermes asks
    /// a loopback endpoint's `/v1/models` for a default, which a capture proxy
    /// may or may not answer; naming the model is the reliable road.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// A credential to write to `model.api_key`. Omitted, hermes sends
    /// `no-key-required`, which is right for a proxy that supplies the real
    /// credential itself. A plan carrying a key should be materialised
    /// private (`0o600`).
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// The wire protocol hermes speaks to the endpoint (`model.api_mode`):
    /// `chat_completions` (hermes's default for `custom`),
    /// `anthropic_messages`, or `codex_responses`. Written verbatim; hermes
    /// ignores a value it does not recognise.
    pub fn with_api_mode(mut self, api_mode: impl Into<String>) -> Self {
        self.api_mode = Some(api_mode.into());
        self
    }

    /// Absolute path the plan writes `config.yaml` to.
    pub fn config_path(&self) -> PathBuf {
        self.home.join(CONFIG_FILE_NAME)
    }

    /// Absolute paths the plan writes the capture plugin's files to, in the
    /// plugin's artifact order (module, then manifest).
    pub fn plugin_paths(&self) -> Vec<PathBuf> {
        crate::plugin::HERMES_ARTIFACTS
            .iter()
            .map(|artifact| plugin_path_under(&self.home, artifact))
            .collect()
    }

    fn document(&self) -> Value {
        let mut model = Map::new();
        model.insert("provider".to_owned(), json!(CUSTOM_PROVIDER));
        model.insert("base_url".to_owned(), json!(self.endpoint.as_str()));
        if let Some(default) = &self.model {
            model.insert("default".to_owned(), json!(default));
        }
        if let Some(api_key) = &self.api_key {
            model.insert("api_key".to_owned(), json!(api_key));
        }
        if let Some(api_mode) = &self.api_mode {
            model.insert("api_mode".to_owned(), json!(api_mode));
        }
        // A static harness id on every request hermes makes to this provider,
        // main and auxiliary alike — hermes merges `model.default_headers`
        // into both clients. It is the floor under the plugin: should the
        // plugin's auxiliary stamping not install on some hermes release, an
        // auxiliary call still names its harness rather than arriving bare.
        // The plugin's per-request `extra_headers` override it with the full
        // envelope wherever it runs.
        model.insert(
            "default_headers".to_owned(),
            json!({ tapes_capture::envelope::X_TAPES_HARNESS_ID: HARNESS_ID }),
        );
        json!({
            "model": Value::Object(model),
            // Stated empty rather than left to defaults: any provider the
            // proxy does not front is a route around capture.
            "providers": {},
            "fallback_providers": [],
            "plugins": { "enabled": [crate::plugin::HERMES_GATEWAY_PLUGIN_NAME] },
        })
    }
}

/// Where an artifact lands beneath a relocated hermes home: its install
/// directory with the leading `.hermes` — the component `HERMES_HOME`
/// replaces — dropped.
fn plugin_path_under(home: &Path, artifact: &crate::plugin::PluginArtifact) -> PathBuf {
    artifact
        .install_dir_components()
        .iter()
        .skip(1)
        .fold(home.to_path_buf(), |path, component| path.join(component))
        .join(artifact.file_name())
}

impl LaunchRecipe for HermesRecipe {
    fn harness(&self) -> &str {
        HARNESS_ID
    }

    fn plan(&self) -> Result<LaunchPlan, LaunchError> {
        let contents = serde_json::to_string_pretty(&self.document())
            .context(launch_error::SerializeHermesConfigSnafu)?;
        let mut config_files = vec![ConfigFile {
            path: self.config_path(),
            contents,
        }];
        config_files.extend(
            crate::plugin::HERMES_ARTIFACTS
                .iter()
                .map(|artifact| ConfigFile {
                    path: plugin_path_under(&self.home, artifact),
                    contents: artifact.contents().to_owned(),
                }),
        );
        Ok(LaunchPlan {
            args: Vec::new(),
            env: vec![
                (HERMES_HOME_ENV.to_owned(), self.home.display().to_string()),
                (
                    crate::plugin::GATEWAY_URL_ENV.to_owned(),
                    self.endpoint.as_str().to_owned(),
                ),
            ],
            config_files,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const HOME: &str = "/tmp/tapes-hermes-home-XXXX";

    fn recipe() -> HermesRecipe {
        HermesRecipe::new(HOME, ProxyEndpoint::new("http://127.0.0.1:9/v1"))
    }

    fn document(plan: &LaunchPlan) -> Value {
        serde_json::from_str(&plan.config_files[0].contents).unwrap()
    }

    #[test]
    fn harness_id_is_hermes() {
        assert_eq!(recipe().harness(), "hermes");
    }

    /// The redirect: a `custom` provider at the endpoint, verbatim.
    #[test]
    fn plan_points_the_custom_provider_at_the_endpoint() {
        let plan = recipe().with_model("mock-model").plan().unwrap();
        let doc = document(&plan);
        assert_eq!(doc["model"]["provider"], "custom");
        assert_eq!(doc["model"]["base_url"], "http://127.0.0.1:9/v1");
        assert_eq!(doc["model"]["default"], "mock-model");
        assert!(doc["model"].get("api_key").is_none(), "no key → no api_key");
        assert!(doc["model"].get("api_mode").is_none());
        // The static floor for auxiliary calls the plugin cannot reach.
        assert_eq!(
            doc["model"]["default_headers"],
            json!({ "x-tapes-harness-id": "hermes" })
        );
        assert_eq!(
            plan.config_files[0].path,
            PathBuf::from(HOME).join("config.yaml")
        );
        assert!(plan.args.is_empty());
    }

    #[test]
    fn plan_writes_a_supplied_key_and_api_mode() {
        let plan = recipe()
            .with_api_key("sk-not-real")
            .with_api_mode("anthropic_messages")
            .plan()
            .unwrap();
        let doc = document(&plan);
        assert_eq!(doc["model"]["api_key"], "sk-not-real");
        assert_eq!(doc["model"]["api_mode"], "anthropic_messages");
    }

    /// Nothing in the planned config routes around the proxy.
    #[test]
    fn plan_declares_no_route_around_capture() {
        let doc = document(&recipe().plan().unwrap());
        assert_eq!(doc["providers"], json!({}));
        assert_eq!(doc["fallback_providers"], json!([]));
        assert!(doc.get("custom_providers").is_none());
        assert!(doc.get("delegation").is_none());
        assert!(doc.get("auxiliary").is_none());
    }

    /// The plugin is enabled under the name its manifest declares — the one
    /// hermes's `plugins.enabled` allow-list is keyed on.
    #[test]
    fn plan_enables_the_capture_plugin_it_writes() {
        let doc = document(&recipe().plan().unwrap());
        assert_eq!(
            doc["plugins"]["enabled"],
            json!([crate::plugin::HERMES_GATEWAY_PLUGIN_NAME])
        );
    }

    /// The regression the plugin files exist for, carried over from opencode:
    /// a relocated home moves the user plugin directory, so the plan must carry
    /// the plugin itself, byte for byte the crate's artifact.
    #[test]
    fn plan_carries_the_capture_plugin_into_the_relocated_home() {
        let plan = recipe().plan().unwrap();
        let expected = [
            (
                PathBuf::from(HOME).join("plugins/tapes-gateway/__init__.py"),
                crate::plugin::HERMES_GATEWAY_PLUGIN.contents(),
            ),
            (
                PathBuf::from(HOME).join("plugins/tapes-gateway/plugin.yaml"),
                crate::plugin::HERMES_GATEWAY_MANIFEST.contents(),
            ),
        ];
        for (path, contents) in expected {
            let file = plan
                .config_files
                .iter()
                .find(|file| file.path == path)
                .unwrap_or_else(|| panic!("the plan does not write {}", path.display()));
            assert_eq!(file.contents, contents);
        }
        assert_eq!(recipe().plugin_paths().len(), 2);
    }

    /// The plan's plugin directory and the artifact's own install directory
    /// are one location against two roots; `.hermes` is exactly the component
    /// `HERMES_HOME` replaces.
    #[test]
    fn the_plans_plugin_directory_is_the_artifacts_own_directory() {
        for artifact in crate::plugin::HERMES_ARTIFACTS {
            assert_eq!(artifact.install_dir_components()[0], ".hermes");
            let installed = artifact.install_path(Path::new("/h"));
            let planned = plugin_path_under(Path::new("/h/.hermes"), artifact);
            assert_eq!(installed, planned);
        }
    }

    #[test]
    fn plan_relocates_the_home_and_names_the_gateway() {
        let plan = recipe().plan().unwrap();
        assert_eq!(
            plan.env,
            vec![
                ("HERMES_HOME".to_owned(), HOME.to_owned()),
                (
                    "TAPES_GATEWAY_URL".to_owned(),
                    "http://127.0.0.1:9/v1".to_owned()
                ),
            ]
        );
    }

    /// Every file lives under the root the consumer owns and deletes.
    #[test]
    fn every_file_a_plan_writes_lives_under_the_home_the_consumer_owns() {
        let plan = recipe().plan().unwrap();
        assert_eq!(plan.config_files.len(), 3);
        for file in &plan.config_files {
            assert!(
                file.path.starts_with(HOME),
                "{:?} escapes the home",
                file.path
            );
        }
    }

    /// JSON is the YAML hermes reads; it must at least be valid JSON, and
    /// byte-stable so a plan can be diffed.
    #[test]
    fn plan_is_byte_stable_json() {
        let first = recipe().plan().unwrap();
        let second = recipe().plan().unwrap();
        assert_eq!(
            first.config_files[0].contents,
            second.config_files[0].contents
        );
        assert!(document(&first).is_object());
    }
}
