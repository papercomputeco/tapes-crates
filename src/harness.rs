//! The harness registry — the one place a harness is declared.
//!
//! Before this module, a harness's identity was scattered: its id was a loose
//! `const` in [`crate::envelope`] (and, for opencode, a private one in its own
//! launch recipe), the list of names a client would accept lived in each
//! consumer's CLI, the User-Agent gate was a hand-written prefix test inside
//! the attribution pipeline, and which attribution shape a harness had was
//! implicit in which modules happened to exist. Adding a harness meant finding
//! all of those, with nothing to check that you had.
//!
//! A [`Harness`] bundles them into one value, and [`REGISTRY`] holds every one
//! the crate knows about. Everything else derives: [`supported_agents`] is the
//! launchable subset, [`find`] resolves a user-typed name through the aliases,
//! [`for_user_agent`] replaces the inline prefix test, and the recipes take
//! their harness ids from the registry rather than spelling them again.
//!
//! # Declarative on purpose
//!
//! The registry describes a harness; it does not construct one. In particular
//! it holds no [`crate::launch::LaunchRecipe`] instances, because recipes carry
//! their inputs as fields and those inputs differ wildly — Claude needs one
//! endpoint, codex needs an endpoint plus an auth mode and a provider identity,
//! opencode needs an endpoint *per provider* plus a model. A registry that
//! built recipes would need a union of every harness's configuration, which is
//! the shape this crate deliberately avoided when it gave [`LaunchSupport`]'s
//! trait a nullary `plan()`. So the registry records *that* a harness has a
//! recipe, and the consumer constructs it with the arguments only the consumer
//! has.
//!
//! Plugin assets are the one thing the registry does hand out whole. `pi` is
//! captured by an in-harness extension, and unlike a recipe an extension takes
//! no per-consumer inputs — it is a fixed file that reads its endpoint from the
//! environment at runtime. So [`PluginDelivery::BundledExtension`] carries the
//! [`crate::plugin::PluginArtifact`]s themselves, and an installer needs
//! nothing from the registry but the harness name the user typed.
//!
//! # Adding a harness
//!
//! Declare a `const` here and add it to [`REGISTRY`]; the invariant tests at
//! the bottom of this file will tell you what else the declaration implies.
//! `docs/adding-a-harness.md` walks the whole path.

use std::path::PathBuf;

use crate::envelope::{
    HARNESS_ID_CLAUDE, HARNESS_ID_CODEX, HARNESS_ID_CODEX_APP, HARNESS_ID_OPENCODE, HARNESS_ID_PI,
};

/// How a request's `User-Agent` identifies a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UserAgentMatch {
    /// No User-Agent gate: this harness is recognised by other evidence (a
    /// route, a launch marker, or its own envelope).
    None,
    /// Case-insensitive **prefix** of the User-Agent. A prefix rather than a
    /// substring: `some-claude-like` must not match `claude`.
    Prefix(&'static str),
}

impl UserAgentMatch {
    /// Does `ua` identify this harness?
    #[must_use]
    pub fn matches(&self, ua: &str) -> bool {
        match self {
            Self::None => false,
            // `eq_ignore_ascii_case` only compares whole strings, so compare a
            // lower-cased copy. A UA is ~200 bytes in practice; the allocation
            // is negligible against the per-request work that follows.
            Self::Prefix(prefix) => ua.to_ascii_lowercase().starts_with(prefix),
        }
    }
}

/// Whether this crate can plan a launch for a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LaunchSupport {
    /// The crate ships a [`crate::launch::LaunchRecipe`] whose `harness()`
    /// equals this harness's id. The consumer constructs it.
    Recipe,
    /// Launchable, but the consumer plans the launch itself: the harness has no
    /// base-URL knob for a recipe to set, so it is captured by an installed
    /// [`PluginDelivery::BundledExtension`] plus whatever argv the consumer uses
    /// to load it. The asset and its environment contract are shared; the argv
    /// is not, yet.
    ConsumerOwned,
    /// This crate cannot plan a launch for the harness.
    Unsupported,
}

/// How a capture client recovers a harness session's identity.
///
/// This is the distinction the per-harness attribution submodules are grouped
/// by, stated once instead of inferred from which modules exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttributionStrategy {
    /// The harness publishes a PID-indexed session file it keeps current, so
    /// the peer PID of the accepted connection indexes straight to an identity.
    /// See [`crate::attribution::claude`].
    SessionsDir,
    /// The harness publishes nothing by PID; identity is recovered from the
    /// rollout file a live process holds open, filtered by recency and by the
    /// provider the launch configured. See [`crate::attribution::codex`].
    OpenRollout,
    /// The harness stamps its own complete `X-Tapes-*` envelope from inside
    /// itself, via an extension the client cannot see through a peer-PID
    /// lookup.
    ///
    /// This is why [`crate::attribution::Attributed::stamp`] preserves a
    /// complete inbound envelope instead of overwriting it with
    /// `harness_id: unknown`: for a self-attributing harness the inbound
    /// envelope is *better* information than the client's own failure to
    /// attribute, and overwriting it would silently re-file those sessions.
    /// A self-attributing harness contributes no attribution module at all.
    SelfAttributing,
    /// Identity arrives as lifecycle hook reports: an installed hook plugin
    /// runs a consumer-supplied command at session, prompt, stop, and
    /// subagent boundaries, and the command reports the allowlisted evidence
    /// ([`crate::attribution::codex_app::LifecycleObservation`]) to the
    /// consumer's runtime. Peer-PID lookup does not apply — the harness is a
    /// long-lived process the consumer configured rather than launched, so
    /// there is no launched PID to anchor trust on.
    LifecycleHooks,
    /// No client-side attribution: traffic is captured, but sessions land
    /// under `harness_id: unknown` until someone implements a lane.
    None,
}

/// Where a harness's on-disk transcripts live.
///
/// tapes' wire capture yields a complete call inventory but no causal/fork
/// skeleton — that lives only in these trees, which the transcript lane
/// uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TranscriptSource {
    /// `~/.claude/projects/`, whose immediate children are cwd-encoded
    /// directories holding `<sid>.jsonl` plus each session's `subagents/`.
    /// This is the root [`crate::transcript::sweep`] walks.
    ClaudeProjects,
    /// The Codex rollout directory — `$CODEX_HOME/sessions` when set,
    /// otherwise `~/.codex/sessions`. Flat JSONL rollouts rather than a
    /// cwd-partitioned tree, so the Claude sweep does not apply to it.
    CodexRollouts,
    /// No transcript tree this crate knows how to locate.
    None,
}

impl TranscriptSource {
    /// Resolve to an absolute path on this machine.
    ///
    /// `None` when the source is [`Self::None`], or when the home directory is
    /// unavailable (extremely unusual). Resolution reads the environment but
    /// never the filesystem: the directory may not exist yet.
    #[must_use]
    pub fn resolve(&self) -> Option<PathBuf> {
        match self {
            Self::None => None,
            Self::ClaudeProjects => dirs::home_dir().map(|h| h.join(".claude").join("projects")),
            // Delegates so `$CODEX_HOME` is honoured in exactly one place.
            Self::CodexRollouts => crate::attribution::codex::session::default_sessions_dir(),
        }
    }
}

/// Whether capturing a harness needs an artifact installed into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginDelivery {
    /// Capture works by redirecting the harness's traffic; nothing is
    /// installed into the harness itself.
    None,
    /// Capture requires an in-harness extension, which this crate ships: the
    /// variant carries the [`crate::plugin::PluginArtifact`]s a consumer writes
    /// to disk, so `plugin install` is a file copy over crate-owned bytes and
    /// no consumer carries its own drifting fork of the asset.
    ///
    /// This is only available to an asset that names no vendor — see
    /// [`crate::plugin`] for what "vendor-neutral" costs and rules out. A
    /// plugin that genuinely cannot shed its branding stays in the consumer's
    /// repository and gets no variant here.
    BundledExtension(&'static [crate::plugin::PluginArtifact]),
    /// Capture requires a hook plugin installed by the harness's *own* plugin
    /// manager, from a source directory the consumer packages. The crate ships
    /// the plugin's manifests as templates — structure and event set are
    /// crate-owned, while the hook command line and the plugin's user-facing
    /// identity are consumer-rendered slots (see
    /// [`crate::plugin::codex_app::render_hooks_manifest`]). Nothing here is
    /// written beneath the user's home by a `plugin install` file copy, which
    /// is why this is not a [`Self::BundledExtension`] with different bytes.
    HookManifestTemplates(&'static crate::plugin::codex_app::HookPluginTemplates),
}

/// Everything this crate knows about one coding-agent harness.
///
/// Construct only as a `const` in this module: the registry is the complete
/// set, and a value built elsewhere would be a harness nothing else can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Harness {
    id: &'static str,
    aliases: &'static [&'static str],
    user_agent: UserAgentMatch,
    launch: LaunchSupport,
    attribution: AttributionStrategy,
    transcripts: TranscriptSource,
    plugin: PluginDelivery,
}

impl Harness {
    /// The canonical harness id — the `X-Tapes-Harness-Id` value, and the name
    /// a launch recipe reports from `harness()`.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Additional spellings a consumer's CLI should accept for this harness.
    #[must_use]
    pub const fn aliases(&self) -> &'static [&'static str] {
        self.aliases
    }

    /// Does `name` name this harness? Case-insensitive over the id and every
    /// alias, so a consumer need not normalise user input first.
    #[must_use]
    pub fn matches_name(&self, name: &str) -> bool {
        let name = name.trim();
        self.id.eq_ignore_ascii_case(name)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    }

    /// The User-Agent rule that identifies this harness's traffic.
    #[must_use]
    pub const fn user_agent(&self) -> UserAgentMatch {
        self.user_agent
    }

    /// Does this request's User-Agent identify this harness?
    #[must_use]
    pub fn matches_user_agent(&self, ua: &str) -> bool {
        self.user_agent.matches(ua)
    }

    /// Whether this crate can plan a launch for the harness.
    #[must_use]
    pub const fn launch(&self) -> LaunchSupport {
        self.launch
    }

    /// Whether a consumer can launch this harness at all — through a shared
    /// recipe or its own assets. [`supported_agents`] is this predicate over
    /// the registry.
    #[must_use]
    pub const fn is_launchable(&self) -> bool {
        !matches!(self.launch, LaunchSupport::Unsupported)
    }

    /// How a capture client recovers this harness's session identity.
    #[must_use]
    pub const fn attribution(&self) -> AttributionStrategy {
        self.attribution
    }

    /// Where this harness's on-disk transcripts live.
    #[must_use]
    pub const fn transcripts(&self) -> TranscriptSource {
        self.transcripts
    }

    /// The transcript tree's absolute path on this machine, if any.
    #[must_use]
    pub fn transcript_root(&self) -> Option<PathBuf> {
        self.transcripts.resolve()
    }

    /// Whether capture needs an artifact installed into the harness.
    #[must_use]
    pub const fn plugin(&self) -> PluginDelivery {
        self.plugin
    }

    /// The files a consumer must install into this harness before its traffic
    /// can be captured, empty when it needs none.
    ///
    /// This is the whole input to a `plugin install`: resolve a user-typed name
    /// through [`find`], take this slice, write each artifact beneath the
    /// user's home. An empty slice is the ordinary case and not an error — it
    /// says capture needs no cooperation from inside the harness.
    #[must_use]
    pub const fn plugin_artifacts(&self) -> &'static [crate::plugin::PluginArtifact] {
        match self.plugin {
            // Templates are deliberately not artifacts: they carry un-rendered
            // slots, so a file-copy installer must see nothing to copy.
            PluginDelivery::None | PluginDelivery::HookManifestTemplates(_) => &[],
            PluginDelivery::BundledExtension(artifacts) => artifacts,
        }
    }
}

/// Claude Code — the reference harness, and the only one that publishes a
/// PID-indexed session file.
pub const CLAUDE: Harness = Harness {
    id: HARNESS_ID_CLAUDE,
    aliases: &["claude-code"],
    user_agent: UserAgentMatch::Prefix("claude"),
    launch: LaunchSupport::Recipe,
    attribution: AttributionStrategy::SessionsDir,
    transcripts: TranscriptSource::ClaudeProjects,
    plugin: PluginDelivery::None,
};

/// OpenAI Codex. Identified by route and launch marker rather than by
/// User-Agent: its SDK's User-Agent is not harness-specific, so a prefix test
/// would either miss real traffic or claim someone else's.
pub const CODEX: Harness = Harness {
    id: HARNESS_ID_CODEX,
    aliases: &[],
    user_agent: UserAgentMatch::None,
    launch: LaunchSupport::Recipe,
    attribution: AttributionStrategy::OpenRollout,
    transcripts: TranscriptSource::CodexRollouts,
    plugin: PluginDelivery::None,
};

/// The Codex desktop app — a distinct harness, not a spelling of [`CODEX`].
///
/// The app is a long-lived Codex host the consumer *configures* rather than
/// launches: its provider settings live in the shared `$CODEX_HOME/config.toml`
/// (which the consumer's installer patches to point at a capture proxy), so
/// [`LaunchSupport::Unsupported`] — a harness you can capture but not start.
/// It speaks Codex's wire protocol and writes rollouts to the same tree, but
/// its session identity arrives through lifecycle hook reports from an
/// installed plugin ([`crate::attribution::codex_app`]) rather than through a
/// peer-PID lane, and the plugin's manifests are crate-owned templates the
/// consumer renders ([`crate::plugin::codex_app`]) and Codex's own plugin
/// manager installs.
pub const CODEX_APP: Harness = Harness {
    id: HARNESS_ID_CODEX_APP,
    aliases: &["codex-desktop"],
    user_agent: UserAgentMatch::None,
    launch: LaunchSupport::Unsupported,
    attribution: AttributionStrategy::LifecycleHooks,
    transcripts: TranscriptSource::CodexRollouts,
    plugin: PluginDelivery::HookManifestTemplates(&crate::plugin::codex_app::CODEX_APP_TEMPLATES),
};

/// opencode. Launchable through a shared recipe, but no attribution lane
/// exists yet — an honest partial entry, and the shape most new harnesses
/// start in.
pub const OPENCODE: Harness = Harness {
    id: HARNESS_ID_OPENCODE,
    aliases: &[],
    user_agent: UserAgentMatch::None,
    launch: LaunchSupport::Recipe,
    attribution: AttributionStrategy::None,
    transcripts: TranscriptSource::None,
    plugin: PluginDelivery::None,
};

/// pi — the self-attributing shape.
///
/// pi carries a capture extension that stamps a complete `X-Tapes-*` envelope
/// from inside the harness, so there is no attribution lane and no session
/// file to read: the client's job is to *preserve* what arrives. That extension
/// is [`crate::plugin::PI_GATEWAY_EXTENSION`], which this crate now ships.
///
/// It stays [`LaunchSupport::ConsumerOwned`] even so. Installing the extension
/// and *launching* pi under capture are separate steps, and only the first one
/// has moved: a recipe would still have to plan the argv that points pi at an
/// installed extension, which nothing here does yet.
pub const PI: Harness = Harness {
    id: HARNESS_ID_PI,
    aliases: &[],
    user_agent: UserAgentMatch::None,
    launch: LaunchSupport::ConsumerOwned,
    attribution: AttributionStrategy::SelfAttributing,
    transcripts: TranscriptSource::None,
    plugin: PluginDelivery::BundledExtension(crate::plugin::PI_ARTIFACTS),
};

/// Every harness this crate knows about.
///
/// Order is the order a consumer should present them in: the harnesses with
/// full capture support first (with the Codex app beside the Codex CLI it is
/// a sibling of), then the partial entries.
pub const REGISTRY: &[Harness] = &[CLAUDE, CODEX, CODEX_APP, OPENCODE, PI];

/// Every registered harness.
#[must_use]
pub fn all() -> &'static [Harness] {
    REGISTRY
}

/// Resolve a user-typed harness name through ids and aliases.
///
/// Case-insensitive and whitespace-trimming, so a consumer can pass a CLI
/// argument straight in.
#[must_use]
pub fn find(name: &str) -> Option<&'static Harness> {
    REGISTRY.iter().find(|harness| harness.matches_name(name))
}

/// Resolve a request's `User-Agent` to the harness it identifies.
///
/// `None` for a User-Agent no harness claims — a loopback `curl`, a health
/// probe, or a harness whose lane is selected by other evidence.
#[must_use]
pub fn for_user_agent(ua: &str) -> Option<&'static Harness> {
    REGISTRY
        .iter()
        .find(|harness| harness.matches_user_agent(ua))
}

/// The harnesses a consumer can launch, in registry order.
///
/// This is the list each consumer's `start` command should offer, derived
/// rather than restated: paper's `SUPPORTED_AGENTS` and tapesctl's harness
/// argument both come from here, so a harness added to [`REGISTRY`] appears in
/// both without either being edited.
#[must_use]
pub fn supported_agents() -> Vec<&'static str> {
    REGISTRY
        .iter()
        .filter(|harness| harness.is_launchable())
        .map(Harness::id)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// Two harnesses whose User-Agent rules can both match one request would
    /// make [`for_user_agent`] order-dependent — today only Claude declares a
    /// prefix, and this pins that any future prefix stays disjoint: for two
    /// prefixes, "disjoint" means neither may be a prefix of the other, since
    /// any UA matching the longer necessarily matches the shorter.
    #[test]
    fn user_agent_rules_are_pairwise_disjoint() {
        let prefixes: Vec<&str> = REGISTRY
            .iter()
            .filter_map(|harness| match harness.user_agent() {
                UserAgentMatch::Prefix(prefix) => Some(prefix),
                UserAgentMatch::None => None,
            })
            .collect();
        for (i, a) in prefixes.iter().enumerate() {
            for b in prefixes.iter().skip(i + 1) {
                assert!(
                    !a.starts_with(b) && !b.starts_with(a),
                    "User-Agent prefixes {a:?} and {b:?} overlap; \
                     for_user_agent would resolve by registry order"
                );
            }
        }
    }

    /// Two harnesses answering to one name would make [`find`] order-dependent
    /// — the failure mode a registry exists to prevent.
    #[test]
    fn every_name_in_the_registry_is_unique() {
        let mut names: Vec<String> = Vec::new();
        for harness in REGISTRY {
            names.push(harness.id().to_ascii_lowercase());
            names.extend(harness.aliases().iter().map(|a| a.to_ascii_lowercase()));
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate name in: {names:?}");
    }

    /// The registry id is the on-wire `X-Tapes-Harness-Id`. If a harness were
    /// declared with an id the envelope never emits, a launched session and
    /// its captured traffic would disagree about the harness's name.
    #[test]
    fn registry_ids_are_the_envelope_ids() {
        assert_eq!(CLAUDE.id(), crate::envelope::HARNESS_ID_CLAUDE);
        assert_eq!(CODEX.id(), crate::envelope::HARNESS_ID_CODEX);
        assert_eq!(CODEX_APP.id(), crate::envelope::HARNESS_ID_CODEX_APP);
        assert_eq!(OPENCODE.id(), crate::envelope::HARNESS_ID_OPENCODE);
        assert_eq!(PI.id(), crate::envelope::HARNESS_ID_PI);
        // And none of them collides with the miss sentinel.
        for harness in REGISTRY {
            assert_ne!(harness.id(), crate::envelope::HARNESS_ID_UNKNOWN);
        }
    }

    #[test]
    fn names_resolve_case_insensitively_through_aliases() {
        assert_eq!(find("claude").map(Harness::id), Some("claude"));
        assert_eq!(find("CLAUDE").map(Harness::id), Some("claude"));
        assert_eq!(find("  claude-code  ").map(Harness::id), Some("claude"));
        assert_eq!(find("codex").map(Harness::id), Some("codex"));
        // The app is a distinct harness, not a spelling of codex — and
        // vice-versa: neither name can drift into resolving as the other.
        assert_eq!(find("codex-app").map(Harness::id), Some("codex-app"));
        assert_eq!(find("codex-desktop").map(Harness::id), Some("codex-app"));
        assert!(find("gemini").is_none());
        assert!(find("").is_none());
    }

    /// The Codex desktop app's whole shape, pinned: capturable but not
    /// launchable, attributed by lifecycle hook reports rather than any
    /// peer-PID lane, sharing the Codex CLI's rollout tree, and carrying its
    /// plugin as templates a consumer renders rather than files it copies.
    #[test]
    fn codex_app_is_the_lifecycle_hooks_variant() {
        assert_eq!(CODEX_APP.launch(), LaunchSupport::Unsupported);
        assert!(!CODEX_APP.is_launchable());
        assert!(!supported_agents().contains(&"codex-app"));
        assert_eq!(CODEX_APP.attribution(), AttributionStrategy::LifecycleHooks);
        // Same rollout tree as the CLI: one resolver, two harnesses.
        assert_eq!(CODEX_APP.transcripts(), CODEX.transcripts());
        assert!(CODEX_APP.transcript_root().is_some());
        assert!(matches!(
            CODEX_APP.plugin(),
            PluginDelivery::HookManifestTemplates(_)
        ));
        assert!(CODEX_APP.plugin_artifacts().is_empty());
        // Exactly one lifecycle-hooks harness today; a second should force a
        // deliberate look at what the strategy generalises to.
        let hook_attributed: Vec<&str> = REGISTRY
            .iter()
            .filter(|h| h.attribution() == AttributionStrategy::LifecycleHooks)
            .map(Harness::id)
            .collect();
        assert_eq!(hook_attributed, vec!["codex-app"]);
    }

    #[test]
    fn the_user_agent_gate_is_a_prefix_not_a_substring() {
        assert_eq!(
            for_user_agent("claude-cli/2.1.145").map(Harness::id),
            Some("claude")
        );
        // The exact spelling Anthropic shipped on at least one beta build.
        assert_eq!(
            for_user_agent("Claude-CLI/2.1.145").map(Harness::id),
            Some("claude")
        );
        assert_eq!(
            for_user_agent("CLAUDE/0.0").map(Harness::id),
            Some("claude")
        );

        assert!(for_user_agent("curl/8.0").is_none());
        assert!(for_user_agent("OpenAI/python").is_none());
        assert!(for_user_agent("").is_none());
        // A substring test would claim this one.
        assert!(for_user_agent("some-claude-like").is_none());
    }

    /// A harness with no UA rule must never be claimed by a stray User-Agent —
    /// its lane is selected by route, marker, or its own envelope.
    #[test]
    fn harnesses_without_a_user_agent_rule_claim_nothing() {
        for harness in REGISTRY {
            if harness.user_agent() == UserAgentMatch::None {
                assert!(!harness.matches_user_agent(harness.id()));
                assert!(!harness.matches_user_agent("anything at all"));
            }
        }
    }

    #[test]
    fn supported_agents_is_the_launchable_subset_in_registry_order() {
        assert_eq!(
            supported_agents(),
            vec!["claude", "codex", "opencode", "pi"]
        );
        // pi is launchable but has no recipe here; the distinction survives.
        assert_eq!(PI.launch(), LaunchSupport::ConsumerOwned);
        assert_eq!(CLAUDE.launch(), LaunchSupport::Recipe);
    }

    /// pi's whole point: it attributes itself, so it contributes no attribution
    /// lane and needs an artifact installed into the harness. Encoding that
    /// here is what makes `Attributed::stamp`'s envelope-preserving branch a
    /// stated contract rather than an unexplained special case.
    #[test]
    fn pi_is_the_self_attributing_variant() {
        assert_eq!(PI.attribution(), AttributionStrategy::SelfAttributing);
        assert!(matches!(PI.plugin(), PluginDelivery::BundledExtension(_)));
        assert_eq!(PI.transcripts(), TranscriptSource::None);
        // Exactly one self-attributing harness today; if a second appears, the
        // preserve-the-inbound-envelope branch needs revisiting rather than
        // silently generalising.
        let self_attributing: Vec<&str> = REGISTRY
            .iter()
            .filter(|h| h.attribution() == AttributionStrategy::SelfAttributing)
            .map(Harness::id)
            .collect();
        assert_eq!(self_attributing, vec!["pi"]);
    }

    /// The path a `plugin install` walks, end to end: a user types a name, the
    /// registry resolves it, and the artifacts to write come straight off the
    /// resolved harness. Nothing else connects the two, so if this hop breaks
    /// an installer silently has nothing to install.
    #[test]
    fn a_resolved_name_reaches_the_artifacts_an_installer_writes() {
        let harness = find("pi").expect("pi is registered");
        let artifacts = harness.plugin_artifacts();
        assert_eq!(artifacts.len(), 1, "pi ships exactly one artifact");
        assert_eq!(artifacts[0].file_name(), "tapes-gateway.ts");

        // And a harness captured by redirection alone reaches none — an
        // installer must be able to tell "nothing to do" from "not found".
        assert!(
            find("claude")
                .expect("registered")
                .plugin_artifacts()
                .is_empty()
        );
    }

    /// A harness declaring a transcript tree must be able to name it, or the
    /// transcript lane has nothing to walk.
    #[test]
    fn declared_transcript_trees_resolve_to_a_path() {
        for harness in REGISTRY {
            match harness.transcripts() {
                TranscriptSource::None => {
                    assert!(harness.transcript_root().is_none(), "{}", harness.id());
                }
                _ => assert!(
                    harness.transcript_root().is_some(),
                    "{} declares a transcript tree it cannot locate",
                    harness.id(),
                ),
            }
        }
    }

    #[test]
    fn claude_transcripts_resolve_under_the_home_directory() {
        let root = CLAUDE.transcript_root().expect("home dir");
        assert!(root.ends_with(".claude/projects"), "got {}", root.display());
    }
}
