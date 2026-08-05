//! Plugin artifacts — the files that must be installed *into* a harness before
//! its traffic can be captured at all.
//!
//! Most harnesses need nothing here: capture works by pointing the harness's
//! base-URL knob at a proxy, which [`crate::launch`] plans. A harness with no
//! such knob needs code running inside it instead, and that code is an asset
//! somebody has to write to disk. This module owns those assets; a consumer is
//! only the installer.
//!
//! # Why the assets live here
//!
//! An in-harness extension is harness knowledge in the most literal sense — it
//! is written against the harness's own extension API. Keeping it in a
//! consumer's repository meant every consumer that wanted to capture that
//! harness had to carry its own copy, and the copies would drift in exactly the
//! way this crate exists to prevent. The asset moved here so
//! `tapesctl plugin install` and a closed-source client install the same bytes.
//!
//! # What still must not be vendored
//!
//! The move is conditional on the asset being **vendor-neutral**, which was not
//! free: the extension that seeded [`PI_GATEWAY_EXTENSION`] read a product's
//! environment variables, defaulted to that product's daemon port, and told the
//! user to run that product's CLI. All three are gone. What a crate-owned asset
//! may know is that *a* capture proxy exists and how to talk to one; where that
//! proxy is, what it is called, and how a user manages it stay with whoever
//! installs the asset.
//!
//! Concretely, an asset here may not carry a vendor's name, a vendor's default
//! endpoint, or a vendor's environment-variable spelling — it reads
//! [`GATEWAY_URL_ENV`] and nothing else. A consumer whose plugin genuinely
//! cannot be de-branded keeps that plugin in its own repository; it does not get
//! a variant of [`crate::harness::PluginDelivery`] here.
//!
//! # The environment contract
//!
//! An installed artifact is inert until the launching consumer sets
//! [`GATEWAY_URL_ENV`]. That is deliberate: an artifact installs globally into
//! the harness's own extension directory, so it loads for every session on the
//! machine — including sessions nobody is capturing. Making the redirect
//! conditional on the environment is what keeps an install from changing the
//! behaviour of sessions the user did not launch under capture.

use std::path::{Path, PathBuf};

/// Environment variable naming the capture-proxy base URL an installed plugin
/// should send the harness's LLM traffic to.
///
/// Accepts a bare `host:port` or a full URL; the asset normalises it the same
/// way [`crate::launch::ProxyEndpoint`] does. Unset means "not captured", and
/// an installed plugin must then leave the harness's own endpoints alone.
pub const GATEWAY_URL_ENV: &str = "TAPES_GATEWAY_URL";

/// Environment variable naming which upstream provider schema the capture proxy
/// is currently fronting (e.g. `anthropic`, `openai`).
///
/// Optional, and a display/diagnostic hint only: a plugin may surface it and may
/// warn when the user picks a model the proxy is not routing, but it must not
/// gate the redirect on it. A proxy that fronts one schema at a time is one
/// deployment shape, not a requirement of the contract.
pub const GATEWAY_SCHEMA_ENV: &str = "TAPES_GATEWAY_SCHEMA";

/// Environment variable carrying the per-launch capture nonce.
///
/// A self-attributing harness's `X-Tapes-*` envelope is a claim, and the
/// ancestry check ([`crate::attribution::peer_trust`]) cannot tell the harness
/// apart from the harness's *own subprocesses* — a command run by a shell tool
/// is a descendant of the launched PID too, and could otherwise stamp another
/// session's envelope. The launching consumer (tapesctl, paperd) generates a
/// fresh secret per capture, sets it in this variable for the harness process,
/// and requires it echoed back before believing any envelope. The value must
/// never be logged, forwarded upstream, or included in captured output.
///
/// An installed plugin must read this variable **once at load and delete it
/// from its process environment immediately**, before any tool can run:
/// subprocesses the harness later spawns inherit the harness's *current*
/// environment, so the deletion keeps them from receiving the secret at all —
/// it survives only in the plugin's own memory. With that in place the
/// residual exposure is exactly two channels, and no more should be claimed:
/// a same-UID process reading the harness's *original* environment out of
/// `/proc/<pid>/environ` on Linux (that file snapshots the environment at
/// `exec` and does not reflect the deletion), and anything the harness itself
/// chooses to pass along explicitly.
///
/// Unset means the launching client predates the nonce contract; an installed
/// plugin must then simply not send the header rather than fail.
pub const GATEWAY_NONCE_ENV: &str = "TAPES_GATEWAY_NONCE";

/// Request header in which an installed plugin echoes the capture nonce back
/// to the proxy that launched it.
///
/// Lower-case for the same reason the [`crate::envelope`] names are: HTTP/2
/// lowercases header names on the wire, so the canonical spelling is the wire
/// spelling. The header is a private channel between the extension and its own
/// capture proxy — the proxy validates it against the value it generated and
/// **strips it before forwarding**, so the nonce never reaches an upstream and
/// never appears in a captured turn.
pub const GATEWAY_NONCE_HEADER: &str = "x-tapes-gateway-nonce";

/// Does a presented nonce match the one this capture generated?
///
/// Shared so both consumers enforce the same rule. Two properties matter:
///
/// * **An empty expectation never matches.** A consumer that failed to
///   generate a nonce must fail closed, not accept an empty echo.
/// * **The comparison is constant-time in the matching prefix.** The caller is
///   a loopback listener reachable by every local process; a byte-at-a-time
///   `==` would let one probe the secret through response timing. Length still
///   leaks, and may: nonce lengths are not secret.
#[must_use]
pub fn nonce_matches(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    if expected.is_empty() || expected.len() != presented.len() {
        return false;
    }
    expected
        .iter()
        .zip(presented)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// One file a consumer installs into a harness.
///
/// The destination is expressed as components relative to the user's home
/// directory rather than an absolute path, for the same reason
/// [`crate::launch`] recipes never pick a location in the user's home: the crate
/// states *where within a home* the harness looks, and the consumer supplies the
/// home it is installing into — which is also what makes the whole thing
/// testable against a temporary directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginArtifact {
    file_name: &'static str,
    install_dir: &'static [&'static str],
    contents: &'static str,
}

impl PluginArtifact {
    /// The file name to write, without any directory part.
    #[must_use]
    pub const fn file_name(&self) -> &'static str {
        self.file_name
    }

    /// The install directory's path components, relative to the user's home.
    ///
    /// Exposed so an installer can describe the destination — in a dry run, say
    /// — without having a home directory to resolve against.
    #[must_use]
    pub const fn install_dir_components(&self) -> &'static [&'static str] {
        self.install_dir
    }

    /// The asset's full contents, embedded at compile time.
    #[must_use]
    pub const fn contents(&self) -> &'static str {
        self.contents
    }

    /// The directory this artifact installs into, beneath `home`.
    #[must_use]
    pub fn install_dir(&self, home: &Path) -> PathBuf {
        self.install_dir
            .iter()
            .fold(home.to_path_buf(), |path, component| path.join(component))
    }

    /// The full path this artifact installs to, beneath `home`.
    #[must_use]
    pub fn install_path(&self, home: &Path) -> PathBuf {
        self.install_dir(home).join(self.file_name)
    }
}

/// pi's capture extension.
///
/// pi has no base-URL environment knob, so there is nothing for a launch recipe
/// to set: capture requires this extension registering pi's providers against
/// the proxy from inside the harness. It is also what makes pi the
/// [`crate::harness::AttributionStrategy::SelfAttributing`] harness — the
/// `X-Tapes-*` headers it attaches are the only attribution pi's turns get,
/// because no PID-indexed session file exists for a client to read.
///
/// pi auto-discovers global extensions from `~/.pi/agent/extensions/*.ts`, so
/// installing the file is the whole installation.
pub const PI_GATEWAY_EXTENSION: PluginArtifact = PluginArtifact {
    file_name: "tapes-gateway.ts",
    install_dir: &[".pi", "agent", "extensions"],
    contents: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/pi/tapes-gateway.ts"
    )),
};

/// The artifact set for a harness captured by a bundled pi extension.
pub(crate) const PI_ARTIFACTS: &[PluginArtifact] = &[PI_GATEWAY_EXTENSION];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::envelope::{HARNESS_ID_PI, X_TAPES_HARNESS_ID, X_TAPES_HARNESS_SESSION_ID};
    use crate::harness::{PluginDelivery, REGISTRY};

    /// Every artifact the crate ships, however it is reached from the registry.
    fn all_artifacts() -> Vec<&'static PluginArtifact> {
        REGISTRY
            .iter()
            .flat_map(|harness| harness.plugin_artifacts())
            .collect()
    }

    /// The point of the whole module: an artifact must be installable, so the
    /// registry has to actually reach one. A refactor that left every harness's
    /// artifact list empty would otherwise pass every other test here
    /// vacuously.
    #[test]
    fn the_registry_reaches_at_least_one_artifact() {
        assert!(
            !all_artifacts().is_empty(),
            "no harness in the registry declares a plugin artifact"
        );
        assert!(all_artifacts().contains(&&PI_GATEWAY_EXTENSION));
    }

    /// An artifact's destination components are joined onto a caller-supplied
    /// home. A component carrying a separator or a `..` would let that join
    /// leave the home directory — the installer canonicalises and contains, but
    /// the crate must not hand it something designed to escape in the first
    /// place.
    #[test]
    fn no_artifact_path_component_can_leave_the_home_directory() {
        for artifact in all_artifacts() {
            let components = artifact
                .install_dir_components()
                .iter()
                .chain(std::iter::once(&artifact.file_name()))
                .copied()
                .collect::<Vec<_>>();
            for component in components {
                assert!(!component.is_empty(), "empty component in {artifact:?}");
                assert!(
                    !component.contains('/') && !component.contains('\\'),
                    "{component:?} is a path, not a component"
                );
                assert!(
                    component != ".." && component != ".",
                    "{component:?} traverses"
                );
            }
        }
    }

    #[test]
    fn an_artifact_resolves_beneath_the_home_it_is_given() {
        let home = Path::new("/home/u");
        assert_eq!(
            PI_GATEWAY_EXTENSION.install_path(home),
            PathBuf::from("/home/u/.pi/agent/extensions/tapes-gateway.ts"),
        );
        assert_eq!(
            PI_GATEWAY_EXTENSION.install_dir(home),
            PathBuf::from("/home/u/.pi/agent/extensions"),
        );
        // Different home, same shape — nothing is baked in at compile time.
        assert!(
            PI_GATEWAY_EXTENSION
                .install_path(Path::new("/tmp/t"))
                .starts_with("/tmp/t"),
        );
    }

    #[test]
    fn every_artifact_carries_its_bytes() {
        for artifact in all_artifacts() {
            assert!(
                !artifact.contents().trim().is_empty(),
                "{} is empty",
                artifact.file_name(),
            );
        }
    }

    /// The de-branding pass, pinned. The asset was generalised out of a
    /// vendor's repository, and the licence to keep it here is that it names no
    /// vendor: a re-vendored branded copy, or a branded hint added later, fails
    /// here rather than shipping to every consumer of the crate.
    #[test]
    fn no_artifact_carries_vendor_branding() {
        for artifact in all_artifacts() {
            let lowered = artifact.contents().to_ascii_lowercase();
            for token in ["paper", "papercompute"] {
                assert!(
                    !lowered.contains(token),
                    "{} mentions {token:?}; a crate-owned asset must be vendor-neutral",
                    artifact.file_name(),
                );
            }
        }
    }

    /// The asset reads the environment by name, so the Rust constant and the
    /// literal in the asset are two spellings of one contract. Renaming the
    /// constant alone would leave an installed plugin waiting for a variable
    /// nobody sets — a silently uncaptured session, not a build failure.
    #[test]
    fn the_pi_extension_reads_the_gateway_environment_contract() {
        let contents = PI_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains(GATEWAY_URL_ENV),
            "the asset does not read {GATEWAY_URL_ENV}"
        );
        assert!(
            contents.contains(GATEWAY_SCHEMA_ENV),
            "the asset does not read {GATEWAY_SCHEMA_ENV}"
        );
    }

    /// The nonce contract is asset-side too: the extension reads the secret
    /// from the environment and echoes it in the header, and both names are
    /// TypeScript literals that must be the same spellings as the Rust
    /// constants a consumer generates and validates against. A drift in either
    /// direction is a silent hole — the extension echoing a header nobody
    /// checks, or the proxy demanding an echo nobody sends.
    #[test]
    fn the_pi_extension_echoes_the_capture_nonce_contract() {
        let contents = PI_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains(GATEWAY_NONCE_ENV),
            "the asset does not read {GATEWAY_NONCE_ENV}"
        );
        assert!(
            contents.contains(GATEWAY_NONCE_HEADER),
            "the asset does not echo the nonce in {GATEWAY_NONCE_HEADER}"
        );
        // And the echo is a real read-then-send, not just the names appearing:
        // the asset must read the env by the constant's name and place the
        // value under the header's name.
        assert!(
            contents.contains("process.env[GATEWAY_NONCE_ENV]"),
            "the asset does not read the nonce from the environment"
        );
        assert!(
            contents.contains("[GATEWAY_NONCE_HEADER]: nonce"),
            "the asset does not place the nonce value under the header name"
        );
    }

    /// The read must also be a *removal*. Subprocesses the harness spawns
    /// inherit its current environment and already pass the ancestry check, so
    /// a nonce left sitting in `process.env` hands every shell-tool child both
    /// halves of the trust decision. The asset takes the value into its
    /// closure and deletes the variable at load, before any tool can run —
    /// and that delete is as load-bearing as the echo itself, so it is pinned
    /// the same way the spellings are.
    #[test]
    fn the_pi_extension_deletes_the_nonce_from_its_environment_at_load() {
        let contents = PI_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains("delete process.env[GATEWAY_NONCE_ENV]"),
            "the asset does not delete the nonce from its environment; \
             shell-tool subprocesses would inherit the secret"
        );
        // The delete must come after the one read into the closure — a delete
        // alone would silence the echo entirely.
        let read = contents
            .find("process.env[GATEWAY_NONCE_ENV]")
            .unwrap_or(usize::MAX);
        let delete = contents
            .find("delete process.env[GATEWAY_NONCE_ENV]")
            .unwrap_or(0);
        assert!(
            read < delete,
            "the asset must capture the nonce before deleting it"
        );
    }

    /// Fail closed on an unset expectation, and match only the exact echo.
    #[test]
    fn nonce_matching_is_exact_and_never_matches_an_empty_expectation() {
        assert!(nonce_matches("abc123", "abc123"));
        assert!(!nonce_matches("abc123", "abc124"));
        assert!(!nonce_matches("abc123", "abc12"));
        assert!(!nonce_matches("abc123", ""));
        // A consumer that never generated a nonce must not accept an empty
        // echo as a match — that would turn a misconfiguration into a bypass.
        assert!(!nonce_matches("", ""));
        assert!(!nonce_matches("", "abc123"));
    }

    /// pi stamps its own envelope, so the header names in the asset are the
    /// crate's `X-Tapes-*` contract expressed in TypeScript. If
    /// [`crate::envelope`] renames one, ingest would stop recognising pi's
    /// self-attribution and its sessions would silently file as `unknown`.
    #[test]
    fn the_pi_extension_stamps_the_envelope_this_crate_defines() {
        let lowered = PI_GATEWAY_EXTENSION.contents().to_ascii_lowercase();
        assert!(
            lowered.contains(&format!("\"{X_TAPES_HARNESS_ID}\": \"{HARNESS_ID_PI}\"")),
            "the asset does not stamp {X_TAPES_HARNESS_ID}: {HARNESS_ID_PI}"
        );
        assert!(
            lowered.contains(X_TAPES_HARNESS_SESSION_ID),
            "the asset does not stamp {X_TAPES_HARNESS_SESSION_ID}"
        );
    }

    /// A default endpoint is the specific branding failure that made the asset
    /// un-shippable before: it pointed at one product's daemon port, so every
    /// pi session on the machine was redirected there whether or not anything
    /// was capturing. Absence of a loopback literal is the cheapest durable
    /// check that it has not come back.
    #[test]
    fn the_pi_extension_has_no_built_in_endpoint() {
        let contents = PI_GATEWAY_EXTENSION.contents();
        for literal in ["127.0.0.1:", "localhost:", "http://127.0.0.1"] {
            assert!(
                !contents.contains(literal),
                "the asset hard-codes {literal:?}; it must be inert without {GATEWAY_URL_ENV}"
            );
        }
    }

    /// Only a harness declared as needing a bundled plugin may reach one, and
    /// one that declares it must actually have one. Either half broken means
    /// `plugin install` and the registry disagree about what a harness needs.
    #[test]
    fn artifacts_are_declared_exactly_where_the_registry_says() {
        for harness in REGISTRY {
            match harness.plugin() {
                PluginDelivery::None => assert!(
                    harness.plugin_artifacts().is_empty(),
                    "{} needs no plugin but ships artifacts",
                    harness.id(),
                ),
                PluginDelivery::BundledExtension(_) => assert!(
                    !harness.plugin_artifacts().is_empty(),
                    "{} declares a bundled extension with no artifacts",
                    harness.id(),
                ),
            }
        }
    }

    /// Two harnesses installing the same path would have the second install
    /// overwrite the first, which no caller could detect.
    #[test]
    fn no_two_artifacts_install_to_the_same_path() {
        let home = Path::new("/home/u");
        let mut paths: Vec<PathBuf> = all_artifacts()
            .iter()
            .map(|artifact| artifact.install_path(home))
            .collect();
        let total = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), total, "two artifacts share an install path");
    }
}
