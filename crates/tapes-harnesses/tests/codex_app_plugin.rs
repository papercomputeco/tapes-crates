//! What a downstream installer can actually do with the codex-app plugin
//! templates, exercised from outside the crate.
//!
//! The unit tests beside [`tapes_harnesses::plugin::codex_app`] can build a
//! `HookPluginIdentity` with a struct literal no matter what the type's
//! visibility rules say, because they live inside the defining module. That
//! made a real gap invisible: the type is `#[non_exhaustive]`, so for a
//! consumer a struct literal is E0639 — and with no constructor there was no
//! other way to build one, leaving `render_plugin_manifest` uncallable from
//! any other crate while every in-crate test stayed green.
//!
//! An integration test is a separate crate, so it is subject to exactly the
//! rules a consumer is. Whatever this file can do, paper's installer and a
//! future tapesctl installer can do too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use tapes_harnesses::plugin::codex_app::{
    CODEX_APP_TEMPLATES, HookPluginIdentity, render_hooks_manifest, render_plugin_manifest,
};

/// The end-to-end packaging path an installer walks: build an identity, render
/// both manifests, and get JSON with no slots left in it.
#[test]
fn a_consumer_can_render_a_complete_plugin_from_the_public_api() {
    let identity = HookPluginIdentity::new("acme-codex", "1.4.2")
        .with_description("Keeps Codex connected to acmed.")
        .with_display_name("Acme for Codex")
        .with_short_description("Keep Codex connected to Acme.")
        .with_long_description("Forwards allowlisted lifecycle metadata to local acmed.")
        .with_developer_name("Acme, Inc.");

    let plugin_manifest = render_plugin_manifest(&identity);
    let hooks_manifest = render_hooks_manifest(r#"/bin/sh "${PLUGIN_ROOT}/scripts/capture-hook""#);

    for (what, rendered) in [
        ("plugin.json", &plugin_manifest),
        ("hooks.json", &hooks_manifest),
    ] {
        assert!(
            !rendered.contains("__TAPES_"),
            "{what} still carries an unfilled slot:\n{rendered}"
        );
        serde_json::from_str::<serde_json::Value>(rendered)
            .unwrap_or_else(|e| panic!("{what} is not valid JSON: {e}\n{rendered}"));
    }

    let parsed: serde_json::Value = serde_json::from_str(&plugin_manifest).unwrap();
    assert_eq!(parsed["name"], "acme-codex");
    assert_eq!(parsed["version"], "1.4.2");
    assert_eq!(parsed["interface"]["displayName"], "Acme for Codex");
    assert_eq!(parsed["interface"]["developerName"], "Acme, Inc.");
    assert_eq!(parsed["author"]["name"], "Acme, Inc.");
}

/// The minimum a consumer must supply. `new` alone must produce a shippable
/// manifest — an installer that has only a plugin id and a version should not
/// have to guess at five presentation strings to get valid, slot-free JSON.
#[test]
fn a_consumer_can_render_from_the_constructor_alone() {
    let rendered = render_plugin_manifest(&HookPluginIdentity::new("acme-codex", "1.4.2"));
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

    assert!(!rendered.contains("__TAPES_"));
    assert_eq!(parsed["name"], "acme-codex");
    assert_eq!(parsed["interface"]["displayName"], "acme-codex");
}

/// The identity a consumer renders through is the same one it can inspect:
/// the fields stay readable, so an installer can log or diff what it is about
/// to write without re-deriving it from the rendered JSON.
#[test]
fn a_consumer_can_read_the_identity_it_built() {
    let identity = HookPluginIdentity::new("acme-codex", "1.4.2").with_developer_name("Acme, Inc.");

    assert_eq!(identity.name, "acme-codex");
    assert_eq!(identity.version, "1.4.2");
    assert_eq!(identity.developer_name, "Acme, Inc.");
}

/// The whole installer path from the registry: ask for the harness, take the
/// templates it declares, and render them. This is how a consumer finds the
/// bytes in the first place — it never names the template constants directly —
/// so the reachability of the render functions from a `PluginDelivery` is part
/// of the public contract, not an implementation detail.
#[test]
fn an_installer_reaches_the_templates_through_the_registry_and_renders_them() {
    let harness = tapes_harnesses::harness::find("codex-app").expect("codex-app is registered");
    let templates = match harness.plugin() {
        tapes_harnesses::harness::PluginDelivery::HookManifestTemplates(templates) => templates,
        other => panic!("codex-app declares {other:?}, not hook manifest templates"),
    };
    assert_eq!(*templates, CODEX_APP_TEMPLATES);

    // The registry's copies are templates, not finished files: a consumer that
    // packaged them verbatim would ship literal slots into the plugin UI.
    assert!(templates.plugin_manifest.contains("__TAPES_"));
    assert!(templates.hooks_manifest.contains("__TAPES_"));

    let identity = HookPluginIdentity::new("acme-codex", "1.4.2");
    assert!(!render_plugin_manifest(&identity).contains("__TAPES_"));
    assert!(!render_hooks_manifest("/usr/bin/acme-hook").contains("__TAPES_"));
}

/// The whole packaged tree, from the public API alone: the marketplace wrapper
/// plus the two manifests, each at the path the manager names.
///
/// This is the file list an installer writes. It is asserted from outside the
/// crate because the point of moving the layout here was that two installers
/// stop authoring it — if any part of it were unreachable, the second
/// installer would have to guess again.
#[test]
fn a_consumer_can_package_a_complete_marketplace_from_the_public_api() {
    use tapes_harnesses::plugin::codex_app::manager;

    let plugin_name = "acme-codex";
    let files: Vec<(std::path::PathBuf, String)> = vec![
        (
            std::path::PathBuf::from(manager::MARKETPLACE_MANIFEST_PATH),
            manager::render_marketplace_manifest(
                &manager::MarketplaceIdentity::new("acme", plugin_name).with_display_name("Acme"),
            ),
        ),
        (
            manager::plugin_manifest_path(plugin_name),
            render_plugin_manifest(&HookPluginIdentity::new(plugin_name, "1.4.2")),
        ),
        (
            manager::hooks_manifest_path(plugin_name),
            render_hooks_manifest("/usr/bin/acme-hook"),
        ),
    ];

    let root = tempfile::tempdir().unwrap();
    for (relative, contents) in &files {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        assert!(
            !contents.contains("__TAPES_"),
            "{} carries an unfilled slot",
            relative.display()
        );
        serde_json::from_str::<serde_json::Value>(contents)
            .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", relative.display()));
    }

    // The offer resolves: the marketplace's declared source path is a real
    // directory under the root, and it holds the plugin manifest Codex reads.
    let marketplace: serde_json::Value = serde_json::from_str(&files[0].1).unwrap();
    let offered = marketplace["plugins"][0]["source"]["path"]
        .as_str()
        .unwrap();
    let offered = root.path().join(offered.trim_start_matches("./"));
    assert!(offered.is_dir(), "{} is not a directory", offered.display());
    assert!(offered.join(".codex-plugin").join("plugin.json").is_file());

    // And a manager built over that root issues commands naming it.
    let manager = manager::PluginManager::new("codex", root.path(), "acme", plugin_name);
    assert_eq!(manager.plugin_spec(), "acme-codex@acme");
    assert_eq!(
        manager.manual_commands()[0],
        format!("codex plugin marketplace add {}", root.path().display())
    );
}
