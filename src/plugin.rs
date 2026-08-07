//! Plugin artifacts — the files that must be installed *into* a harness before
//! its traffic can be captured at all.
//!
//! Most harnesses need nothing here: capture works by pointing the harness's
//! base-URL knob at a proxy, which [`crate::launch`] plans. A harness with no
//! such knob needs code running inside it instead, and that code is an asset
//! somebody has to write to disk. This module owns those assets, and — because
//! *how many* copies of an asset end up in a harness's auto-discovery directory
//! is a correctness property, not a packaging detail — it owns the install too,
//! through [`PluginArtifact::install`].
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
//! # …and what a consumer may still choose
//!
//! De-branding is not the same as having nothing to say. A consumer's status
//! label and the command it tells a user to run are legitimately its own, and a
//! consumer that had to fork a whole asset to express them would be back where
//! this module started.
//!
//! An asset resolves that at **runtime**, not by being rendered: the launching
//! consumer sets those strings in the environment of the launch it owns, and
//! the asset reads them (see [`pi`] for pi's three). Rendering per consumer is
//! the thing this module now refuses for a file-copy artifact, and for a
//! structural reason — a rendered asset is one *file per product*, and a
//! harness that auto-loads every file in a directory then loads two of them
//! into one process, where they contend over the launch nonce and over the
//! provider registrations and silently unattribute both products' sessions.
//! One artifact, one path, identical bytes is what makes that second reader
//! impossible rather than merely coordinated.
//!
//! [`codex_app`] is still rendered, and can be: its manifests are installed by
//! the harness's own plugin manager into a per-consumer plugin, not copied into
//! a directory something globs.
//!
//! # The environment contract
//!
//! An installed artifact is inert until the launching consumer sets
//! [`GATEWAY_URL_ENV`]. That is deliberate: an artifact installs globally into
//! the harness's own extension directory, so it loads for every session on the
//! machine — including sessions nobody is capturing. Making the redirect
//! conditional on the environment is what keeps an install from changing the
//! behaviour of sessions the user did not launch under capture.
//!
//! The names are shared across consumers, and that is safe for exactly the
//! reason above: one installed artifact means one reader per harness, so there
//! is nothing to collide with. Per-consumer variable names buy nothing once the
//! second copy is gone, and cost a launcher that can set a variable its
//! installed asset does not read.

use std::path::{Path, PathBuf};

pub mod codex_app;
pub mod pi;
mod slots;

// The capture-gateway environment contract and the launch-nonce protocol moved
// to `tapes-capture`. They are protocol, not artifact: adding a harness does not
// change a constant there, and an in-harness extension is written *against* the
// contract rather than being part of it. Keeping the two in one file is what let
// a protocol change ride along with an artifact change.
//
// Re-exported at their original paths so a consumer pinning this crate by git
// rev is not forced to move in lockstep; the canonical spelling is
// `tapes_capture::gateway::…`.
pub use tapes_capture::gateway::{
    GATEWAY_NONCE_ENV, GATEWAY_NONCE_HEADER, GATEWAY_SCHEMA_ENV, GATEWAY_URL_ENV, nonce_matches,
};

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
    superseded_file_names: &'static [&'static str],
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

    /// File names in this artifact's own install directory that a previous
    /// release of *some* client wrote, and that installing this artifact must
    /// remove.
    ///
    /// Only meaningful for a harness that loads a directory rather than a file:
    /// there a superseded copy is not merely stale, it is a second reader, and
    /// it keeps running the behaviour this artifact replaced. pi is that
    /// harness, and its list names a file another client shipped — which is the
    /// whole reason the list is crate-owned. A client can be expected to know
    /// what *it* used to install; it cannot be expected to know what its
    /// competitor did, and removing only one's own leaves the collision intact
    /// from the other direction.
    ///
    /// This is the one place a vendor's name may appear in this module. It is
    /// not carried into anything installed — it names bytes being deleted, not
    /// bytes being written — and the vendor-neutrality bar on
    /// [`PluginArtifact::contents`] is unaffected.
    #[must_use]
    pub const fn superseded_file_names(&self) -> &'static [&'static str] {
        self.superseded_file_names
    }

    /// The paths [`Self::install`] removes, beneath `home`.
    ///
    /// Exposed for a consumer that owns its own write path — a content-keyed
    /// refresh, say — and needs the removal without the write.
    #[must_use]
    pub fn superseded_paths(&self, home: &Path) -> Vec<PathBuf> {
        let dir = self.install_dir(home);
        self.superseded_file_names
            .iter()
            .map(|name| dir.join(name))
            .collect()
    }

    /// The path this artifact stages its bytes at, in `dir`, before renaming
    /// them onto the name the harness loads.
    ///
    /// The name is deliberately not one the harness's glob matches, and it is
    /// deliberately a sibling of the destination — which is what makes the
    /// final rename a within-filesystem one, and so atomic.
    fn staged_path(&self, dir: &Path) -> PathBuf {
        dir.join(format!(".{}.{}.tmp", self.file_name, std::process::id()))
    }

    /// Write this artifact beneath `home`, creating its directory and removing
    /// every superseded sibling. Returns the path written.
    ///
    /// The removal is not tidiness. A harness that auto-discovers a whole
    /// directory loads a superseded copy alongside this one, and two copies of
    /// a capture extension in one process destroy each other's attribution —
    /// which means an install that only *wrote* would leave an upgrading user
    /// exactly as broken as before, with the new bytes on disk to prove the fix
    /// had shipped. Writing and removing therefore belong to one operation, not
    /// to each consumer's good intentions.
    ///
    /// Belonging to one operation is a claim about the failures too, and it
    /// constrains the order — because the state that must never be reached is
    /// *both files present*, and writing first reaches it the moment a removal
    /// fails. So the bytes are staged first under a name the harness's glob
    /// cannot match ([`Self::staged_path`]), the superseded siblings are
    /// removed second, and the staged file is renamed onto its final name last.
    /// Each way that can fail leaves at most one extension where the harness
    /// looks:
    ///
    /// - staging fails — nothing on disk changed;
    /// - a superseded sibling exists and cannot be removed — the staged bytes
    ///   are discarded and the error returned, so the user is left with the old
    ///   copy still working rather than with a second reader;
    /// - the rename fails — the superseded copy is gone and the new file never
    ///   arrived, so capture is off, loudly, instead of on and silently
    ///   unattributed.
    ///
    /// Staging under a non-matching name buys a second thing: a harness reads
    /// that directory every time it starts a session, not once when an
    /// installer runs, so a session starting mid-write must not be able to find
    /// a half-written file spelled like something it loads.
    ///
    /// A superseded file that is absent is not an error. One that exists and
    /// cannot be removed is: the caller has to know that the harness will still
    /// load it, and is better placed than this crate to decide whether that
    /// fails the launch or warns.
    ///
    /// # Errors
    ///
    /// Any I/O failure creating the directory, staging the bytes, removing a
    /// superseded sibling that exists, or renaming the staged file into place.
    /// An error after staging takes the staged file with it, so a failed
    /// install never leaves debris in a directory the harness reads — but a
    /// rename that failed has already removed the superseded copies, and the
    /// caller is being told that nothing is installed.
    pub fn install(&self, home: &Path) -> std::io::Result<PathBuf> {
        let dir = self.install_dir(home);
        std::fs::create_dir_all(&dir)?;
        let staged = self.staged_path(&dir);
        std::fs::write(&staged, self.contents)?;
        for superseded in self.superseded_paths(home) {
            match std::fs::remove_file(&superseded) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    drop(std::fs::remove_file(&staged));
                    return Err(error);
                }
            }
        }
        let path = dir.join(self.file_name);
        if let Err(error) = std::fs::rename(&staged, &path) {
            drop(std::fs::remove_file(&staged));
            return Err(error);
        }
        Ok(path)
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
/// pi auto-discovers global extensions from `~/.pi/agent/extensions/*.ts` — it
/// loads *every* file there, into one process — so installing the file is the
/// whole installation, and the number of files is part of the contract.
///
/// These bytes are what every client installs, to this one path. They used to
/// be one rendering of a per-consumer template, which put two files in that
/// directory on any machine with two clients: both registered the same
/// providers, the second to load found the launch nonce already consumed and
/// registered without the echo, and both products' sessions filed as `unknown`.
/// What a product says differently it now says through the environment of its
/// own launch — see [`pi`].
///
/// [`Self::superseded_file_names`] carries the branded name that model left on
/// disk, because an upgrading user has one and pi would go on loading it.
pub const PI_GATEWAY_EXTENSION: PluginArtifact = PluginArtifact {
    file_name: "tapes-gateway.ts",
    install_dir: &[".pi", "agent", "extensions"],
    superseded_file_names: &["paper-gateway.ts"],
    contents: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/pi/tapes-gateway.ts"
    )),
};

/// The artifact set for a harness captured by a bundled pi extension.
pub(crate) const PI_ARTIFACTS: &[PluginArtifact] = &[PI_GATEWAY_EXTENSION];

/// opencode's capture plugin.
///
/// opencode *can* be redirected without one — its provider endpoints live in a
/// JSON config file, which is what [`crate::launch::OpenCodeRecipe`] plans —
/// but a config file cannot attribute: opencode publishes no PID-indexed
/// session file, so a redirected session's turns land under
/// `harness_id: unknown`. This plugin is what closes that gap. It does both
/// halves from inside the harness: a `config` hook points the captured
/// providers at the proxy named by [`GATEWAY_URL_ENV`], and a `chat.headers`
/// hook stamps the `X-Tapes-*` envelope with opencode's own session id plus
/// the [`GATEWAY_NONCE_HEADER`] echo — which is what makes opencode the second
/// [`crate::harness::AttributionStrategy::SelfAttributing`] harness.
///
/// opencode auto-discovers plugins by globbing `{plugin,plugins}/*.{ts,js}`
/// under its global config directory (`~/.config/opencode`) and the project's
/// `.opencode/`, so installing the file is the whole installation. The
/// documented spelling of the directory is `plugins`, which is the one used
/// here. The one soft spot is the root: opencode resolves its config directory
/// through `$XDG_CONFIG_HOME`, and this artifact's destination is the
/// *default* resolution of that variable — a user who relocated it installs by
/// hand, exactly as they already do for every other opencode plugin.
pub const OPENCODE_GATEWAY_EXTENSION: PluginArtifact = PluginArtifact {
    file_name: "tapes-gateway.ts",
    install_dir: &[".config", "opencode", "plugins"],
    // Never rendered per consumer, so no client ever wrote a differently-named
    // copy of it and there is nothing to supersede. This is the shape pi has
    // now been given.
    superseded_file_names: &[],
    contents: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/opencode/tapes-gateway.ts"
    )),
};

/// The artifact set for a harness captured by the bundled opencode plugin.
pub(crate) const OPENCODE_ARTIFACTS: &[PluginArtifact] = &[OPENCODE_GATEWAY_EXTENSION];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::envelope::{
        HARNESS_ID_OPENCODE, HARNESS_ID_PI, X_TAPES_HARNESS_ID, X_TAPES_HARNESS_SESSION_ID,
    };
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
        assert!(all_artifacts().contains(&&OPENCODE_GATEWAY_EXTENSION));
    }

    /// An artifact's destination components are joined onto a caller-supplied
    /// home. A component carrying a separator or a `..` would let that join
    /// leave the home directory — the installer canonicalises and contains, but
    /// the crate must not hand it something designed to escape in the first
    /// place.
    ///
    /// Superseded names are held to the same bar, and more urgently: they are
    /// joined the same way and then handed to `remove_file`, so a traversing
    /// component there deletes something outside the harness's own directory.
    #[test]
    fn no_artifact_path_component_can_leave_the_home_directory() {
        for artifact in all_artifacts() {
            let components = artifact
                .install_dir_components()
                .iter()
                .chain(std::iter::once(&artifact.file_name()))
                .chain(artifact.superseded_file_names())
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

    /// **The PCC-1125 property.** pi loads every file in its extension
    /// directory into one process, so two clients installing "their" copy is
    /// two readers contending over one launch's nonce and over the same
    /// provider registrations — and the loser registers anyway, unattributed.
    /// The fix is that there is nothing per-client to install: whoever installs
    /// writes these bytes, to this path.
    ///
    /// Two homes stand in for two clients. The relative destination and the
    /// written bytes must match, because a difference in either is a second
    /// file in that directory.
    #[test]
    fn every_client_installs_identical_bytes_to_one_path() {
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let one = PI_GATEWAY_EXTENSION.install(first.path()).unwrap();
        let two = PI_GATEWAY_EXTENSION.install(second.path()).unwrap();

        assert_eq!(
            one.strip_prefix(first.path()),
            two.strip_prefix(second.path()),
            "two installs disagree about where the pi extension goes"
        );
        assert_eq!(
            std::fs::read_to_string(&one).unwrap(),
            std::fs::read_to_string(&two).unwrap(),
            "two installs wrote different bytes; a second reader can exist again"
        );
        assert_eq!(
            std::fs::read_to_string(&one).unwrap(),
            PI_GATEWAY_EXTENSION.contents(),
        );
    }

    /// **The migration, and the half without which the fix reaches nobody.**
    /// Every user who ran an older `paper` has `paper-gateway.ts` sitting in
    /// pi's extension directory. Writing the new file next to it leaves two
    /// extensions loaded and the bug exactly as it was — with the fix installed,
    /// which is worse than not shipping it. So installing removes it.
    #[test]
    fn installing_the_pi_extension_removes_a_superseded_branded_copy() {
        let home = tempfile::tempdir().unwrap();
        let dir = PI_GATEWAY_EXTENSION.install_dir(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        let superseded = dir.join("paper-gateway.ts");
        std::fs::write(&superseded, "// an older client's rendering\n").unwrap();

        let installed = PI_GATEWAY_EXTENSION.install(home.path()).unwrap();

        assert!(
            !superseded.exists(),
            "the superseded extension survived the install; pi would load both"
        );
        assert_eq!(
            std::fs::read_to_string(&installed).unwrap(),
            PI_GATEWAY_EXTENSION.contents(),
        );
        // …and nothing else in the directory was touched: the removal is a
        // named list, not a sweep of a directory the user also puts their own
        // extensions in.
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "installing removed or added something it was not asked to"
        );
    }

    /// **The ordering, in the direction that bites.** Writing the new file
    /// first and removing second means a removal that fails leaves *both*
    /// extensions in the directory — the precise state this artifact exists to
    /// prevent, arrived at by the code meant to prevent it, and with the new
    /// bytes on disk to argue the fix had shipped. Installing must instead fail
    /// with the destination still empty, leaving the user the old copy that at
    /// least works.
    ///
    /// The removal is blocked with a rule of the filesystem rather than a
    /// permission bit: `remove_file` refuses a non-empty directory whoever is
    /// asking, whereas CI runs as root, where a read-only file proves nothing.
    #[test]
    fn a_superseded_copy_that_cannot_be_removed_leaves_nothing_installed() {
        let home = tempfile::tempdir().unwrap();
        let dir = PI_GATEWAY_EXTENSION.install_dir(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        // A non-empty directory standing where the superseded file goes: not
        // removable by anyone, root included.
        let superseded = dir.join("paper-gateway.ts");
        std::fs::create_dir_all(&superseded).unwrap();
        std::fs::write(superseded.join("occupant"), "unremovable\n").unwrap();

        let error = PI_GATEWAY_EXTENSION.install(home.path()).unwrap_err();
        assert_ne!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "the blocker was not in place; the test proves nothing"
        );

        assert!(
            !PI_GATEWAY_EXTENSION.install_path(home.path()).exists(),
            "the install wrote its extension anyway, so pi would load two"
        );
        assert!(
            superseded.exists(),
            "the blocker vanished; the removal did not actually fail"
        );
        // …and the staged bytes went with the error, rather than sitting in a
        // directory the harness reads on every session start.
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "a failed install left debris in the extension directory"
        );
    }

    /// The staged file exists for as long as the write takes, in a directory
    /// the harness globs every time it starts a session — so its name must not
    /// be one of the names that glob picks up. pi loads `*.ts` and opencode
    /// `*.{ts,js}`; a partially written file spelled either way is an extension
    /// the harness will happily load half of.
    ///
    /// Staging as a sibling is the other half: rename is only atomic within a
    /// filesystem, and only a sibling is guaranteed to be on the same one.
    #[test]
    fn the_staged_name_is_not_one_a_harness_loads() {
        let dir = Path::new("/home/u/.pi/agent/extensions");
        for artifact in all_artifacts() {
            let staged = artifact.staged_path(dir);
            let name = staged.file_name().unwrap().to_str().unwrap();
            assert!(
                !name.ends_with(".ts"),
                "{name:?} would be auto-loaded as an extension mid-write"
            );
            assert!(
                !name.ends_with(".js"),
                "{name:?} would be auto-loaded as a plugin mid-write"
            );
            assert_ne!(
                name,
                artifact.file_name(),
                "staging onto the destination is not staging at all"
            );
            assert_eq!(
                staged.parent(),
                Some(dir),
                "the staged file must be a sibling of its destination"
            );
        }
    }

    /// The list is named, so it has to actually name the file the bug is about.
    /// The test above would pass just as happily against an empty list if it
    /// created no superseded file.
    #[test]
    fn the_pi_artifact_names_the_branded_copy_an_upgrading_user_has() {
        assert!(
            PI_GATEWAY_EXTENSION
                .superseded_file_names()
                .contains(&"paper-gateway.ts"),
            "nothing removes the file an older paper installed"
        );
        assert_eq!(
            PI_GATEWAY_EXTENSION.superseded_paths(Path::new("/home/u")),
            vec![PathBuf::from(
                "/home/u/.pi/agent/extensions/paper-gateway.ts"
            )],
        );
    }

    /// A first install, onto a machine that has neither the directory nor a
    /// superseded copy, is the ordinary case and must not error on the absent
    /// file it was told to remove.
    #[test]
    fn installing_creates_the_directory_and_tolerates_nothing_to_supersede() {
        let home = tempfile::tempdir().unwrap();
        let installed = PI_GATEWAY_EXTENSION.install(home.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&installed).unwrap(),
            PI_GATEWAY_EXTENSION.contents(),
        );
    }

    /// An artifact that superseded its own file name would delete what it had
    /// just written, leaving the harness with no extension at all — and the
    /// symptom (nothing captured) looks nothing like the cause.
    #[test]
    fn no_artifact_supersedes_the_file_it_installs() {
        for artifact in all_artifacts() {
            assert!(
                !artifact
                    .superseded_file_names()
                    .contains(&artifact.file_name()),
                "{} would delete itself on install",
                artifact.file_name(),
            );
        }
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
    ///
    /// Pinned as the whole `const … = "…";` declaration rather than as a
    /// substring. A per-product namespacing of these names was the shape of an
    /// earlier attempt at PCC-1125, and a `contains` accepts an asset that
    /// keeps such a name *alongside* the shared one — which is a launcher and
    /// an extension agreeing on a variable nobody else sets.
    #[test]
    fn the_pi_extension_reads_the_gateway_environment_contract() {
        let contents = PI_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains(&format!("const GATEWAY_URL_ENV = \"{GATEWAY_URL_ENV}\";")),
            "the asset does not read {GATEWAY_URL_ENV}"
        );
        assert!(
            contents.contains(&format!(
                "const GATEWAY_SCHEMA_ENV = \"{GATEWAY_SCHEMA_ENV}\";"
            )),
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
            contents.contains(&format!(
                "const GATEWAY_NONCE_ENV = \"{GATEWAY_NONCE_ENV}\";"
            )),
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

    // --- the opencode plugin, pinned the same way the pi extension is -------
    //
    // The two assets implement one environment contract against two different
    // extension APIs, so each carries its own copy of the spellings and each
    // copy is pinned independently: a drift in either asset is a silently
    // uncaptured (or silently unattributed) harness, not a build failure.

    #[test]
    fn opencode_installs_where_opencode_discovers_plugins() {
        // opencode globs `{plugin,plugins}/*.{ts,js}` beneath its config
        // directory; `plugins` is the documented spelling. A destination
        // outside that glob is an installed file opencode never loads.
        let home = Path::new("/home/u");
        assert_eq!(
            OPENCODE_GATEWAY_EXTENSION.install_path(home),
            PathBuf::from("/home/u/.config/opencode/plugins/tapes-gateway.ts"),
        );
    }

    /// The asset reads the environment by name, so the Rust constant and the
    /// literal in the asset are two spellings of one contract — same
    /// reasoning as the pi test above, pinned against the opencode copy.
    #[test]
    fn the_opencode_plugin_reads_the_gateway_environment_contract() {
        let contents = OPENCODE_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains(GATEWAY_URL_ENV),
            "the asset does not read {GATEWAY_URL_ENV}"
        );
        assert!(
            contents.contains(GATEWAY_SCHEMA_ENV),
            "the asset does not read {GATEWAY_SCHEMA_ENV}"
        );
    }

    /// The nonce contract, asset-side: read from the environment, echoed in
    /// the header, both under the crate's spellings.
    #[test]
    fn the_opencode_plugin_echoes_the_capture_nonce_contract() {
        let contents = OPENCODE_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains(GATEWAY_NONCE_ENV),
            "the asset does not read {GATEWAY_NONCE_ENV}"
        );
        assert!(
            contents.contains(GATEWAY_NONCE_HEADER),
            "the asset does not echo the nonce in {GATEWAY_NONCE_HEADER}"
        );
        // And the echo is a real read-then-send: the asset reads the env by
        // the constant's name and places the value under the header's name.
        assert!(
            contents.contains("process.env[GATEWAY_NONCE_ENV]"),
            "the asset does not read the nonce from the environment"
        );
        assert!(
            contents.contains("output.headers[GATEWAY_NONCE_HEADER] = nonce"),
            "the asset does not place the nonce value under the header name"
        );
    }

    /// The read must also be a *removal*, before any tool can run — the same
    /// property pinned for pi, load-bearing for the same reason: shell-tool
    /// children inherit the current environment and already pass the ancestry
    /// check, so a lingering variable hands them both halves of the trust
    /// decision. This asset does the read-and-delete at module load, which is
    /// earlier still than pi's (inside the exported function).
    #[test]
    fn the_opencode_plugin_deletes_the_nonce_from_its_environment_at_load() {
        let contents = OPENCODE_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains("delete process.env[GATEWAY_NONCE_ENV]"),
            "the asset does not delete the nonce from its environment; \
             shell-tool subprocesses would inherit the secret"
        );
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

    /// opencode stamps its own envelope, so the header names in the asset are
    /// the crate's `X-Tapes-*` contract expressed in TypeScript — a rename in
    /// [`crate::envelope`] must fail here, not silently re-file opencode's
    /// sessions as `unknown`.
    #[test]
    fn the_opencode_plugin_stamps_the_envelope_this_crate_defines() {
        let lowered = OPENCODE_GATEWAY_EXTENSION.contents().to_ascii_lowercase();
        assert!(
            lowered.contains(&format!(
                "\"{X_TAPES_HARNESS_ID}\": \"{HARNESS_ID_OPENCODE}\""
            )),
            "the asset does not stamp {X_TAPES_HARNESS_ID}: {HARNESS_ID_OPENCODE}"
        );
        assert!(
            lowered.contains(X_TAPES_HARNESS_SESSION_ID),
            "the asset does not stamp {X_TAPES_HARNESS_SESSION_ID}"
        );
    }

    /// The nonce is a secret shared with the proxy alone, and the stamp runs
    /// per request against whatever endpoint the provider actually resolved:
    /// the asset must gate the echo on the request really routing through the
    /// gateway, or an auth loader that swapped endpoints would carry the
    /// secret to a real upstream.
    #[test]
    fn the_opencode_plugin_stamps_nothing_toward_a_real_upstream() {
        let contents = OPENCODE_GATEWAY_EXTENSION.contents();
        assert!(
            contents.contains("isGatewayAddress(resolvedBaseUrl, baseUrl)"),
            "the asset does not verify the resolved provider endpoint is the \
             gateway before stamping the nonce and envelope"
        );
    }

    /// …and that gate compares URLs, not strings.
    ///
    /// The adversarial sibling of the test above, pinned separately because the
    /// two failures are different sizes. Failing the check above means turns go
    /// unattributed; failing this one means the launch nonce and the session
    /// envelope are handed to an attacker-controlled host. A textual
    /// `resolved.startsWith(baseUrl)` looks like it asks "is this the gateway"
    /// and instead asks "does this begin with those characters", so a gateway at
    /// `https://gw.example` also accepts `https://gw.example.attacker.invalid` —
    /// a different host, a registrable lookalike, and `options.baseURL` is
    /// user-editable config. The asset must therefore compare parsed origins,
    /// and must not carry the prefix test that this replaced.
    #[test]
    fn the_opencode_plugins_gateway_check_is_a_url_boundary_not_a_string_prefix() {
        let contents = OPENCODE_GATEWAY_EXTENSION.contents();
        assert!(
            !contents.contains("startsWith(baseUrl)"),
            "the asset compares the resolved endpoint to the gateway as a string \
             prefix; a lookalike host sharing that prefix would be handed the \
             capture nonce and the session envelope"
        );
        assert!(
            contents.contains("url.origin !== gateway.origin"),
            "the asset does not compare parsed origins, which is what makes the \
             host boundary — scheme, host and port — actually hold"
        );
        // Origin alone would let a gateway mounted at a sub-path accept a
        // sibling of it, so the path is bounded on a separator too.
        assert!(
            contents.contains("url.pathname.startsWith(`${mount}/`)"),
            "the asset does not bound the gateway's mount path on a separator"
        );
    }

    /// Same no-default-endpoint bar as the pi asset: inert without
    /// [`GATEWAY_URL_ENV`], with no loopback literal to fall back on.
    #[test]
    fn the_opencode_plugin_has_no_built_in_endpoint() {
        let contents = OPENCODE_GATEWAY_EXTENSION.contents();
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
                // Templates are rendered, not copied: `plugin_artifacts()` is
                // the file-copy path and must stay empty so an installer does
                // not write un-rendered slots into a harness.
                PluginDelivery::HookManifestTemplates(templates) => {
                    assert!(
                        harness.plugin_artifacts().is_empty(),
                        "{} must not expose templates as copyable artifacts",
                        harness.id(),
                    );
                    assert!(
                        !templates.plugin_manifest.trim().is_empty()
                            && !templates.hooks_manifest.trim().is_empty(),
                        "{} declares empty manifest templates",
                        harness.id(),
                    );
                }
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
