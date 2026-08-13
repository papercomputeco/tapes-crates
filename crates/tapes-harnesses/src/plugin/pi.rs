//! pi's capture extension: one installed file, branded at runtime.
//!
//! pi has no base-URL environment knob, so capture requires code running
//! *inside* the harness: an extension that registers pi's providers against the
//! capture proxy and stamps the `X-Tapes-*` envelope pi's turns are attributed
//! by. That extension is harness knowledge — written against pi's extension API
//! — and so it lives here, as [`super::PI_GATEWAY_EXTENSION`].
//!
//! # Why exactly one file
//!
//! pi auto-discovers global extensions by loading *every* file in
//! `~/.pi/agent/extensions/`, into one process. That makes the number of
//! installed copies a correctness property rather than a packaging detail.
//!
//! Two copies contend over everything the file touches. The nonce read is a
//! read-and-delete, so the second copy to load finds nothing; worse, it
//! registers the same three providers anyway, without the echo, and the last
//! registration wins. The proxy then cannot tell a real launch from a forged
//! envelope, and both products' sessions file as `unknown` with no error
//! anywhere. Coordinating two copies — per-product variable names, a gate that
//! stands one of them down — manages that collision. Installing one file to one
//! path removes the second reader, and a collision needs two.
//!
//! So the asset is not rendered per consumer. Every client writes the same
//! bytes to the same path, which is the property the opencode plugin has always
//! had for free and the reason it was never exposed to this bug.
//!
//! # What a product may still say
//!
//! Its status entry's name, and what it tells a user to run when the proxy is
//! fronting the wrong schema. Those are real differences and shipping different
//! *bytes* was only ever one way to express them; the extension reads them from
//! the environment of the launch instead — [`GATEWAY_LABEL_ENV`],
//! [`GATEWAY_LABEL_SUFFIX_ENV`], [`GATEWAY_REMEDY_ENV`] — set by whichever
//! client launched the session, for the length of that session.
//!
//! Runtime branding keeps the containment a rendered slot used to buy, and
//! keeps it more cheaply: a value read from the environment is a string in a
//! variable, so it cannot be syntax however it is spelled. It reaches
//! `setStatus` and `notify` and nothing else — never the nonce handling, the
//! envelope, or the provider registration — and the test
//! `presentation_values_reach_only_the_status_entry_and_the_notification` pins
//! that by reading the asset.
//!
//! What is *not* here is a default endpoint. The asset used to carry one, so a
//! product running a long-lived proxy at a fixed address could capture pi
//! sessions nobody launched under it. One file cannot hold one product's
//! address without redirecting every other product's sessions there too, so the
//! address moved entirely into the launch: a product that wants uncaptured pi
//! sessions routed anyway sets [`super::GATEWAY_URL_ENV`] in the environment
//! those sessions inherit, where the claim is explicit and revocable.
//!
//! The environment, nonce, and schema contract stays wholly crate-owned. There
//! is no product-supplied name anywhere in it, which is what makes the shared
//! spellings in [`super`] safe again.

/// Environment variable naming the pi status entry this extension registers,
/// and the prefix of the label shown in it.
///
/// Display text, set by the launching client. A short product word — the
/// crate's own asset falls back to [`DEFAULT_LABEL`] when nothing set it.
pub const GATEWAY_LABEL_ENV: &str = "TAPES_GATEWAY_LABEL";

/// Environment variable appended to the status label after the active schema.
///
/// Display text. Exists because a product may need to say something about its
/// own routing that the schema name alone does not carry — a proxy that also
/// fronts a provider outside the active schema, say. Unset and empty mean the
/// same thing: nothing appended.
pub const GATEWAY_LABEL_SUFFIX_ENV: &str = "TAPES_GATEWAY_LABEL_SUFFIX";

/// Environment variable carrying the sentence appended to the extension's
/// schema-mismatch warning.
///
/// Display text. The diagnosis is the asset's; only the remedy is the
/// launching client's, because only that client knows what command switches
/// its proxy. Unset falls back to a sentence phrased in terms of
/// [`super::GATEWAY_URL_ENV`], since the crate's own asset may name no product.
pub const GATEWAY_REMEDY_ENV: &str = "TAPES_GATEWAY_REMEDY";

/// The status label the asset presents when [`GATEWAY_LABEL_ENV`] is unset.
pub const DEFAULT_LABEL: &str = "tapes";

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::{
        GATEWAY_NONCE_ENV, GATEWAY_NONCE_HEADER, GATEWAY_PROVIDER_ROUTE_PREFIX,
        GATEWAY_PROVIDER_ROUTES_ENV, GATEWAY_SCHEMA_ENV, GATEWAY_URL_ENV, PI_GATEWAY_EXTENSION,
        provider_route, split_provider_route,
    };
    use super::*;

    /// The asset, as the only thing there is to inspect. There is no renderer
    /// any more: what a consumer installs is exactly these bytes.
    fn asset() -> &'static str {
        PI_GATEWAY_EXTENSION.contents()
    }

    /// The value of a `const NAME = "…";` declaration in the asset.
    ///
    /// Reading the declaration rather than matching a substring is what makes
    /// the pins below say "this name and no other": an asset that kept a stale
    /// spelling *alongside* the right one would satisfy a `contains` and fail
    /// here.
    fn declared_const(name: &str) -> String {
        let prefix = format!("const {name} = \"");
        let at = asset()
            .find(&prefix)
            .unwrap_or_else(|| panic!("the asset declares no {name}"));
        let rest = &asset()[at + prefix.len()..];
        let end = rest
            .find("\";")
            .unwrap_or_else(|| panic!("the asset's {name} declaration is unterminated"));
        rest[..end].to_string()
    }

    /// The asset's code, one line per line, with line comments and blank lines
    /// removed.
    ///
    /// The containment test below asks what a value *reaches*, and prose about
    /// what it must not reach would otherwise answer for it.
    fn code_lines() -> Vec<String> {
        asset()
            .lines()
            .map(|line| match line.find("//") {
                // `http://` is the one `//` inside code here, and truncating at
                // it leaves the identifiers this test looks for intact.
                Some(at) => line[..at].to_string(),
                None => line.to_string(),
            })
            .filter(|line| !line.trim().is_empty())
            .collect()
    }

    /// **The one-artifact fix, stated as a property.** The asset is not a
    /// template
    /// and has no consumer-varying bytes: what any client installs is this
    /// file, so a second installed reader of the pi extension directory cannot
    /// exist for the copies to contend over.
    ///
    /// A reintroduced renderer would show up here as a placeholder left in the
    /// shipped asset — which is precisely what the old two-branding model wrote
    /// into it.
    #[test]
    fn the_asset_is_a_finished_file_and_not_a_template() {
        assert!(
            !asset().contains("__TAPES_"),
            "the asset carries a slot placeholder; it is a template again, and \
             a template means one rendering per product means two installed files"
        );
    }

    /// The environment contract, pinned as whole declarations.
    ///
    /// The asset reads the environment by name, so the Rust constant and the
    /// literal in the asset are two spellings of one contract; renaming the
    /// constant alone would leave an installed extension waiting for a variable
    /// nobody sets, which is a silently uncaptured session rather than a build
    /// failure. Pinned as the entire `const … = "…";` declaration because the
    /// per-product namespacing this change replaces is exactly the kind of
    /// second spelling a `contains` would let through.
    #[test]
    fn the_asset_reads_the_shared_gateway_environment_contract() {
        assert_eq!(declared_const("GATEWAY_URL_ENV"), GATEWAY_URL_ENV);
        assert_eq!(declared_const("GATEWAY_SCHEMA_ENV"), GATEWAY_SCHEMA_ENV);
        assert_eq!(declared_const("GATEWAY_NONCE_ENV"), GATEWAY_NONCE_ENV);
        assert_eq!(declared_const("GATEWAY_NONCE_HEADER"), GATEWAY_NONCE_HEADER);
    }

    /// The presentation contract, pinned the same way. These are the names that
    /// replaced the render-time slots, so a client that sets them and an asset
    /// that reads something else is the failure mode: a status entry stuck on
    /// the neutral fallback, with capture otherwise working.
    #[test]
    fn the_asset_reads_the_presentation_contract_at_runtime() {
        assert_eq!(declared_const("GATEWAY_LABEL_ENV"), GATEWAY_LABEL_ENV);
        assert_eq!(
            declared_const("GATEWAY_LABEL_SUFFIX_ENV"),
            GATEWAY_LABEL_SUFFIX_ENV
        );
        assert_eq!(declared_const("GATEWAY_REMEDY_ENV"), GATEWAY_REMEDY_ENV);
        assert_eq!(declared_const("DEFAULT_LABEL"), DEFAULT_LABEL);
        // …and each declared name is actually read, so the pins above cannot
        // pass against a variable the asset merely names.
        for identifier in [
            "GATEWAY_LABEL_ENV",
            "GATEWAY_LABEL_SUFFIX_ENV",
            "GATEWAY_REMEDY_ENV",
        ] {
            assert!(
                asset().contains(&format!("process.env[{identifier}]")),
                "the asset declares {identifier} but never reads it"
            );
        }
    }

    /// The per-provider routing contract, pinned as whole declarations for the
    /// same reason the rest of the environment contract is: the asset and the
    /// launching client are two spellings of one agreement, and a rename on one
    /// side alone is a session that routes every provider to one upstream while
    /// the proxy waits for labels.
    #[test]
    fn the_asset_reads_the_provider_routing_contract() {
        assert_eq!(
            declared_const("GATEWAY_PROVIDER_ROUTES_ENV"),
            GATEWAY_PROVIDER_ROUTES_ENV
        );
        assert_eq!(
            declared_const("PROVIDER_ROUTE_PREFIX"),
            GATEWAY_PROVIDER_ROUTE_PREFIX
        );
        assert!(
            asset().contains(&format!("process.env[{}]", "GATEWAY_PROVIDER_ROUTES_ENV")),
            "the asset declares the routing variable but never reads it"
        );
    }

    /// **The property an internally routing gateway depends on.** A launcher
    /// that sets
    /// nothing must get the requests it got before this existed — same base
    /// URL, same path — because a gateway that routes internally would receive
    /// a labelled path it has no route for and fail every turn.
    ///
    /// Stated against the asset's own conditional rather than by executing it:
    /// the label is built in exactly one place, and that place is gated on the
    /// variable. An edit that labelled unconditionally would have to move the
    /// construction out of the ternary, which fails here.
    #[test]
    fn a_launcher_that_asks_for_nothing_gets_unlabelled_registrations() {
        let opening = "providerRoutes ? `";
        let at = asset()
            .find(opening)
            .expect("the asset does not gate the provider label on the routing variable");
        let rest = &asset()[at + opening.len()..];
        let labelled = &rest[..rest.find('`').expect("unterminated provider base URL")];
        assert_eq!(labelled, "${baseUrl}${PROVIDER_ROUTE_PREFIX}/${provider}");
        // …and the other arm is the bare base URL, unchanged.
        let otherwise = rest[rest.find('`').unwrap() + 1..]
            .trim_start()
            .strip_prefix(':')
            .expect("the routing conditional has no unlabelled arm")
            .trim_start();
        assert!(
            otherwise.starts_with("baseUrl"),
            "the unlabelled arm is not the bare base URL: {otherwise:?}"
        );
    }

    /// The active schema stops being a constraint once every provider has its
    /// own route, so the mismatch warning must stand down with it. Left in, it
    /// tells a user whose session is being captured correctly that it is not.
    #[test]
    fn the_schema_mismatch_warning_stands_down_under_provider_routes() {
        assert!(
            asset().contains("if (!providerRoutes && schemaProvider &&"),
            "the schema-mismatch warning still fires when the proxy routes \
             every provider it registers"
        );
    }

    /// The two halves of the route shape agree: what the asset builds is what
    /// [`split_provider_route`] takes apart. The asset composes its path in
    /// TypeScript and the proxy parses it in Rust, so nothing but a test can
    /// hold the two spellings together.
    #[test]
    fn the_route_the_asset_builds_is_the_route_the_contract_parses() {
        for provider in ["anthropic", "openai", "openai-codex"] {
            let base = provider_route(provider);
            assert!(
                base.starts_with(GATEWAY_PROVIDER_ROUTE_PREFIX),
                "{base} is not under the declared prefix"
            );
            // pi appends its own path to the registered base URL; the join is
            // what actually reaches the proxy.
            let requested = format!("{base}/v1/messages");
            let (labelled, rest) = split_provider_route(&requested)
                .expect("a route this contract built is not one it parses");
            assert_eq!(labelled, provider);
            assert_eq!(rest, "/v1/messages");
        }
    }

    /// **The containment property**, and the reason runtime branding is safe to
    /// hand a product at all: a value the launching client sets reaches the
    /// status entry and the notification, and nothing else.
    ///
    /// The rendered-slot model needed this proven against deliberately hostile
    /// values, because a rendered value became *syntax* in a file. A runtime
    /// value cannot: it is a string in a variable however it is spelled. What
    /// is still worth pinning is where the file lets those variables go — a
    /// later edit that interpolated the product's label into the envelope, or
    /// used it to build the base URL, would hand a display string authority
    /// over attribution.
    #[test]
    fn presentation_values_reach_only_the_status_entry_and_the_notification() {
        let sensitive = [
            "registerProvider",
            "GATEWAY_NONCE_HEADER",
            "nonce",
            "baseUrl",
            "X-Tapes-",
            "envelope",
            "headers",
        ];
        for identifier in ["statusLabel", "statusSuffix", "schemaRemedy"] {
            let lines: Vec<String> = code_lines()
                .into_iter()
                .filter(|line| line.contains(identifier))
                .collect();
            assert!(
                lines.len() >= 2,
                "{identifier} is declared but never used; this test would pass vacuously"
            );
            for (line, token) in lines
                .iter()
                .flat_map(|line| sensitive.iter().copied().map(move |token| (line, token)))
            {
                assert!(
                    !line.contains(token),
                    "{identifier} reaches {token:?} on {line:?}; a display string \
                     must not touch the capture path"
                );
            }
        }
    }

    /// The status label a product sees, still built the way it was before the
    /// slots became environment reads. For a product whose label is `acme`, the
    /// composed value is `acme:anthropic+codex` exactly — a string users read in
    /// a status bar and match on, so the pieces, their order, and the separator
    /// are all observable.
    ///
    /// The expected value is reconstructed from the template literal *in the
    /// asset*, so reordering the pieces there fails here instead of quietly
    /// producing `anthropic:acme+codex`.
    #[test]
    fn the_status_label_is_composed_exactly_as_it_was_when_it_was_rendered() {
        let opening = "ctx.ui.setStatus(statusLabel, `";
        let at = asset()
            .find(opening)
            .expect("the asset does not set a status entry from the runtime label");
        let rest = &asset()[at + opening.len()..];
        let pattern = &rest[..rest.find("`);").expect("unterminated status label")];

        assert_eq!(pattern, "${statusLabel}:${activeSchema}${statusSuffix}");
        let label = pattern
            .replace("${statusLabel}", "acme")
            .replace("${activeSchema}", "anthropic")
            .replace("${statusSuffix}", "+codex");
        assert_eq!(
            label, "acme:anthropic+codex",
            "the label a consumer's launch presents has changed"
        );
    }

    /// An unset presentation variable must leave the asset saying something,
    /// and something vendor-neutral: these bytes install into every client,
    /// including ones that set none of the three.
    #[test]
    fn the_fallbacks_are_neutral_and_name_the_variable_a_user_would_set() {
        assert_eq!(declared_const("DEFAULT_LABEL"), DEFAULT_LABEL);
        let remedy = asset()
            .split_once("const DEFAULT_REMEDY =")
            .expect("the asset declares no DEFAULT_REMEDY")
            .1;
        let remedy = &remedy[..remedy.find(';').expect("unterminated DEFAULT_REMEDY")];
        assert!(
            remedy.contains(GATEWAY_URL_ENV),
            "the neutral remedy does not name {GATEWAY_URL_ENV}, so it tells a \
             user nothing they can act on"
        );
    }

    /// The nonce contract in full, against the one asset there now is: read
    /// once, deleted before any tool can run, echoed under the crate's header
    /// name, and in that order. Unchanged by this fix, and pinned so it stays
    /// that way — the delete is what keeps shell-tool children from inheriting
    /// the secret, and it is the read the *second* installed copy used to lose.
    #[test]
    fn the_asset_reads_deletes_and_echoes_the_nonce_in_that_order() {
        assert!(
            asset().contains("const nonce = process.env[GATEWAY_NONCE_ENV];"),
            "the asset does not read the nonce from the environment"
        );
        assert!(
            asset().contains("delete process.env[GATEWAY_NONCE_ENV];"),
            "the asset does not delete the nonce from its environment; \
             shell-tool subprocesses would inherit the secret"
        );
        assert!(
            asset().contains("[GATEWAY_NONCE_HEADER]: nonce"),
            "the asset does not echo the nonce under the header name"
        );
        let read = asset()
            .find("process.env[GATEWAY_NONCE_ENV]")
            .unwrap_or(usize::MAX);
        let delete = asset()
            .find("delete process.env[GATEWAY_NONCE_ENV]")
            .unwrap_or(0);
        assert!(
            read < delete,
            "the asset deletes the nonce before it reads it"
        );
    }

    /// The default endpoint is gone, and must stay gone. One file installed by
    /// every client cannot carry one client's address: it would redirect every
    /// other client's uncaptured pi sessions to that address too. Absence of a
    /// loopback literal is the cheapest durable check that it has not come
    /// back as a constant.
    #[test]
    fn the_asset_has_no_built_in_endpoint_to_fall_back_to() {
        for literal in ["127.0.0.1", "localhost:", "DEFAULT_GATEWAY_URL"] {
            assert!(
                !asset().contains(literal),
                "the asset carries {literal:?}; it must be inert without {GATEWAY_URL_ENV}"
            );
        }
        assert!(
            asset().contains("const rawBaseUrl = process.env[GATEWAY_URL_ENV];"),
            "the asset must take its address from the launch and nowhere else"
        );
    }
}
