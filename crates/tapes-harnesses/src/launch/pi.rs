//! pi launch recipe for a gateway-served model.
//!
//! pi needs no recipe to be captured: its built-in `anthropic`, `openai` and
//! `openai-codex` providers are redirected by the capture extension
//! ([`crate::plugin::PI_GATEWAY_EXTENSION`]) from the environment alone. What
//! the extension cannot do is make pi *know* a model it has never heard of. A
//! gateway backend serving a self-hosted OpenAI-compatible model has an id in no
//! catalog pi ships, and pi's `openai` provider speaks the Responses API by
//! default, which such a backend usually does not. Without a recipe the user
//! has to hand-write the model into `models.json` under `openai`, with
//! `api: "openai-completions"`.
//!
//! This recipe writes that entry, for one launch, without touching the user's
//! own `models.json`.
//!
//! # Why a relocated agent directory
//!
//! pi reads `models.json` from its agent directory and offers no flag naming a
//! different file. The only way to give one session a model the user's config
//! lacks is to point [`PI_AGENT_DIR_ENV`] at a directory the consumer chose and
//! plan the file there.
//!
//! That variable relocates the *whole* agent directory, so the plan has to put
//! back what a session needs from it:
//!
//! * **The capture extension.** pi auto-discovers extensions from
//!   `<agent-dir>/extensions/`, so a relocated session no longer loads the copy
//!   installed under the user's home. The plan writes the crate's asset there,
//!   byte for byte, which also makes it the only copy loaded — no
//!   `--extension`, no second reader.
//! * **Preferences.** [`PI_CARRIED_FILES`] names the files a consumer may copy
//!   from the user's own agent directory with [`PiRecipe::with_carried_file`]:
//!   settings, keybindings, and the user-level instruction files. All are the
//!   user's own preferences and none holds a credential.
//!
//! What is deliberately *not* carried: `auth.json` (saved keys and OAuth
//! tokens — the declared model's credential is the gateway's business, and
//! pi's other providers still read their usual environment variables),
//! `mcp.json` and `mcp-auth.json` (server configuration and tokens), the user's
//! own `models.json` (its providers are exactly the endpoints that route
//! around capture), and other extensions. Sessions are written beneath the
//! planned directory; a consumer that keeps the directory between launches keeps
//! them, and one that uses a temporary directory starts fresh each time.
//!
//! # The planned model
//!
//! Each model passed to [`PiRecipe::with_model`] is declared under pi's `openai`
//! provider — one of the providers the extension routes — with
//! `api: "openai-completions"` and a model-level `baseUrl` at the proxy. The
//! extension's registration of `openai` already points the provider there, so
//! the model-level URL is a floor: should the extension fail to load, the
//! model's requests still go to the proxy rather than to the provider's public
//! endpoint. A `models.json` entry *adds* to pi's built-in `openai` models
//! rather than replacing them, except for an id pi already ships, which it
//! replaces for the session.
//!
//! pi lists a model only when its provider has a credential. When the consumer
//! has none to offer it passes [`PLACEHOLDER_API_KEY`], a fixed non-secret
//! string; one that wants the user's own key forwarded can pass a pi
//! environment reference such as `$OPENAI_API_KEY`, which pi resolves at
//! request time and which keeps the key itself off disk. The value is the
//! provider's, so it applies to pi's built-in `openai` models in the session
//! too.
//!
//! The plan's argv selects the first declared model, as `--model
//! openai/<id>`; a user flag later on the command line still wins.
//!
//! The nonce, the gateway URL, the schema and the routes switch stay the
//! consumer's to set, exactly as for a launch without this recipe.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use snafu::ResultExt;

use super::{ConfigFile, LaunchError, LaunchPlan, LaunchRecipe, ProxyEndpoint, launch_error};

/// The `X-Tapes-Harness-Id` value for pi traffic, taken from the registry.
const HARNESS_ID: &str = crate::harness::PI.id();

/// The environment variable pi resolves its agent directory from.
pub const PI_AGENT_DIR_ENV: &str = "PI_CODING_AGENT_DIR";

/// pi's model configuration document, relative to its agent directory.
const MODELS_FILE_NAME: &str = "models.json";

/// The pi provider a declared model is planned under. One of the providers the
/// capture extension redirects, so the model needs no route of its own.
pub const PI_GATEWAY_MODEL_PROVIDER: &str = "openai";

/// The wire API a declared model speaks: Chat Completions, which an
/// OpenAI-compatible backend serves, rather than the Responses API pi's
/// `openai` provider defaults to.
pub const PI_GATEWAY_MODEL_API: &str = "openai-completions";

/// A non-secret credential that makes pi list a declared model when the
/// consumer has no key to offer. pi will not offer a model whose provider has
/// no credential; the capture proxy, or the backend behind it, owns the real
/// one.
pub const PLACEHOLDER_API_KEY: &str = "tapes-gateway";

/// Files a consumer may copy from the user's own agent directory into the
/// planned one. Preferences and instructions only — nothing here holds a
/// credential or names an endpoint.
pub const PI_CARRIED_FILES: &[&str] = &[
    "settings.json",
    "keybindings.json",
    "AGENTS.md",
    "AGENTS.override.md",
    "CLAUDE.md",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

/// pi's thinking-level names, which a `--model` pattern may carry as a
/// `:<level>` suffix.
const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Launch pi with gateway-served models declared for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiRecipe {
    agent_dir: PathBuf,
    endpoint: ProxyEndpoint,
    provider_routes: bool,
    models: Vec<String>,
    api_key: Option<String>,
    carried: Vec<(String, String)>,
}

impl PiRecipe {
    /// Plan a pi agent directory at `agent_dir` whose declared models are
    /// served at `endpoint` — the same proxy address the consumer sets
    /// [`crate::plugin::GATEWAY_URL_ENV`] to.
    ///
    /// The consumer creates `agent_dir` and owns its lifetime; it must not be
    /// the user's own agent directory, whose `models.json` the plan would
    /// overwrite.
    pub fn new(agent_dir: impl Into<PathBuf>, endpoint: ProxyEndpoint) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            endpoint,
            provider_routes: false,
            models: Vec::new(),
            api_key: None,
            carried: Vec::new(),
        }
    }

    /// Whether the proxy serves each provider on its own route — the same
    /// switch as [`crate::plugin::GATEWAY_PROVIDER_ROUTES_ENV`]. On, a declared
    /// model's URL carries the `openai` route label, as the extension's own
    /// registration does.
    pub fn with_provider_routes(mut self, provider_routes: bool) -> Self {
        self.provider_routes = provider_routes;
        self
    }

    /// Declare a model id the gateway serves. Called more than once, each id
    /// is declared and the first is selected. Use [`pi_gateway_model`] to turn
    /// a user's `--model` pattern into an id.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.models.push(model.into());
        self
    }

    /// The `openai` provider credential written to the planned `models.json`,
    /// verbatim — a literal, or a pi environment reference (`$NAME`). See the
    /// module docs; [`PLACEHOLDER_API_KEY`] when the consumer has none. A plan
    /// carrying a literal key should be materialised private (`0o600`).
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Carry one of [`PI_CARRIED_FILES`] from the user's agent directory into
    /// the planned one. The consumer reads it — see [`pi_user_agent_dir`] for
    /// where — and the plan writes it. Any other name fails the plan.
    pub fn with_carried_file(
        mut self,
        name: impl Into<String>,
        contents: impl Into<String>,
    ) -> Self {
        self.carried.push((name.into(), contents.into()));
        self
    }

    /// Absolute path the plan writes `models.json` to.
    pub fn models_path(&self) -> PathBuf {
        self.agent_dir.join(MODELS_FILE_NAME)
    }

    /// Absolute path the plan writes the capture extension to.
    pub fn extension_path(&self) -> PathBuf {
        extension_path_under(&self.agent_dir)
    }

    fn model_url(&self) -> String {
        if self.provider_routes {
            format!(
                "{}{}",
                self.endpoint,
                crate::plugin::provider_route(PI_GATEWAY_MODEL_PROVIDER)
            )
        } else {
            self.endpoint.as_str().to_owned()
        }
    }

    fn document(&self) -> Value {
        let url = self.model_url();
        let models: Vec<Value> = self
            .models
            .iter()
            .map(|id| {
                json!({
                    "id": id,
                    "name": id,
                    "api": PI_GATEWAY_MODEL_API,
                    "baseUrl": url,
                })
            })
            .collect();
        let mut provider = Map::new();
        if let Some(api_key) = &self.api_key {
            provider.insert("apiKey".to_owned(), json!(api_key));
        }
        provider.insert("models".to_owned(), Value::Array(models));
        json!({ "providers": { PI_GATEWAY_MODEL_PROVIDER: Value::Object(provider) } })
    }
}

/// Where the capture extension lands beneath a relocated agent directory: its
/// install directory with the leading `.pi/agent` — the components
/// [`PI_AGENT_DIR_ENV`] replaces — dropped.
fn extension_path_under(agent_dir: &Path) -> PathBuf {
    let artifact = &crate::plugin::PI_GATEWAY_EXTENSION;
    artifact
        .install_dir_components()
        .iter()
        .skip(2)
        .fold(agent_dir.to_path_buf(), |path, component| {
            path.join(component)
        })
        .join(artifact.file_name())
}

/// The user's own pi agent directory: `configured` (the value of
/// [`PI_AGENT_DIR_ENV`] in the environment the consumer inherited) when it is
/// set and non-empty, otherwise `.pi/agent` beneath `home`. Where a consumer
/// reads [`PI_CARRIED_FILES`] from; never a place a plan writes.
pub fn pi_user_agent_dir(home: &Path, configured: Option<&OsStr>) -> PathBuf {
    match configured.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".pi").join("agent"),
    }
}

/// The gateway model id a pi `--model` pattern names, if it names one.
///
/// pi patterns are `[provider/]id[:thinking]`. The thinking suffix is dropped
/// (only a known level — `qwen2.5-coder:7b` keeps its tag) and an `openai/`
/// prefix is removed. A pattern for another of the providers the extension
/// routes (`anthropic/…`, `openai-codex/…`) names one of pi's own models and
/// is `None`, as is a blank one. Any other prefix is part of the id:
/// `deepseek-ai/DeepSeek-V3` is a model id, not a provider.
pub fn pi_gateway_model(pattern: &str) -> Option<String> {
    let pattern = pattern.trim();
    let pattern = match pattern.rsplit_once(':') {
        Some((head, level)) if THINKING_LEVELS.contains(&level) => head,
        _ => pattern,
    };
    let id = match pattern.split_once('/') {
        Some((provider, rest)) if provider == PI_GATEWAY_MODEL_PROVIDER => rest,
        Some((provider, _)) if crate::plugin::pi::ROUTED_PROVIDERS.contains(&provider) => {
            return None;
        }
        _ => pattern,
    };
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

impl LaunchRecipe for PiRecipe {
    fn harness(&self) -> &str {
        HARNESS_ID
    }

    fn plan(&self) -> Result<LaunchPlan, LaunchError> {
        if let Some(model) = self.models.iter().find(|model| model.trim().is_empty()) {
            return launch_error::InvalidPiModelSnafu {
                model: model.clone(),
            }
            .fail();
        }
        let contents = serde_json::to_string_pretty(&self.document())
            .context(launch_error::SerializePiConfigSnafu)?;
        let mut config_files = vec![
            ConfigFile {
                path: self.models_path(),
                contents,
            },
            ConfigFile {
                path: self.extension_path(),
                contents: crate::plugin::PI_GATEWAY_EXTENSION.contents().to_owned(),
            },
        ];
        for (name, contents) in &self.carried {
            if !PI_CARRIED_FILES.contains(&name.as_str()) {
                return launch_error::UncarriedPiFileSnafu { name: name.clone() }.fail();
            }
            config_files.push(ConfigFile {
                path: self.agent_dir.join(name),
                contents: contents.clone(),
            });
        }
        let args = self
            .models
            .first()
            .map(|model| {
                vec![
                    "--model".to_owned(),
                    format!("{PI_GATEWAY_MODEL_PROVIDER}/{model}"),
                ]
            })
            .unwrap_or_default();
        Ok(LaunchPlan {
            args,
            env: vec![(
                PI_AGENT_DIR_ENV.to_owned(),
                self.agent_dir.display().to_string(),
            )],
            config_files,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const DIR: &str = "/tmp/tapes-pi-agent-XXXX";

    fn recipe() -> PiRecipe {
        PiRecipe::new(DIR, ProxyEndpoint::new("http://127.0.0.1:9"))
    }

    fn document(plan: &LaunchPlan) -> Value {
        serde_json::from_str(&plan.config_files[0].contents).unwrap()
    }

    #[test]
    fn harness_id_is_pi() {
        assert_eq!(recipe().harness(), "pi");
    }

    /// The fix for the hand-written `models.json`: the model under `openai`,
    /// speaking Chat Completions, at the proxy.
    #[test]
    fn plan_declares_the_model_under_openai_speaking_chat_completions() {
        let plan = recipe()
            .with_model("self-hosted-v4")
            .with_api_key(PLACEHOLDER_API_KEY)
            .plan()
            .unwrap();
        assert_eq!(
            document(&plan),
            json!({ "providers": { "openai": {
                "apiKey": "tapes-gateway",
                "models": [{
                    "id": "self-hosted-v4", "name": "self-hosted-v4",
                    "api": "openai-completions", "baseUrl": "http://127.0.0.1:9",
                }],
            }}})
        );
        assert_eq!(
            plan.config_files[0].path,
            PathBuf::from(DIR).join("models.json")
        );
        assert_eq!(plan.args, vec!["--model", "openai/self-hosted-v4"]);
    }

    #[test]
    fn without_a_key_the_provider_names_none() {
        let doc = document(&recipe().with_model("m").plan().unwrap());
        assert!(doc["providers"]["openai"].get("apiKey").is_none());
    }

    #[test]
    fn every_model_is_declared_and_the_first_selected() {
        let plan = recipe().with_model("a").with_model("b/c").plan().unwrap();
        let ids: Vec<_> = document(&plan)["providers"]["openai"]["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, ["a", "b/c"]);
        assert_eq!(plan.args, vec!["--model", "openai/a"]);
    }

    /// The model's URL carries the same label the extension's own `openai`
    /// registration does, so the floor and the redirect agree.
    #[test]
    fn provider_routes_label_the_model_url_as_the_extension_does() {
        let doc = document(
            &recipe()
                .with_provider_routes(true)
                .with_model("m")
                .plan()
                .unwrap(),
        );
        assert_eq!(
            doc["providers"]["openai"]["models"][0]["baseUrl"],
            format!(
                "http://127.0.0.1:9{}",
                crate::plugin::provider_route("openai")
            )
        );
    }

    #[test]
    fn plan_relocates_the_agent_dir_and_carries_the_extension() {
        let plan = recipe().with_model("m").plan().unwrap();
        assert_eq!(
            plan.env,
            vec![("PI_CODING_AGENT_DIR".to_owned(), DIR.to_owned())]
        );
        let extension = plan
            .config_files
            .iter()
            .find(|file| file.path == PathBuf::from(DIR).join("extensions/tapes-gateway.ts"))
            .expect("the plan does not carry the capture extension");
        assert_eq!(
            extension.contents,
            crate::plugin::PI_GATEWAY_EXTENSION.contents()
        );
    }

    /// The planned extension directory and the artifact's install directory
    /// are one location against two roots; `.pi/agent` is exactly what the
    /// variable replaces.
    #[test]
    fn the_planned_extension_directory_is_the_artifacts_own_directory() {
        let artifact = &crate::plugin::PI_GATEWAY_EXTENSION;
        assert_eq!(&artifact.install_dir_components()[..2], [".pi", "agent"]);
        assert_eq!(
            artifact.install_path(Path::new("/h")),
            extension_path_under(Path::new("/h/.pi/agent"))
        );
    }

    #[test]
    fn carried_files_are_written_and_nothing_else_may_be() {
        let plan = recipe()
            .with_model("m")
            .with_carried_file("settings.json", "{\"theme\":\"dark\"}")
            .plan()
            .unwrap();
        let settings = plan
            .config_files
            .iter()
            .find(|file| file.path == PathBuf::from(DIR).join("settings.json"))
            .unwrap();
        assert_eq!(settings.contents, "{\"theme\":\"dark\"}");
        for name in ["auth.json", "mcp.json", "models.json", "../settings.json"] {
            let error = recipe()
                .with_carried_file(name, "{}")
                .plan()
                .expect_err(name);
            assert!(
                matches!(error, LaunchError::UncarriedPiFile { .. }),
                "{name}"
            );
        }
        assert!(!PI_CARRIED_FILES.iter().any(|name| name.contains("auth")));
    }

    #[test]
    fn every_file_a_plan_writes_lives_under_the_agent_dir() {
        let mut planned = recipe().with_model("m");
        for name in PI_CARRIED_FILES {
            planned = planned.with_carried_file(*name, "x");
        }
        let plan = planned.plan().unwrap();
        assert_eq!(plan.config_files.len(), 2 + PI_CARRIED_FILES.len());
        for file in &plan.config_files {
            assert!(file.path.starts_with(DIR), "{:?} escapes", file.path);
        }
    }

    #[test]
    fn a_blank_model_fails_the_plan() {
        assert!(matches!(
            recipe().with_model(" ").plan(),
            Err(LaunchError::InvalidPiModel { .. })
        ));
    }

    #[test]
    fn gateway_model_patterns() {
        for (pattern, id) in [
            ("self-hosted-v4", Some("self-hosted-v4")),
            ("openai/self-hosted-v4", Some("self-hosted-v4")),
            ("openai/self-hosted-v4:high", Some("self-hosted-v4")),
            ("qwen2.5-coder:7b", Some("qwen2.5-coder:7b")),
            ("deepseek-ai/DeepSeek-V3", Some("deepseek-ai/DeepSeek-V3")),
            (
                "openai/deepseek-ai/DeepSeek-V3:off",
                Some("deepseek-ai/DeepSeek-V3"),
            ),
            ("anthropic/claude-sonnet-4-5", None),
            ("openai-codex/gpt-5.4", None),
            ("openai/", None),
            ("  ", None),
            (":high", None),
        ] {
            assert_eq!(pi_gateway_model(pattern).as_deref(), id, "{pattern:?}");
        }
    }

    #[test]
    fn the_user_agent_dir_honours_the_users_own_override() {
        assert_eq!(
            pi_user_agent_dir(Path::new("/home/u"), None),
            PathBuf::from("/home/u/.pi/agent")
        );
        assert_eq!(
            pi_user_agent_dir(Path::new("/home/u"), Some(OsStr::new(""))),
            PathBuf::from("/home/u/.pi/agent")
        );
        assert_eq!(
            pi_user_agent_dir(Path::new("/home/u"), Some(OsStr::new("/elsewhere"))),
            PathBuf::from("/elsewhere")
        );
    }

    #[test]
    fn plan_is_byte_stable() {
        let first = recipe().with_model("m").plan().unwrap();
        let second = recipe().with_model("m").plan().unwrap();
        assert_eq!(first, second);
    }
}
