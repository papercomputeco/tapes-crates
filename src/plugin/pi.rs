//! pi's capture extension, as a template.
//!
//! pi has no base-URL environment knob, so capture requires code running
//! *inside* the harness: an extension that registers pi's providers against the
//! capture proxy and stamps the `X-Tapes-*` envelope pi's turns are attributed
//! by. That extension is harness knowledge — written against pi's extension API
//! — and so it lives here.
//!
//! # Why a template and not just a file
//!
//! [`super::PI_GATEWAY_EXTENSION`] ships the extension as a fixed file, and for
//! a consumer whose product has nothing to say that is the whole story. But a
//! consumer *does* legitimately differ in three presentational ways: what its
//! status entry is called, what it tells a user to run when the proxy is
//! fronting the wrong schema, and — for a product that runs one long-lived
//! proxy at a known address — where to point when nothing set
//! [`super::GATEWAY_URL_ENV`]; and in one way that is not presentational at all,
//! [`ExtensionBranding::nonce_env`]. Before this module those differences were a
//! forked copy of the whole extension in a consumer's repository, and the fork
//! bit exactly as forks do: the nonce echo-and-delete hardening had to be
//! hand-mirrored into it, and its assertions hand-mirrored alongside.
//!
//! So the capture logic is written once, in `assets/pi/gateway.template.ts`,
//! and the branded strings are slots a consumer fills through
//! [`render_extension`]. A hardening lands in the template and every consumer
//! regenerates. This is the same bargain [`super::codex_app`] strikes, arrived
//! at from the opposite direction: there the consumer's part could not be
//! removed, here it could not be *shared*.
//!
//! # What a slot may be
//!
//! Branding, defaults, and names. Never behaviour.
//!
//! The template declares every slot as the entire string literal of a `const`,
//! and reads slots nowhere else. A rendered value is therefore data in a
//! string, and cannot reach the capture-nonce handling — or anything else —
//! however it is spelled. That is a structural property, and the test
//! `a_slot_reaches_nothing_but_its_own_declaration` pins it by rendering
//! deliberately hostile values and checking that only the slot declarations
//! moved.
//!
//! The URL and schema variables stay crate-owned in full: they are one address
//! two products can safely agree on, and a consumer that wanted the extension
//! to read a different one, or to trust an envelope on different terms, is not
//! asking for a slot — it is asking for a fork, and the answer is no.
//!
//! [`ExtensionBranding::nonce_env`] is the exception, and it is an exception on
//! purpose rather than by erosion: the nonce variable is *consumed*, and pi
//! loads every installed extension into one process. Two renderings sharing
//! that name is two readers racing over one secret, and the loser registers
//! pi's providers with no echo — so the name has to belong to the artifact, not
//! to the crate. What stays crate-owned is everything the name is used *for*:
//! the read, the delete-at-load, the header the value is echoed in, and the
//! order of the three.

use super::slots::render_slots;

/// The extension template — one implementation, rendered per consumer.
///
/// Not installable as it stands: it still carries its slot placeholders. Every
/// path that writes a file goes through [`render_extension`].
pub const EXTENSION_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/pi/gateway.template.ts"
));

/// Slot for [`ExtensionBranding::default_gateway_url`].
pub const DEFAULT_GATEWAY_URL_SLOT: &str = "__TAPES_DEFAULT_GATEWAY_URL__";

/// Slot for [`ExtensionBranding::status_key`].
pub const STATUS_KEY_SLOT: &str = "__TAPES_STATUS_KEY__";

/// Slot for [`ExtensionBranding::status_suffix`].
pub const STATUS_SUFFIX_SLOT: &str = "__TAPES_STATUS_SUFFIX__";

/// Slot for [`ExtensionBranding::schema_mismatch_remedy`].
pub const SCHEMA_MISMATCH_REMEDY_SLOT: &str = "__TAPES_SCHEMA_MISMATCH_REMEDY__";

/// Slot for [`ExtensionBranding::nonce_env`].
pub const NONCE_ENV_SLOT: &str = "__TAPES_GATEWAY_NONCE_ENV__";

/// Every slot the template declares, for consumers that want to check their own
/// rendering left none behind.
pub const SLOTS: &[&str] = &[
    DEFAULT_GATEWAY_URL_SLOT,
    STATUS_KEY_SLOT,
    STATUS_SUFFIX_SLOT,
    SCHEMA_MISMATCH_REMEDY_SLOT,
    NONCE_ENV_SLOT,
];

/// The consumer-supplied strings a rendered pi extension presents.
///
/// All fields are plain strings that [`render_extension`] emits as escaped
/// string literals, so quotes, backslashes, backticks and `${…}` in any field
/// are inert: a double-quoted literal is not a template literal, and the two
/// characters that could end one early are escaped.
///
/// Build one with [`ExtensionBranding::new`] and the `with_*` setters. The
/// fields stay public because reading one is useful, but the type is
/// `#[non_exhaustive]`, so a struct literal only compiles inside this crate and
/// a downstream renderer must go through the constructor.
///
/// # Examples
///
/// ```
/// use tapes_harnesses::plugin::pi::{ExtensionBranding, render_extension};
///
/// let branding = ExtensionBranding::new(
///     "acme",
///     "ACME_GATEWAY_NONCE",
///     "Run `acme proxy use …` if requests fail.",
/// )
/// .with_default_gateway_url("127.0.0.1:4000");
/// let extension = render_extension(&branding);
/// assert!(!extension.contains("__TAPES_"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExtensionBranding<'a> {
    /// Where to route when [`super::GATEWAY_URL_ENV`] is unset.
    ///
    /// Empty — the default, and the right answer for almost everyone — leaves
    /// uncaptured pi sessions entirely alone. See the slot's own commentary in
    /// the template for why a non-empty value is a product decision with
    /// consequences beyond capture.
    pub default_gateway_url: &'a str,
    /// The pi status-entry key, and the prefix of the label in it.
    pub status_key: &'a str,
    /// Appended to the status label after the active schema. Usually empty.
    pub status_suffix: &'a str,
    /// The sentence appended to the schema-mismatch warning, naming whatever
    /// command *this* product switches its proxy with. The diagnosis it
    /// follows is the template's.
    pub schema_mismatch_remedy: &'a str,
    /// The environment variable this rendering takes its per-launch capture
    /// nonce from — and the one the consumer's launcher must set.
    ///
    /// **It must not be a name any other installed pi extension reads**, which
    /// in practice means it must not be [`super::GATEWAY_NONCE_ENV`] unless
    /// this rendering *is* the crate's own. pi loads every file in its global
    /// extension directory into one process, and the read is destructive: two
    /// renderings under one name means the second to load finds nothing,
    /// registers pi's providers with no nonce echo, and both products' sessions
    /// file as `unknown` with no error anywhere. The obvious safe answer is the
    /// consumer's own prefix — `PAPER_GATEWAY_NONCE` beside the crate's
    /// `TAPES_GATEWAY_NONCE`.
    ///
    /// The value is a name, not a secret, and — like every slot — is rendered
    /// as an escaped string literal, so it can reach nothing but its own
    /// declaration. What the extension *does* with the variable is not a slot:
    /// the read, the delete-at-load, and the echo header are the template's.
    pub nonce_env: &'a str,
}

impl<'a> ExtensionBranding<'a> {
    /// A branding from the three strings every consumer must answer for
    /// itself: what its status entry is called, which environment variable its
    /// launcher hands the capture nonce in, and how a user fixes a schema
    /// mismatch. The other two slots have meaningful empty defaults.
    ///
    /// `nonce_env` is required rather than defaulted precisely because there is
    /// no safe default — inheriting the crate's own name is the collision this
    /// argument exists to prevent. See [`ExtensionBranding::nonce_env`].
    #[must_use]
    pub const fn new(
        status_key: &'a str,
        nonce_env: &'a str,
        schema_mismatch_remedy: &'a str,
    ) -> Self {
        Self {
            default_gateway_url: "",
            status_key,
            status_suffix: "",
            schema_mismatch_remedy,
            nonce_env,
        }
    }

    /// Route uncaptured pi sessions at `url` instead of leaving them alone.
    #[must_use]
    pub const fn with_default_gateway_url(mut self, url: &'a str) -> Self {
        self.default_gateway_url = url;
        self
    }

    /// Append `suffix` to the status label.
    #[must_use]
    pub const fn with_status_suffix(mut self, suffix: &'a str) -> Self {
        self.status_suffix = suffix;
        self
    }

    fn slots(&self) -> [(&str, &str); 5] {
        [
            (DEFAULT_GATEWAY_URL_SLOT, self.default_gateway_url),
            (STATUS_KEY_SLOT, self.status_key),
            (STATUS_SUFFIX_SLOT, self.status_suffix),
            (SCHEMA_MISMATCH_REMEDY_SLOT, self.schema_mismatch_remedy),
            (NONCE_ENV_SLOT, self.nonce_env),
        ]
    }
}

/// This crate's own branding — the one [`super::PI_GATEWAY_EXTENSION`] carries.
///
/// Vendor-neutral by obligation, not by taste: an asset shipped from here loads
/// in every consumer's install, so it names no product, points nowhere by
/// default, and phrases its remedy in terms of the environment contract.
///
/// Its nonce variable is [`super::GATEWAY_NONCE_ENV`], the crate's own — which
/// is exactly the name a consumer rendering its own variant must *not* reuse.
pub const NEUTRAL_BRANDING: ExtensionBranding<'static> = ExtensionBranding::new(
    "tapes",
    super::GATEWAY_NONCE_ENV,
    "Point TAPES_GATEWAY_URL at a proxy serving that provider, or switch that proxy's active schema, if requests fail.",
);

/// Render the extension with a consumer's branding.
///
/// The result is a complete TypeScript file, ready to write into pi's
/// extension directory.
#[must_use]
pub fn render_extension(branding: &ExtensionBranding) -> String {
    render_slots(EXTENSION_TEMPLATE, &branding.slots())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::slots::string_literal;
    use super::super::{
        GATEWAY_NONCE_ENV, GATEWAY_NONCE_HEADER, GATEWAY_SCHEMA_ENV, GATEWAY_URL_ENV,
        PI_GATEWAY_EXTENSION,
    };
    use super::*;

    /// The checked-in neutral asset, as a path, so a failing golden test can
    /// name it — and rewrite it when asked to.
    fn neutral_asset_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pi/tapes-gateway.ts")
    }

    /// **The golden test.** `assets/pi/tapes-gateway.ts` is not hand-written any
    /// more: it is this template rendered with [`NEUTRAL_BRANDING`], checked in
    /// so that [`PI_GATEWAY_EXTENSION`] can stay a compile-time `&'static str`
    /// and every consumer that only copies bytes needs no code change at all.
    ///
    /// Checked in *and* generated means the two can disagree, so the comparison
    /// is byte-for-byte: an edit to either the template or the asset that is not
    /// mirrored in the other fails here rather than shipping an asset nobody
    /// meant to write.
    #[test]
    fn the_shipped_asset_is_this_template_rendered_neutrally() {
        let rendered = render_extension(&NEUTRAL_BRANDING);
        if rendered == PI_GATEWAY_EXTENSION.contents() {
            return;
        }
        if std::env::var_os("TAPES_BLESS").is_some() {
            std::fs::write(neutral_asset_path(), &rendered).unwrap();
            panic!("TAPES_BLESS rewrote the asset; rerun without it to confirm");
        }
        panic!(
            "{} has drifted from `gateway.template.ts` rendered with NEUTRAL_BRANDING.\n\
             Regenerate with `TAPES_BLESS=1 cargo test -p tapes-harnesses \
             the_shipped_asset_is_this_template_rendered_neutrally`.",
            neutral_asset_path().display(),
        );
    }

    /// A rendering is a finished file, not a half-filled template. A slot the
    /// template gained without [`ExtensionBranding`] gaining a field would
    /// otherwise be written into a user's pi directory verbatim.
    #[test]
    fn a_rendering_leaves_no_slot_behind() {
        for branding in [NEUTRAL_BRANDING, hostile_branding()] {
            let rendered = render_extension(&branding);
            assert!(
                !rendered.contains("__TAPES_"),
                "rendered extension still carries a slot placeholder"
            );
        }
        // …and the check above can actually fail: the template really does
        // carry every slot this module declares.
        for slot in SLOTS {
            assert!(
                EXTENSION_TEMPLATE.contains(slot),
                "the template declares no {slot}"
            );
        }
    }

    /// Values chosen to break out of a string literal, out of a template
    /// literal, out of a comment, and onto their own line.
    fn hostile_branding() -> ExtensionBranding<'static> {
        ExtensionBranding::new(
            "\";\ndelete process.env;\nconst STATUS_KEY = \"",
            "\";\nprocess.env.PATH = \"\";\nconst GATEWAY_NONCE_ENV = \"",
            "*/ `${nonce}` \\ end",
        )
        .with_default_gateway_url("\\\"; process.exit(1); //")
        .with_status_suffix("`+nonce+`")
    }

    /// A second consumer's branding, spelled the way a real one would be. Used
    /// wherever the property under test is about *two* renderings coexisting.
    fn second_consumer_branding() -> ExtensionBranding<'static> {
        ExtensionBranding::new(
            "paper",
            "PAPER_GATEWAY_NONCE",
            "Run `paper proxy use …` if requests fail.",
        )
    }

    /// **The containment property**, and the reason a slot is safe to hand a
    /// consumer at all: a rendered value lands in its own `const` declaration
    /// and nowhere else. Rendered against values built to escape, every line of
    /// the file except the four slot declarations must come out byte-identical
    /// to the neutral rendering — so no slot value, however spelled, can alter
    /// the nonce handling, the envelope, or the provider registration.
    #[test]
    fn a_slot_reaches_nothing_but_its_own_declaration() {
        let neutral = render_extension(&NEUTRAL_BRANDING);
        let hostile = render_extension(&hostile_branding());

        // Escaping keeps a newline inside a value a *newline in a value*, so
        // the two renderings still line up line for line.
        assert_eq!(
            neutral.lines().count(),
            hostile.lines().count(),
            "a slot value changed the file's line structure; escaping failed"
        );

        let differing: Vec<&str> = neutral
            .lines()
            .zip(hostile.lines())
            .filter(|(before, after)| before != after)
            .map(|(_, after)| after)
            .collect();
        assert_eq!(
            differing.len(),
            SLOTS.len(),
            "expected exactly the slot declarations to differ, got: {differing:#?}"
        );
        for line in &differing {
            assert!(
                line.starts_with("const ") && line.ends_with("\";"),
                "a slot value reached something that is not a const string \
                 declaration: {line:?}"
            );
        }
    }

    /// The nonce contract is the template's, in full, and survives any
    /// branding. Pinned here as well as against the shipped asset because the
    /// asset is now derived: a consumer rendering its own variant must get
    /// these bytes too.
    ///
    /// The *name* of the variable is the branding's, and so is checked against
    /// the branding rather than against a crate constant. Everything the name
    /// is used for — the read, the delete, the echo, and their order — is the
    /// template's and is checked literally.
    #[test]
    fn every_rendering_carries_the_whole_nonce_contract() {
        for branding in [
            NEUTRAL_BRANDING,
            second_consumer_branding(),
            hostile_branding(),
        ] {
            let rendered = render_extension(&branding);
            assert!(
                rendered.contains(&format!(
                    "const GATEWAY_NONCE_ENV = {};",
                    string_literal(branding.nonce_env)
                )),
                "the rendering does not declare its own branding's nonce variable"
            );
            assert!(rendered.contains(GATEWAY_NONCE_HEADER));
            assert!(
                rendered.contains("const nonce = process.env[GATEWAY_NONCE_ENV];"),
                "the rendering does not read the nonce from the environment"
            );
            assert!(
                rendered.contains("delete process.env[GATEWAY_NONCE_ENV];"),
                "the rendering does not delete the nonce from its environment"
            );
            assert!(
                rendered.contains("[GATEWAY_NONCE_HEADER]: nonce"),
                "the rendering does not echo the nonce under the header name"
            );
            let read = rendered
                .find("process.env[GATEWAY_NONCE_ENV]")
                .unwrap_or(usize::MAX);
            let delete = rendered
                .find("delete process.env[GATEWAY_NONCE_ENV]")
                .unwrap_or(0);
            assert!(read < delete, "the rendering deletes before it reads");
        }
    }

    /// The *addressing* half of the environment contract is the template's — a
    /// consumer cannot render an extension that looks for its gateway or its
    /// schema somewhere else, because there is no slot that could. (The nonce
    /// variable is the deliberate exception, and has a slot; see
    /// `two_consumers_renderings_do_not_share_a_nonce_variable`.)
    #[test]
    fn no_branding_can_move_the_environment_contract() {
        let hostile = render_extension(&hostile_branding());
        assert!(hostile.contains(&format!("= \"{GATEWAY_URL_ENV}\"")));
        assert!(hostile.contains(&format!("= \"{GATEWAY_SCHEMA_ENV}\"")));
        assert!(hostile.contains("process.env[GATEWAY_URL_ENV]"));
        assert!(hostile.contains("process.env[GATEWAY_SCHEMA_ENV]"));
    }

    /// **The PCC-1125 regression.** Two products' pi extensions install side by
    /// side in `~/.pi/agent/extensions/` under different file names, and pi
    /// loads both into one process. The nonce read is a read-*and-delete*, so
    /// under one variable name the second to load finds nothing, registers
    /// pi's providers with no echo, and every session on that machine — both
    /// products' — files as `unknown`.
    ///
    /// Asserted against real renderings rather than against the branding
    /// values, so re-unifying the name inside the template fails here even if
    /// the Rust still carries two.
    #[test]
    fn two_consumers_renderings_do_not_share_a_nonce_variable() {
        let ours = PI_GATEWAY_EXTENSION.contents();
        let theirs = render_extension(&second_consumer_branding());

        let declaration =
            |name: &str| format!("const GATEWAY_NONCE_ENV = {};", string_literal(name));
        assert!(
            ours.contains(&declaration(GATEWAY_NONCE_ENV)),
            "the shipped asset does not take its nonce from {GATEWAY_NONCE_ENV}"
        );
        assert!(
            theirs.contains(&declaration(second_consumer_branding().nonce_env)),
            "a consumer's rendering does not take its nonce from the variable its branding names"
        );
        assert_ne!(
            GATEWAY_NONCE_ENV,
            second_consumer_branding().nonce_env,
            "the test's two brandings must actually differ for it to prove anything"
        );
        assert!(
            !theirs.contains(&declaration(GATEWAY_NONCE_ENV)),
            "a consumer's rendering still reads the crate's own nonce variable; \
             two installed extensions would consume one secret and race"
        );

        // Namespacing is a fix only if it is *only* the variable. The header
        // travels to one launch's own proxy, which compares it against the
        // secret it generated — so both renderings must still echo under the
        // one name, and both must still delete at load.
        for rendering in [ours, theirs.as_str()] {
            assert!(
                rendering.contains(&format!(
                    "const GATEWAY_NONCE_HEADER = \"{GATEWAY_NONCE_HEADER}\";"
                )),
                "a rendering echoes the nonce under some other header"
            );
            assert!(
                rendering.contains("delete process.env[GATEWAY_NONCE_ENV];"),
                "a rendering no longer deletes the nonce at load"
            );
        }
    }

    /// The neutral remedy sentence names the variable a user would actually
    /// set. It is prose, so nothing else would notice it going stale after a
    /// rename of the constant.
    #[test]
    fn the_neutral_remedy_names_the_environment_variable_it_tells_users_to_set() {
        assert!(
            NEUTRAL_BRANDING
                .schema_mismatch_remedy
                .contains(GATEWAY_URL_ENV),
            "the neutral remedy does not name {GATEWAY_URL_ENV}"
        );
    }

    /// A consumer's default endpoint is a real slot with real consequences, so
    /// it must actually work — and must not be reachable by accident. Rendered
    /// empty (the neutral case) the extension stays inert.
    #[test]
    fn the_default_endpoint_slot_renders_a_usable_fallback() {
        let branded = render_extension(
            &ExtensionBranding::new("acme", "ACME_GATEWAY_NONCE", "…")
                .with_default_gateway_url("127.0.0.1:51539"),
        );
        assert!(branded.contains("const DEFAULT_GATEWAY_URL = \"127.0.0.1:51539\";"));
        assert!(
            render_extension(&NEUTRAL_BRANDING).contains("const DEFAULT_GATEWAY_URL = \"\";"),
            "the neutral rendering must fall back to nothing"
        );
    }
}
