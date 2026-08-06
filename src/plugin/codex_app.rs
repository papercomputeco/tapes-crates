//! The Codex desktop app's hook-plugin manifests, as templates.
//!
//! Capturing the desktop app needs a Codex *plugin*: a manifest naming the
//! plugin plus a hooks file subscribing a command to the five lifecycle
//! boundaries [`crate::attribution::codex_app`] parses. Unlike pi's extension
//! (a fixed file, installed by copying), a Codex plugin is installed by
//! Codex's own plugin manager from a consumer-packaged source directory, and
//! two of its ingredients are irreducibly the consumer's:
//!
//! * **The hook command line.** Each hook runs an executable that receives
//!   the lifecycle payload on stdin and reports it to the consumer's local
//!   runtime. Which executable, and how it is located without depending on
//!   the app's `PATH`, is deployment knowledge — the branded launcher script
//!   paper ships is exactly the part that cannot live here.
//! * **The plugin's identity.** The name, description, and developer strings
//!   Codex shows the user must say who is actually asking for hook trust.
//!
//! So the crate ships the manifests as **templates**: the JSON structure and
//! the event set are crate-owned (and pinned against the attribution module's
//! event list), while the command and identity are slots the consumer fills
//! through [`render_hooks_manifest`] and [`render_plugin_manifest`]. Both
//! installers — paper's today, a tapesctl installer later — render the same
//! bytes around their own strings, which is the same anti-drift bargain the
//! pi asset struck, adapted to a plugin that cannot be vendor-complete.
//!
//! The templates carry no endpoint and read no environment: a hook plugin is
//! inert until the *rendered command* does something, so the inertness
//! obligation [`crate::plugin::GATEWAY_URL_ENV`] discharges for pi rests here
//! on the consumer's command instead.
//!
//! Rendered manifests are still not an *installed* plugin. [`manager`] owns
//! the rest: the marketplace wrapper that makes them installable, and the
//! `codex` CLI invocation that installs them.

pub mod manager;

/// Slot in [`HOOKS_MANIFEST_TEMPLATE`] that a consumer's hook command line
/// replaces. The slot is the entire JSON string value, so substitution is
/// JSON-escaped by [`render_hooks_manifest`]; a consumer never edits the
/// template text itself.
pub const HOOK_COMMAND_SLOT: &str = "__TAPES_HOOK_COMMAND__";

/// The hooks manifest template — `hooks/hooks.json` in the packaged plugin.
///
/// Structure is Codex's hook-file contract: one key per lifecycle event, each
/// holding a single registration with a single `type: "command"` hook whose
/// command is [`HOOK_COMMAND_SLOT`].
pub const HOOKS_MANIFEST_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/codex-app/hooks.json"
));

/// The plugin manifest template — `.codex-plugin/plugin.json` in the packaged
/// plugin. Identity fields are slots for [`HookPluginIdentity`]; the manifest
/// deliberately registers no tool, app, or skill, so an installed plugin is
/// hook-only by construction.
pub const PLUGIN_MANIFEST_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/codex-app/plugin.json"
));

/// The two manifests a consumer packages into its plugin source directory,
/// as the registry hands them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HookPluginTemplates {
    /// [`PLUGIN_MANIFEST_TEMPLATE`], destined for `.codex-plugin/plugin.json`.
    pub plugin_manifest: &'static str,
    /// [`HOOKS_MANIFEST_TEMPLATE`], destined for `hooks/hooks.json`.
    pub hooks_manifest: &'static str,
}

/// The Codex desktop app's manifest templates.
pub const CODEX_APP_TEMPLATES: HookPluginTemplates = HookPluginTemplates {
    plugin_manifest: PLUGIN_MANIFEST_TEMPLATE,
    hooks_manifest: HOOKS_MANIFEST_TEMPLATE,
};

/// The consumer-supplied identity a rendered plugin manifest presents to the
/// user in Codex's plugin UI.
///
/// All fields are plain strings; [`render_plugin_manifest`] JSON-escapes them,
/// so quotes and backslashes in any field are safe.
///
/// Build one with [`HookPluginIdentity::new`] and the `with_*` setters. The
/// fields stay public — reading and patching one is useful, and the fixture
/// oracle style elsewhere in the crate relies on it — but the type is
/// `#[non_exhaustive]`, so a struct literal only compiles inside this crate.
/// Without the constructor a downstream installer got E0639 and could not call
/// [`render_plugin_manifest`] at all, which is the whole public point of the
/// module.
///
/// # Examples
///
/// ```
/// use tapes_harnesses::plugin::codex_app::{HookPluginIdentity, render_plugin_manifest};
///
/// let identity = HookPluginIdentity::new("acme-codex", "0.1.0")
///     .with_display_name("Acme for Codex")
///     .with_developer_name("Acme");
/// let manifest = render_plugin_manifest(&identity);
/// assert!(!manifest.contains("__TAPES_"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HookPluginIdentity<'a> {
    /// The plugin id — what Codex records trust and enablement against.
    pub name: &'a str,
    /// Plugin version. Bumping it is how a consumer invalidates the app's
    /// cached copy of an installed plugin.
    pub version: &'a str,
    /// One-line description shown beside the plugin.
    pub description: &'a str,
    /// Display name in the plugin UI.
    pub display_name: &'a str,
    /// Short marketplace description.
    pub short_description: &'a str,
    /// Long marketplace description.
    pub long_description: &'a str,
    /// Developer/author name shown to the user granting hook trust.
    pub developer_name: &'a str,
}

impl<'a> HookPluginIdentity<'a> {
    /// A hook plugin's identity, from the two fields that carry meaning
    /// beyond presentation.
    ///
    /// `name` is what Codex records trust and enablement against, and
    /// `version` is how a consumer invalidates the app's cached copy of an
    /// installed plugin — get either wrong and an install misbehaves, so they
    /// are the arguments rather than defaults.
    ///
    /// The five remaining fields are strings Codex only *shows*, and each
    /// starts as `name`. That default is deliberate: every slot in
    /// [`PLUGIN_MANIFEST_TEMPLATE`] must be filled or a literal
    /// `__TAPES_PLUGIN_…` string appears in the user-facing plugin UI, so the
    /// worst outcome of a forgotten `with_*` call is a repetitive UI, never a
    /// blank field and never a leaked slot. Override each with its setter.
    #[must_use]
    pub const fn new(name: &'a str, version: &'a str) -> Self {
        Self {
            name,
            version,
            description: name,
            display_name: name,
            short_description: name,
            long_description: name,
            developer_name: name,
        }
    }

    /// Set the one-line description shown beside the plugin.
    #[must_use]
    pub const fn with_description(mut self, description: &'a str) -> Self {
        self.description = description;
        self
    }

    /// Set the display name shown in the plugin UI.
    #[must_use]
    pub const fn with_display_name(mut self, display_name: &'a str) -> Self {
        self.display_name = display_name;
        self
    }

    /// Set the short marketplace description.
    #[must_use]
    pub const fn with_short_description(mut self, short_description: &'a str) -> Self {
        self.short_description = short_description;
        self
    }

    /// Set the long marketplace description.
    #[must_use]
    pub const fn with_long_description(mut self, long_description: &'a str) -> Self {
        self.long_description = long_description;
        self
    }

    /// Set the developer/author name shown to the user granting hook trust.
    #[must_use]
    pub const fn with_developer_name(mut self, developer_name: &'a str) -> Self {
        self.developer_name = developer_name;
        self
    }

    /// The slot each field fills, paired with its value. One table so the
    /// render loop and the template-coverage test share a single spelling.
    fn slots(&self) -> [(&'static str, &str); 7] {
        [
            ("__TAPES_PLUGIN_NAME__", self.name),
            ("__TAPES_PLUGIN_VERSION__", self.version),
            ("__TAPES_PLUGIN_DESCRIPTION__", self.description),
            ("__TAPES_PLUGIN_DISPLAY_NAME__", self.display_name),
            ("__TAPES_PLUGIN_SHORT_DESCRIPTION__", self.short_description),
            ("__TAPES_PLUGIN_LONG_DESCRIPTION__", self.long_description),
            ("__TAPES_PLUGIN_DEVELOPER_NAME__", self.developer_name),
        ]
    }
}

/// Render the hooks manifest with the consumer's hook command line.
///
/// The command is substituted as a JSON string value, escaping included, so a
/// command containing quotes, backslashes (a Windows path), or `${...}`
/// expansions passes through byte-exact to Codex.
#[must_use]
pub fn render_hooks_manifest(hook_command: &str) -> String {
    render_slots(
        HOOKS_MANIFEST_TEMPLATE,
        &[(HOOK_COMMAND_SLOT, hook_command)],
    )
}

/// Render the plugin manifest with the consumer's identity strings.
#[must_use]
pub fn render_plugin_manifest(identity: &HookPluginIdentity) -> String {
    render_slots(PLUGIN_MANIFEST_TEMPLATE, &identity.slots())
}

/// Replace every quoted slot occurrence with its JSON-escaped value, in one
/// pass over the template.
///
/// Substitution targets `"__SLOT__"` including its quotes and emits a
/// complete JSON string literal, so escaping cannot be forgotten and a slot
/// can never be half-replaced inside a larger value. Single-pass is
/// load-bearing, not a micro-optimisation: only *template* text is ever
/// scanned for slots, and substituted values go straight to the output. A
/// sequential per-slot `replace` re-scans earlier insertions, so an identity
/// value that merely *contains* another slot's placeholder — pathological but
/// consumer-controlled — would itself get substituted. Here such a value
/// passes through verbatim (escaped), like every other value byte.
fn render_slots(template: &str, slots: &[(&str, &str)]) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    loop {
        // The earliest quoted slot in the remaining *template* text wins;
        // everything before it is emitted untouched.
        let next = slots
            .iter()
            .filter_map(|(slot, value)| {
                let quoted = format!("\"{slot}\"");
                rest.find(&quoted).map(|at| (at, quoted.len(), *value))
            })
            .min_by_key(|(at, ..)| *at);
        let Some((at, slot_len, value)) = next else {
            rendered.push_str(rest);
            return rendered;
        };
        rendered.push_str(&rest[..at]);
        rendered.push_str(&json_string_literal(value));
        rest = &rest[at + slot_len..];
    }
}

/// `value` as a complete JSON string literal, quotes included.
///
/// Hand-rolled rather than `serde_json::to_string` because that API returns a
/// `Result` this crate would have to pretend can fail; for a `&str` it cannot,
/// and the escaping rules (RFC 8259 §7: `"` and `\` escaped, control
/// characters as `\u00XX`) are small enough to state directly.
fn json_string_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('"');
    for character in value.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                literal.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::attribution::codex_app::LIFECYCLE_EVENTS;
    use std::collections::BTreeMap;

    fn identity() -> HookPluginIdentity<'static> {
        // Built through the public constructor, not a struct literal: this is
        // the shape a downstream installer is limited to, so the whole
        // template-coverage suite below runs against it.
        HookPluginIdentity::new("acme-codex", "0.1.0")
            .with_description("Keeps Codex connected to acmed.")
            .with_display_name("Acme for Codex")
            .with_short_description("Keep Codex connected to Acme.")
            .with_long_description("Forwards lifecycle metadata to local acmed.")
            .with_developer_name("Acme")
    }

    /// A bare `new` fills every presentation slot with the plugin name. The
    /// property that matters is not the choice of default but that no slot is
    /// left unfilled: an unset field must never render as an empty string or
    /// as a literal `__TAPES_…` placeholder in the plugin UI.
    #[test]
    fn a_minimal_identity_fills_every_slot_with_the_plugin_name() {
        let rendered = render_plugin_manifest(&HookPluginIdentity::new("bare-codex", "2.0.0"));
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert!(
            !rendered.contains("__TAPES_"),
            "a slot survived a minimal render: {rendered}"
        );
        assert_eq!(parsed["name"], "bare-codex");
        assert_eq!(parsed["version"], "2.0.0");
        assert_eq!(parsed["interface"]["displayName"], "bare-codex");
        assert_eq!(parsed["interface"]["developerName"], "bare-codex");
        assert_eq!(parsed["author"]["name"], "bare-codex");
    }

    /// Each setter reaches exactly one slot. Distinct values per field would
    /// pass even if two setters wrote the same slot, so assert the whole
    /// rendered mapping rather than one field at a time.
    #[test]
    fn each_setter_reaches_its_own_slot() {
        let identity = HookPluginIdentity::new("n", "v")
            .with_description("d")
            .with_display_name("dn")
            .with_short_description("sd")
            .with_long_description("ld")
            .with_developer_name("dev");

        assert_eq!(
            identity.slots().map(|(_, value)| value),
            ["n", "v", "d", "dn", "sd", "ld", "dev"],
        );
    }

    /// The shape Codex parses a hooks file into, mirrored here so the test
    /// fails if the template stops being a valid hook file rather than only
    /// if the JSON stops parsing.
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HookFile {
        hooks: BTreeMap<String, Vec<HookRegistration>>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HookRegistration {
        hooks: Vec<CommandHook>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CommandHook {
        #[serde(rename = "type")]
        kind: String,
        command: String,
    }

    /// The rendered hooks file subscribes the supplied command to exactly the
    /// lifecycle events the attribution module parses — the two ends of the
    /// hook contract, pinned to one list.
    #[test]
    fn the_rendered_hooks_manifest_subscribes_the_command_to_every_lifecycle_event() {
        let command = r#"/bin/sh "${PLUGIN_ROOT}/scripts/capture-hook""#;
        let rendered = render_hooks_manifest(command);
        let parsed: HookFile = serde_json::from_str(&rendered).unwrap();

        let mut events: Vec<&str> = parsed.hooks.keys().map(String::as_str).collect();
        let mut expected: Vec<&str> = LIFECYCLE_EVENTS.to_vec();
        events.sort_unstable();
        expected.sort_unstable();
        assert_eq!(events, expected);

        for (event, registrations) in &parsed.hooks {
            assert_eq!(registrations.len(), 1, "{event} has multiple registrations");
            assert_eq!(registrations[0].hooks.len(), 1);
            assert_eq!(registrations[0].hooks[0].kind, "command");
            assert_eq!(
                registrations[0].hooks[0].command, command,
                "{event}'s command did not survive rendering byte-exact"
            );
        }
    }

    /// Substituted values are output, not template: an identity value that
    /// contains — or *is* — another slot's placeholder must survive
    /// verbatim, not get substituted itself. The sharpest case is exact
    /// equality: the value's own JSON-literal quotes complete the quoted
    /// `"__SLOT__"` pattern, so a sequential per-slot `replace` re-scanning
    /// its earlier insertions would swap the name for the version. Values
    /// merely embedding the spelling ride along as regression cover.
    #[test]
    fn a_value_containing_another_slots_placeholder_survives_verbatim() {
        let mut identity = identity();
        identity.name = "__TAPES_PLUGIN_VERSION__";
        identity.long_description = "mentions __TAPES_PLUGIN_NAME__ in prose";
        let rendered = render_plugin_manifest(&identity);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(
            parsed["name"], "__TAPES_PLUGIN_VERSION__",
            "the name was re-substituted as if it were template text"
        );
        assert_eq!(
            parsed["interface"]["longDescription"],
            "mentions __TAPES_PLUGIN_NAME__ in prose"
        );
        // And the real slots still rendered normally around them.
        assert_eq!(parsed["version"], "0.1.0");
        assert_eq!(parsed["interface"]["displayName"], "Acme for Codex");
    }

    /// Same property on the hooks side: a command containing the command
    /// slot's own quoted spelling is emitted once, escaped, and the five
    /// real slots are the only things substituted.
    #[test]
    fn a_command_containing_the_slot_spelling_survives_verbatim() {
        let command = "run --note '\"__TAPES_HOOK_COMMAND__\"'";
        let rendered = render_hooks_manifest(command);
        let parsed: HookFile = serde_json::from_str(&rendered).unwrap();
        for registrations in parsed.hooks.values() {
            assert_eq!(registrations[0].hooks[0].command, command);
        }
    }

    /// The substitution is real JSON escaping, not text splicing: quotes and
    /// backslashes in the command round-trip through a JSON parse.
    #[test]
    fn rendering_escapes_the_command_as_a_json_string() {
        let command = "C:\\tools\\hook.exe --label \"two words\"\twith\ncontrol\u{1}chars";
        let rendered = render_hooks_manifest(command);
        let parsed: HookFile = serde_json::from_str(&rendered).unwrap();
        let registrations = parsed.hooks.get("Stop").unwrap();
        assert_eq!(registrations[0].hooks[0].command, command);
    }

    #[test]
    fn the_rendered_plugin_manifest_carries_the_identity_and_no_slots() {
        let rendered = render_plugin_manifest(&identity());
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(parsed["name"], "acme-codex");
        assert_eq!(parsed["version"], "0.1.0");
        assert_eq!(parsed["author"]["name"], "Acme");
        assert_eq!(parsed["interface"]["displayName"], "Acme for Codex");
        assert_eq!(parsed["interface"]["developerName"], "Acme");
        assert!(
            !rendered.contains("__TAPES_"),
            "an identity slot survived rendering: {rendered}"
        );
        // Hook-only by construction: the manifest points at no hooks path
        // override (default discovery finds hooks/hooks.json) and registers
        // no tool, app, or skill surface.
        for absent in ["hooks", "tools", "apps", "skills"] {
            assert!(
                parsed.get(absent).is_none(),
                "the manifest unexpectedly declares {absent:?}"
            );
        }
    }

    /// Every slot the identity fills exists in the template exactly once —
    /// except the developer name, which the manifest shows in two places —
    /// and no template carries a slot nothing fills. A drifted spelling
    /// would otherwise render a manifest with a literal `__TAPES_...` string
    /// in the user-facing plugin UI.
    #[test]
    fn identity_slots_and_template_slots_cover_each_other() {
        for (slot, _) in identity().slots() {
            assert!(
                PLUGIN_MANIFEST_TEMPLATE.contains(&format!("\"{slot}\"")),
                "template is missing slot {slot}"
            );
        }
        assert_eq!(
            PLUGIN_MANIFEST_TEMPLATE.matches("__TAPES_").count(),
            identity().slots().len() + 1, // developerName repeats author.name's slot
        );
        assert_eq!(
            HOOKS_MANIFEST_TEMPLATE.matches("__TAPES_").count(),
            LIFECYCLE_EVENTS.len(),
            "the hooks template must carry exactly one command slot per event"
        );
        assert!(HOOKS_MANIFEST_TEMPLATE.contains(&format!("\"{HOOK_COMMAND_SLOT}\"")));
    }

    /// The de-branding bar the pi asset set applies to these templates too:
    /// the crate-owned halves name no vendor. Branding enters only through
    /// the consumer's identity strings and command line.
    #[test]
    fn the_templates_carry_no_vendor_branding() {
        for template in [PLUGIN_MANIFEST_TEMPLATE, HOOKS_MANIFEST_TEMPLATE] {
            let lowered = template.to_ascii_lowercase();
            for token in ["paper", "papercompute"] {
                assert!(
                    !lowered.contains(token),
                    "a crate-owned template mentions {token:?}"
                );
            }
        }
    }

    /// Like the pi asset's no-built-in-endpoint rule: a template must not
    /// smuggle in a default destination. The only executable content in a
    /// rendered plugin is the consumer's command.
    #[test]
    fn the_templates_have_no_built_in_endpoint() {
        for template in [PLUGIN_MANIFEST_TEMPLATE, HOOKS_MANIFEST_TEMPLATE] {
            for literal in ["127.0.0.1:", "localhost:", "http://"] {
                assert!(
                    !template.contains(literal),
                    "a template hard-codes {literal:?}"
                );
            }
        }
    }

    /// The registry hands out these exact templates; a drifted copy would
    /// mean `find("codex-app")` and this module disagree about the bytes a
    /// consumer packages.
    #[test]
    fn the_registry_reaches_these_templates() {
        let harness = crate::harness::find("codex-app").expect("codex-app is registered");
        match harness.plugin() {
            crate::harness::PluginDelivery::HookManifestTemplates(templates) => {
                assert_eq!(*templates, CODEX_APP_TEMPLATES);
            }
            other => panic!("codex-app declares {other:?}, not hook manifest templates"),
        }
    }
}
