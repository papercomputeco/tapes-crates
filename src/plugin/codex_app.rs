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

impl HookPluginIdentity<'_> {
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
    render_slot(HOOKS_MANIFEST_TEMPLATE, HOOK_COMMAND_SLOT, hook_command)
}

/// Render the plugin manifest with the consumer's identity strings.
#[must_use]
pub fn render_plugin_manifest(identity: &HookPluginIdentity) -> String {
    identity.slots().iter().fold(
        PLUGIN_MANIFEST_TEMPLATE.to_owned(),
        |manifest, (slot, value)| render_slot(&manifest, slot, value),
    )
}

/// Replace every quoted occurrence of `slot` with the JSON-escaped `value`.
///
/// Substitution targets `"__SLOT__"` including its quotes and replaces it
/// with a complete JSON string literal, so escaping cannot be forgotten and a
/// slot can never be half-replaced inside a larger value.
fn render_slot(template: &str, slot: &str, value: &str) -> String {
    let quoted_slot = format!("\"{slot}\"");
    template.replace(&quoted_slot, &json_string_literal(value))
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
        HookPluginIdentity {
            name: "acme-codex",
            version: "0.1.0",
            description: "Keeps Codex connected to acmed.",
            display_name: "Acme for Codex",
            short_description: "Keep Codex connected to Acme.",
            long_description: "Forwards lifecycle metadata to local acmed.",
            developer_name: "Acme",
        }
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
