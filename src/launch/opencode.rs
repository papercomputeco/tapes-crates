//! opencode launch recipe.
//!
//! opencode is the one harness with no base-URL environment variable at all: it
//! reads provider endpoints out of a JSON config file, so redirecting it to a
//! capture proxy means writing that file and relocating the config directory it
//! is read from.
//!
//! Ported from the Go `tapes start opencode` arm in `cmd/tapes/start/start.go`
//! (`configureOpenCode`, `configureOpenCodeProvider`, `openCodeProviderMetas`,
//! `loadUserOpenCodeConfig`). paper never supported opencode, so unlike
//! [`super::claude`] and [`super::codex`] this recipe has a single upstream
//! implementation — the knowledge here is a port, not a reconciliation of two.
//!
//! # How the redirect works
//!
//! opencode reads `$XDG_CONFIG_HOME/opencode/opencode.json`. The recipe plans a
//! config document plus an [`OPENCODE_CONFIG_HOME_ENV`] override pointing at a
//! root the consumer chose, which is how the Go implementation did it: build a
//! private config tree, point the child at it, delete it on exit.
//!
//! **This is a blunt instrument.** `XDG_CONFIG_HOME` is not opencode's variable,
//! it is the whole XDG spec's, so a relocated value is also seen by anything
//! opencode itself shells out to. Starting the plan's document from the user's
//! existing `opencode.json` (see
//! [`with_base_config`](OpenCodeRecipe::with_base_config)) keeps opencode's own
//! settings intact, but any other tool reading `XDG_CONFIG_HOME` inside that
//! process tree will find a directory containing only opencode's config. The Go
//! implementation shipped with this caveat and it is preserved rather than
//! silently changed; a narrower mechanism would be a behaviour change, not a
//! move.
//!
//! # Provider endpoints and `/v1`
//!
//! opencode's provider adapters are AI SDK adapters, and they append only the
//! *endpoint* name to `options.baseURL` — `/messages` for Anthropic,
//! `/chat/completions` for OpenAI-compatible providers. They therefore expect any
//! `/v1` component to already be part of the configured base URL. Whether a
//! given route needs it depends on what the proxy's upstream mapping already
//! includes, which makes it the consumer's call: pass a fully qualified
//! [`ProxyEndpoint`] per provider and this recipe writes it verbatim.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use snafu::ResultExt;

use super::{ConfigFile, LaunchError, LaunchPlan, LaunchRecipe, ProxyEndpoint, launch_error};

/// The `X-Tapes-Harness-Id` value for opencode traffic.
///
/// Not yet a named constant in [`crate::envelope`] — opencode capture arrives
/// with the standalone client — so it is spelled here and will move once the
/// envelope gains the constant.
const HARNESS_ID: &str = "opencode";

/// The environment variable whose relocation redirects opencode's config lookup.
///
/// opencode has no dedicated config-path variable; it resolves
/// `$XDG_CONFIG_HOME/opencode/opencode.json`. See the module docs for the
/// blast-radius caveat that comes with overriding it.
pub const OPENCODE_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// Path of opencode's config file relative to the config root.
const CONFIG_RELATIVE_PATH: [&str; 2] = ["opencode", "opencode.json"];

/// JSON object key holding opencode's provider table.
const PROVIDER_KEY: &str = "provider";
/// Top-level key holding opencode's persisted `provider/model` selection.
const MODEL_SELECTION_KEY: &str = "model";

/// The npm adapter package and display name opencode needs in order to recognise
/// a provider entry.
///
/// Ported from Go's `openCodeProviderMetas`. Only these three are known here
/// because only these three were known there; an unrecognised provider name is
/// still configurable — it simply gets no defaults filled in, exactly as in Go.
const PROVIDER_METAS: &[(&str, &str, &str)] = &[
    ("anthropic", "@ai-sdk/anthropic", "Anthropic"),
    ("openai", "@ai-sdk/openai", "OpenAI"),
    ("ollama", "@ai-sdk/openai-compatible", "Ollama"),
];

/// One provider entry to write into opencode's config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeProvider {
    /// The provider's opencode name (`"anthropic"`, `"openai"`, `"ollama"`, …).
    /// Used as the key in the config's provider table.
    pub name: String,
    /// Where this provider's traffic should go. Written to `options.baseURL`
    /// verbatim — see the module docs on `/v1`.
    pub endpoint: ProxyEndpoint,
    /// API key to write to `options.apiKey`, when the consumer has one.
    ///
    /// opencode runs its own auth flow, so an environment variable alone is not
    /// always enough — which is why the key can go in the config. A plan whose
    /// document carries a key should be materialised with private permissions
    /// (`0o600`), as the Go implementation did.
    pub api_key: Option<String>,
}

impl OpenCodeProvider {
    /// A provider entry with no API key.
    pub fn new(name: impl Into<String>, endpoint: ProxyEndpoint) -> Self {
        Self {
            name: name.into(),
            endpoint,
            api_key: None,
        }
    }

    /// Attach an API key to write into `options.apiKey`.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
}

/// Launch opencode against a capture proxy by planning its config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeRecipe {
    config_root: PathBuf,
    providers: Vec<OpenCodeProvider>,
    model: Option<String>,
    base_config: Option<Value>,
}

impl OpenCodeRecipe {
    /// Plan an opencode config under `config_root`, redirecting `providers`.
    ///
    /// `config_root` is the directory that [`OPENCODE_CONFIG_HOME_ENV`] will be
    /// set to; the plan's config file lands at
    /// `<config_root>/opencode/opencode.json`. The consumer creates that
    /// directory — conventionally a temporary one — and removes it when opencode
    /// exits.
    pub fn new(config_root: impl Into<PathBuf>, providers: Vec<OpenCodeProvider>) -> Self {
        Self {
            config_root: config_root.into(),
            providers,
            model: None,
            base_config: None,
        }
    }

    /// Pin the model opencode starts on, as `--model <provider>/<model>`.
    ///
    /// Worth doing for capture: only the provider entries this recipe redirected
    /// are captured, so a session that starts on some other provider produces no
    /// telemetry. The Go implementation both pinned the model and printed a
    /// warning that switching models inside opencode escapes capture; surfacing
    /// that warning is the consumer's call.
    ///
    /// No default model is supplied. The Go code carried a curated
    /// provider→default-model table, but a hardcoded model list rots on the
    /// vendors' release schedule rather than on this crate's, and picking a
    /// default is a product decision. The consumer names the model.
    pub fn with_model(mut self, provider: &str, model: &str) -> Self {
        self.model = Some(format!("{provider}/{model}"));
        self
    }

    /// Start the planned document from the user's existing opencode config
    /// instead of an empty object, so their unrelated settings survive.
    ///
    /// Read the file yourself — see [`opencode_user_config_candidates`] for
    /// where — and **read it before** applying the plan's
    /// [`OPENCODE_CONFIG_HOME_ENV`] override, or the candidate paths will resolve
    /// into the private root this recipe is planning.
    ///
    /// A non-object value is ignored: opencode's config is an object, and
    /// silently discarding a malformed base is what the Go implementation did
    /// (its parse failure fell through to an empty map).
    pub fn with_base_config(mut self, base: Value) -> Self {
        self.base_config = Some(base);
        self
    }

    /// Absolute path the plan writes the config document to.
    pub fn config_path(&self) -> PathBuf {
        CONFIG_RELATIVE_PATH
            .iter()
            .fold(self.config_root.clone(), |path, segment| path.join(segment))
    }
}

impl LaunchRecipe for OpenCodeRecipe {
    fn harness(&self) -> &str {
        HARNESS_ID
    }

    fn plan(&self) -> Result<LaunchPlan, LaunchError> {
        let mut document = match self.base_config.clone() {
            Some(Value::Object(map)) => map,
            // Anything that is not a JSON object cannot be opencode's config;
            // start clean rather than fail the launch.
            _ => Map::new(),
        };
        if let Some(providers) = ensure_object(&mut document, PROVIDER_KEY) {
            for provider in &self.providers {
                configure_provider(providers, provider);
            }
            // A provider we did not route is a provider the proxy never
            // sees: selecting one of its models sends traffic straight to
            // its configured endpoint, invisible to capture. The generated
            // config must only offer routes that pass through the proxy,
            // so base-config providers we did not rewrite are dropped, not
            // carried over.
            providers.retain(|name, _| self.providers.iter().any(|p| p.name == *name));
        }
        // The persisted model selection ("provider/model") must not outlive
        // its provider: opencode fails provider resolution at startup when
        // the selection names a provider the prune removed. Dropping it
        // falls back to opencode's own default-model behavior over the
        // routed providers (and `--model` still wins when the recipe sets
        // one).
        if let Some(selected) = document.get(MODEL_SELECTION_KEY).and_then(Value::as_str)
            && let Some((provider_name, _)) = selected.split_once('/')
            && !self.providers.iter().any(|p| p.name == provider_name)
        {
            document.remove(MODEL_SELECTION_KEY);
        }

        let contents = serde_json::to_string_pretty(&Value::Object(document))
            .context(launch_error::SerializeOpenCodeConfigSnafu)?;

        let args = match &self.model {
            Some(model) => vec!["--model".to_string(), model.clone()],
            None => Vec::new(),
        };

        Ok(LaunchPlan {
            args,
            env: vec![(
                OPENCODE_CONFIG_HOME_ENV.to_string(),
                self.config_root.display().to_string(),
            )],
            config_files: vec![ConfigFile {
                path: self.config_path(),
                contents,
            }],
        })
    }
}

/// Write one provider entry, preserving anything the user already set.
///
/// Ported from Go's `configureOpenCodeProvider`: `npm` and `name` are filled only
/// when absent (opencode will not recognise an entry without them, but the user's
/// choice wins), while `options.baseURL` is always overwritten — redirecting it
/// is the entire point.
fn configure_provider(providers: &mut Map<String, Value>, provider: &OpenCodeProvider) {
    let Some(entry) = ensure_object(providers, &provider.name) else {
        return;
    };
    if let Some((_, npm, display)) = PROVIDER_METAS
        .iter()
        .find(|(name, _, _)| *name == provider.name)
    {
        entry
            .entry("npm".to_string())
            .or_insert_with(|| Value::String((*npm).to_string()));
        entry
            .entry("name".to_string())
            .or_insert_with(|| Value::String((*display).to_string()));
    }

    let Some(options) = ensure_object(entry, "options") else {
        return;
    };
    options.insert(
        "baseURL".to_string(),
        Value::String(provider.endpoint.as_str().to_string()),
    );
    if let Some(api_key) = &provider.api_key {
        options.insert("apiKey".to_string(), Value::String(api_key.clone()));
    }
}

/// Borrow `key` from `target` as a JSON object, replacing any non-object value.
///
/// Ported from Go's `ensureMap`. A non-object at that key is config opencode
/// could not have written, so overwriting it is the same recovery Go performed.
///
/// The `None` arm is unreachable — the value is an object by the time it is
/// borrowed back — but it is returned as an `Option` rather than unwrapped
/// because this crate denies panics. Callers treat it as "nothing to configure".
fn ensure_object<'a>(
    target: &'a mut Map<String, Value>,
    key: &str,
) -> Option<&'a mut Map<String, Value>> {
    let entry = target
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    entry.as_object_mut()
}

/// Where opencode looks for the user's own config, in precedence order.
///
/// Ported from Go's `loadUserOpenCodeConfig` candidate list. Reading is the
/// consumer's job — see
/// [`with_base_config`](OpenCodeRecipe::with_base_config), including the warning
/// about reading these *before* applying a planned
/// [`OPENCODE_CONFIG_HOME_ENV`] override.
pub fn opencode_user_config_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg) = std::env::var_os(OPENCODE_CONFIG_HOME_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        candidates.push(config_file_under(&xdg));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(config_file_under(&home.join(".config")));
    }
    candidates
}

fn config_file_under(root: &Path) -> PathBuf {
    CONFIG_RELATIVE_PATH
        .iter()
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

/// Path to opencode's credential file.
///
/// opencode stores auth at `$XDG_DATA_HOME/opencode/auth.json`, defaulting to
/// `~/.local/share/opencode/auth.json`. Returns `None` when neither can be
/// resolved.
///
/// This is a **read** helper. An OAuth entry in this file takes precedence over
/// the `options.apiKey` a plan writes, so a provider the user has logged into
/// interactively will ignore the planned credential. The Go implementation dealt
/// with that by deleting the relevant entries and restoring the file on exit —
/// effective, but it mutates a file the user owns and leaves their login altered
/// if the process dies mid-flight. The hazard is documented here rather than
/// automated; a consumer that hits it is better off telling the user to log out
/// of that provider than editing their credential store behind their back.
pub fn opencode_auth_file() -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))?;
    Some(data_home.join("opencode").join("auth.json"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn base_config_providers_that_were_not_routed_are_dropped() {
        // A retained provider is a selectable route the proxy never sees —
        // traffic to it would be invisible to capture.
        let base = serde_json::json!({
            "theme": "dark",
            "provider": {
                "mystery": { "options": { "baseURL": "https://direct.example.com/v1" } }
            }
        });
        let plan = recipe().with_base_config(base).plan().unwrap();
        let document = parse(&plan.config_files[0].contents);
        let providers = document["provider"].as_object().unwrap();
        assert!(
            !providers.contains_key("mystery"),
            "unrouted provider retained: {providers:?}"
        );
        assert_eq!(document["theme"], "dark");
    }

    #[test]
    fn model_selection_does_not_outlive_its_pruned_provider() {
        // A selection naming a pruned provider would make opencode fail
        // provider resolution at startup; it must fall with the provider.
        let base = serde_json::json!({
            "model": "mystery/secret-model",
            "provider": {
                "mystery": { "options": { "baseURL": "https://direct.example.com/v1" } }
            }
        });
        let plan = recipe().with_base_config(base).plan().unwrap();
        let document = parse(&plan.config_files[0].contents);
        assert!(
            document.get("model").is_none(),
            "stale selection retained: {:?}",
            document.get("model")
        );

        // A selection over a ROUTED provider survives.
        let base = serde_json::json!({ "model": "anthropic/claude-sonnet-4-6" });
        let plan = recipe().with_base_config(base).plan().unwrap();
        let document = parse(&plan.config_files[0].contents);
        assert_eq!(document["model"], "anthropic/claude-sonnet-4-6");
    }

    fn parse(contents: &str) -> Value {
        serde_json::from_str(contents).unwrap()
    }

    fn recipe() -> OpenCodeRecipe {
        OpenCodeRecipe::new(
            "/tmp/tapes-opencode-config-XXXX",
            vec![
                OpenCodeProvider::new(
                    "anthropic",
                    ProxyEndpoint::new("http://127.0.0.1:9/agents/opencode/providers/anthropic/v1"),
                )
                .with_api_key("sk-ant-not-real"),
                OpenCodeProvider::new(
                    "openai",
                    ProxyEndpoint::new("http://127.0.0.1:9/agents/opencode/providers/openai"),
                )
                .with_api_key("sk-not-real"),
                OpenCodeProvider::new(
                    "ollama",
                    ProxyEndpoint::new("http://127.0.0.1:9/agents/opencode/providers/ollama/v1"),
                ),
            ],
        )
    }

    /// Each provider entry carries its npm adapter, display name, and redirected
    /// `options.baseURL`; a supplied key lands in `options.apiKey` and an absent
    /// one leaves the field out entirely.
    ///
    /// Mirrors the Go `configureOpenCode` behaviour: same three providers, same
    /// per-provider endpoints (including openai's missing `/v1`, which is the
    /// caller's route decision), same adapter table.
    #[test]
    fn plan_writes_every_provider_entry() {
        let plan = recipe().plan().unwrap();
        assert_eq!(plan.config_files.len(), 1);
        let document = parse(&plan.config_files[0].contents);
        let providers = &document["provider"];

        assert_eq!(providers["anthropic"]["npm"], "@ai-sdk/anthropic");
        assert_eq!(providers["anthropic"]["name"], "Anthropic");
        assert_eq!(
            providers["anthropic"]["options"]["baseURL"],
            "http://127.0.0.1:9/agents/opencode/providers/anthropic/v1",
        );
        assert_eq!(
            providers["anthropic"]["options"]["apiKey"],
            "sk-ant-not-real"
        );

        assert_eq!(providers["openai"]["npm"], "@ai-sdk/openai");
        assert_eq!(providers["openai"]["name"], "OpenAI");
        assert_eq!(
            providers["openai"]["options"]["baseURL"],
            "http://127.0.0.1:9/agents/opencode/providers/openai",
        );

        assert_eq!(providers["ollama"]["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(providers["ollama"]["name"], "Ollama");
        assert!(
            providers["ollama"]["options"].get("apiKey").is_none(),
            "no key supplied → no apiKey field: {}",
            providers["ollama"]["options"],
        );
    }

    /// The config file lands where opencode looks for it, and the plan points
    /// opencode at that root.
    #[test]
    fn plan_places_the_config_where_opencode_reads_it() {
        let plan = recipe().plan().unwrap();
        assert_eq!(
            plan.config_files[0].path,
            PathBuf::from("/tmp/tapes-opencode-config-XXXX/opencode/opencode.json"),
        );
        assert_eq!(
            plan.env,
            vec![(
                "XDG_CONFIG_HOME".to_string(),
                "/tmp/tapes-opencode-config-XXXX".to_string(),
            )],
        );
    }

    /// A user's existing settings survive, and their explicit `npm` / `name`
    /// overrides win — but their stale `baseURL` does not, because redirecting it
    /// is the point.
    ///
    /// Mirrors Go's "fill only when absent" behaviour in
    /// `configureOpenCodeProvider`.
    #[test]
    fn plan_preserves_user_settings_but_overwrites_the_base_url() {
        let base = serde_json::json!({
            "theme": "gruvbox",
            "provider": {
                "anthropic": {
                    "npm": "@scoped/custom-adapter",
                    "name": "My Anthropic",
                    "options": {
                        "baseURL": "https://api.anthropic.com",
                        "timeout": 30
                    }
                },
                "unrelated": { "options": { "baseURL": "https://elsewhere" } }
            }
        });
        let plan = OpenCodeRecipe::new(
            "/tmp/root",
            vec![OpenCodeProvider::new(
                "anthropic",
                ProxyEndpoint::new("http://127.0.0.1:9/providers/anthropic/v1"),
            )],
        )
        .with_base_config(base)
        .plan()
        .unwrap();
        let document = parse(&plan.config_files[0].contents);

        assert_eq!(document["theme"], "gruvbox", "unrelated keys survive");
        let anthropic = &document["provider"]["anthropic"];
        assert_eq!(
            anthropic["npm"], "@scoped/custom-adapter",
            "the user's adapter choice wins",
        );
        assert_eq!(anthropic["name"], "My Anthropic");
        assert_eq!(
            anthropic["options"]["timeout"], 30,
            "sibling options survive"
        );
        assert_eq!(
            anthropic["options"]["baseURL"], "http://127.0.0.1:9/providers/anthropic/v1",
            "the redirect always wins",
        );
        assert!(
            document["provider"].get("unrelated").is_none(),
            "providers this recipe did not name must not survive: they are \
             selectable routes the capture proxy never sees",
        );
    }

    /// A base config that is not a JSON object is discarded rather than failing
    /// the launch — the same recovery Go's parse-failure path performed.
    #[test]
    fn plan_ignores_a_non_object_base_config() {
        let plan = OpenCodeRecipe::new(
            "/tmp/root",
            vec![OpenCodeProvider::new(
                "openai",
                ProxyEndpoint::new("http://127.0.0.1:9"),
            )],
        )
        .with_base_config(serde_json::json!(["not", "an", "object"]))
        .plan()
        .unwrap();
        let document = parse(&plan.config_files[0].contents);
        assert!(document["provider"]["openai"]["options"]["baseURL"].is_string());
    }

    /// A non-object sitting where an object belongs is replaced, not merged.
    /// Ported from Go's `ensureMap`, which did the same.
    #[test]
    fn plan_replaces_a_non_object_provider_table() {
        let plan = OpenCodeRecipe::new(
            "/tmp/root",
            vec![OpenCodeProvider::new(
                "openai",
                ProxyEndpoint::new("http://127.0.0.1:9"),
            )],
        )
        .with_base_config(serde_json::json!({"provider": "nonsense"}))
        .plan()
        .unwrap();
        let document = parse(&plan.config_files[0].contents);
        assert_eq!(
            document["provider"]["openai"]["options"]["baseURL"],
            "http://127.0.0.1:9",
        );
    }

    /// An unknown provider name is still configurable; it simply gets no adapter
    /// defaults, exactly as in Go.
    #[test]
    fn plan_configures_an_unknown_provider_without_defaults() {
        let plan = OpenCodeRecipe::new(
            "/tmp/root",
            vec![OpenCodeProvider::new(
                "my-gateway",
                ProxyEndpoint::new("http://127.0.0.1:9/v1"),
            )],
        )
        .plan()
        .unwrap();
        let entry = &parse(&plan.config_files[0].contents)["provider"]["my-gateway"];
        assert_eq!(entry["options"]["baseURL"], "http://127.0.0.1:9/v1");
        assert!(entry.get("npm").is_none(), "no adapter guess: {entry}");
        assert!(entry.get("name").is_none());
    }

    /// The model pin is `--model <provider>/<model>`, and absent unless asked
    /// for.
    #[test]
    fn plan_pins_the_model_only_when_requested() {
        assert!(recipe().plan().unwrap().args.is_empty());
        let plan = recipe()
            .with_model("anthropic", "claude-sonnet-4-5")
            .plan()
            .unwrap();
        assert_eq!(
            plan.args,
            vec![
                "--model".to_string(),
                "anthropic/claude-sonnet-4-5".to_string(),
            ],
        );
    }

    /// The planned document is byte-stable across runs. Go marshalled a
    /// `map[string]any`, whose key order was randomised; `serde_json`'s object
    /// map is ordered, so a plan can be diffed and asserted on.
    #[test]
    fn plan_is_byte_stable() {
        let first = recipe().plan().unwrap();
        let second = recipe().plan().unwrap();
        assert_eq!(
            first.config_files[0].contents,
            second.config_files[0].contents
        );
        assert!(
            first.config_files[0].contents.contains("\n  \""),
            "two-space indented, matching Go's MarshalIndent: {}",
            first.config_files[0].contents,
        );
    }

    /// The user-config candidates name opencode's conventional locations, XDG
    /// first.
    #[test]
    fn user_config_candidates_end_at_opencode_json() {
        for candidate in opencode_user_config_candidates() {
            assert!(
                candidate.ends_with("opencode/opencode.json"),
                "{candidate:?}"
            );
        }
    }

    #[test]
    fn auth_file_names_the_conventional_path() {
        if let Some(path) = opencode_auth_file() {
            assert!(path.ends_with("opencode/auth.json"), "{path:?}");
        }
    }

    #[test]
    fn harness_id_is_opencode() {
        assert_eq!(recipe().harness(), "opencode");
    }
}
