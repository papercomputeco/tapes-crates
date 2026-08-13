//! opencode launch recipe.
//!
//! opencode is the one harness with no base-URL environment variable at all: it
//! reads provider endpoints out of a JSON config file, so redirecting it to a
//! capture proxy means writing that file and relocating the config directory it
//! is read from.
//!
//! Ported from the Go `tapes start opencode` arm in `cmd/tapes/start/start.go`
//! (`configureOpenCode`, `configureOpenCodeProvider`, `openCodeProviderMetas`,
//! `loadUserOpenCodeConfig`). The daemon client this crate also draws from never
//! supported opencode, so unlike
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
//! # Why the plan also carries the capture plugin
//!
//! Relocating the config root moves opencode's *plugin* directory too, and that
//! turned the two capture roads into a trap. A consumer that ran
//! `plugin install opencode` — writing
//! [`crate::plugin::OPENCODE_GATEWAY_EXTENSION`] to
//! `~/.config/opencode/plugins/` — and then launched through this recipe got a
//! session where the plugin was simply not on any path opencode scans. The
//! traffic was still redirected, so everything looked fine; the turns just
//! carried no `X-Tapes-*` envelope and filed as `harness_id: unknown`. Silent
//! unattribution is the worst available outcome, and combining the two obvious
//! steps produced it.
//!
//! So a plan writes the plugin into its own config root alongside the config
//! document. That is the only one of the three available fixes where the user
//! gets what they asked for:
//!
//! * The registry could have declared the two deliveries mutually exclusive,
//!   but a harness that can be captured both ways is a fact about opencode, and
//!   removing a capability to dodge an interaction is a poor trade.
//! * This recipe could have refused when the artifact was installed — except a
//!   recipe is pure and may not read the user's home, so it cannot see an
//!   install. Only the consumer could implement that check, which means every
//!   consumer would have to, and one that forgot would be back to the silent
//!   case.
//! * Writing the plugin needs neither: the artifact lives in this same crate,
//!   so its bytes and its directory are already known here, and a plan already
//!   carries files for the consumer to materialise and remove.
//!
//! An installed copy is not required and is not consulted — a plan is
//! self-contained, so `plugin install` is not a precondition for this road.
//! Nothing is imposed on a consumer that does not want capture from inside,
//! either, because the plugin is inert unless the launched environment sets
//! [`crate::plugin::GATEWAY_URL_ENV`]: written but dormant costs a file in a
//! directory the consumer is deleting anyway.
//!
//! When that variable *is* set, the plugin's `config` hook runs after opencode
//! loads this document and overwrites the captured providers' `baseURL` with
//! the gateway the environment names. That is the intended precedence — the
//! plugin is the only half that can attribute the session — but it does mean a
//! consumer pointing the two halves at different addresses gets the plugin's.
//! Setting both is a deliberate act; the failure it produces is a request to a
//! route that answers or does not, which is loud, and not a session that
//! captures perfectly and belongs to nobody.
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

/// The `X-Tapes-Harness-Id` value for opencode traffic, taken from the
/// registry so the recipe and the declaration cannot disagree.
const HARNESS_ID: &str = crate::harness::OPENCODE.id();

/// The environment variable whose relocation redirects opencode's config lookup.
///
/// opencode has no dedicated config-path variable; it resolves
/// `$XDG_CONFIG_HOME/opencode/opencode.json`. See the module docs for the
/// blast-radius caveat that comes with overriding it.
pub const OPENCODE_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// Path of opencode's config file relative to the config root.
const CONFIG_RELATIVE_PATH: [&str; 2] = ["opencode", "opencode.json"];

/// Directory, relative to the config root, that opencode auto-discovers plugins
/// from.
///
/// This is [`crate::plugin::OPENCODE_GATEWAY_EXTENSION`]'s own install directory
/// with its leading `.config` component removed — that component is exactly what
/// [`OPENCODE_CONFIG_HOME_ENV`] replaces. The two are asserted equal in this
/// module's tests, so relocating one without the other fails the build rather
/// than producing a plan that installs the plugin somewhere opencode will not
/// look.
const PLUGIN_RELATIVE_DIR: [&str; 2] = ["opencode", "plugins"];

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

    /// Absolute path the plan writes the capture plugin to.
    ///
    /// See the module docs on why a plan carries the plugin at all.
    pub fn plugin_path(&self) -> PathBuf {
        PLUGIN_RELATIVE_DIR
            .iter()
            .fold(self.config_root.clone(), |path, segment| path.join(segment))
            .join(crate::plugin::OPENCODE_GATEWAY_EXTENSION.file_name())
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
            config_files: vec![
                ConfigFile {
                    path: self.config_path(),
                    contents,
                },
                // The capture plugin travels with the relocated config root.
                // See the module docs: without this, an installed plugin is
                // invisible to a recipe-launched session.
                ConfigFile {
                    path: self.plugin_path(),
                    contents: crate::plugin::OPENCODE_GATEWAY_EXTENSION
                        .contents()
                        .to_owned(),
                },
            ],
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
        // The config document is first; the capture plugin rides beside it.
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

    /// The regression this plan's second file exists for: relocating the config
    /// root also relocates opencode's plugin directory, so a plugin installed in
    /// the user's home is invisible to a recipe-launched session. Redirected but
    /// unattributed is the failure that produced — and it is silent, which is
    /// why the plugin travels with the root rather than being required in it.
    #[test]
    fn plan_carries_the_capture_plugin_into_the_relocated_config_root() {
        let plan = recipe().plan().unwrap();
        let plugin = plan
            .config_files
            .iter()
            .find(|file| file.path == recipe().plugin_path())
            .expect("the plan does not carry the capture plugin");
        assert_eq!(
            plugin.path,
            PathBuf::from("/tmp/tapes-opencode-config-XXXX/opencode/plugins/tapes-gateway.ts"),
        );
        // The bytes are the crate's own artifact, not a copy that could drift.
        assert_eq!(
            plugin.contents,
            crate::plugin::OPENCODE_GATEWAY_EXTENSION.contents(),
        );
    }

    /// The plan's plugin destination and the artifact's own install destination
    /// are one location expressed against two roots: `~` for an installer, and
    /// the relocated config home for a plan. `.config` is precisely the
    /// component [`OPENCODE_CONFIG_HOME_ENV`] replaces, so the artifact's
    /// components must be that marker followed by this module's relative
    /// directory. Moving either alone would put a plan's plugin somewhere
    /// opencode does not scan — the exact silent failure above, reintroduced.
    #[test]
    fn the_plans_plugin_directory_is_the_artifacts_own_directory() {
        let installed = crate::plugin::OPENCODE_GATEWAY_EXTENSION.install_dir_components();
        let expected: Vec<&str> = std::iter::once(".config")
            .chain(PLUGIN_RELATIVE_DIR.iter().copied())
            .collect();
        assert_eq!(
            installed, expected,
            "the artifact installs to {installed:?} but a plan writes it to \
             <config-root>/{PLUGIN_RELATIVE_DIR:?}",
        );
    }

    /// A plan is self-contained: the plugin it writes is the whole delivery, so
    /// `plugin install` is not a precondition for this road, and a consumer that
    /// removes the config root removes the plugin with it.
    #[test]
    fn every_file_a_plan_writes_lives_under_the_root_the_consumer_owns() {
        let recipe = recipe();
        let plan = recipe.plan().unwrap();
        assert_eq!(plan.config_files.len(), 2);
        for file in &plan.config_files {
            assert!(
                file.path.starts_with("/tmp/tapes-opencode-config-XXXX"),
                "{:?} escapes the config root the consumer created and deletes",
                file.path,
            );
        }
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
