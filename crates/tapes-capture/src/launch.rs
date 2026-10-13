//! The launch-lineage environment contract: how a launched harness, and any
//! launcher that runs *inside* it, learns which launch it descends from.
//!
//! # The problem
//!
//! A captured session can start another captured session. A shell tool in a
//! Claude Code session runs `<launcher> start hermes`; a hermes terminal tool
//! starts codex. Each child is launched by its own launcher process, which
//! knows nothing about the session it is running inside — and that parent
//! session's id is a fact only the *parent's* launcher (or the parent
//! harness) knows. Without a shared channel, the child's turns land with no
//! lineage at all, and the tree a user actually ran renders as unrelated
//! sessions.
//!
//! # The contract
//!
//! Every launcher exports two variables into the environment of the harness
//! it starts:
//!
//! | variable | const | value |
//! | --- | --- | --- |
//! | `TAPES_LAUNCH_ID` | [`LAUNCH_ID_ENV`] | An opaque id unique to this launch. |
//! | `TAPES_LAUNCH_HARNESS_ID` | [`LAUNCH_HARNESS_ID_ENV`] | The harness id this launch started — an `X-Tapes-Harness-Id` value. |
//!
//! Because a harness's tools inherit its environment, a launcher started from
//! anywhere inside that harness's process tree finds both variables already
//! set in **its own** environment. It treats them as its **parent** launch,
//! then overwrites them with its own values for the child it starts — so the
//! variables always describe the nearest enclosing launch, one hop, never the
//! root.
//!
//! [`LaunchContext::from_env`] is the read half and [`LaunchContext::child_env`]
//! the write half; a launcher calls the first before spawning and applies the
//! second to the child's environment.
//!
//! # What is deliberately not here
//!
//! **The parent's session id.** A launch id names a *process launch*, not a
//! session: one launch may host several sessions over its life (a harness's
//! `/new`, a compaction that rotates the session id, subagents), and which one
//! was live when the child started is something only the parent's launcher can
//! answer — it is the one receiving the parent's traffic. So the mapping from
//! parent launch to parent session id is the **launcher's**: a child launcher
//! reports its [`ParentLaunch`] to whatever registry its deployment keeps (the
//! parent launcher's control socket, a daemon, the capture proxy), receives the
//! parent session id back, and stamps it as
//! [`crate::envelope::X_TAPES_PARENT_HARNESS_SESSION_ID`] with
//! [`crate::envelope::X_TAPES_PARENT_HARNESS_ID`] set to
//! [`ParentLaunch::harness_id`]. That registry is deployment knowledge, which
//! this crate never holds.
//!
//! Nor is any of this a trust signal. The variables are inherited by every
//! descendant and are trivially forged; they are lineage *hints* a launcher
//! uses to ask a better question, never evidence a proxy believes on their own.
//! Trust stays where [`crate::gateway`] and [`crate::peer_trust`] put it.
//!
//! An in-harness plugin must **not** delete these variables the way it deletes
//! the capture nonce: propagating them to the harness's subprocesses is their
//! entire purpose.

use std::sync::atomic::{AtomicU64, Ordering};

/// Environment variable carrying the opaque id of the launch that started the
/// current process tree's harness.
pub const LAUNCH_ID_ENV: &str = "TAPES_LAUNCH_ID";

/// Environment variable carrying the harness id that launch started.
pub const LAUNCH_HARNESS_ID_ENV: &str = "TAPES_LAUNCH_HARNESS_ID";

/// The enclosing launch a launcher found in its own environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentLaunch {
    /// The parent launch's opaque id.
    pub launch_id: String,
    /// The harness the parent launch started.
    pub harness_id: String,
}

impl ParentLaunch {
    /// Read the parent launch from this process's environment.
    ///
    /// `None` unless **both** variables are present and non-blank. Half a
    /// parent is not evidence of one: a launch id without its harness cannot
    /// be resolved to a session, and a harness id alone names every launch of
    /// that harness. Absent means "no evidence", never a sentinel.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// [`Self::from_env`] over an arbitrary variable lookup, so a launcher can
    /// read a prepared environment rather than its own (and so this is
    /// testable without mutating process state).
    #[must_use]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let field = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        Some(Self {
            launch_id: field(LAUNCH_ID_ENV)?,
            harness_id: field(LAUNCH_HARNESS_ID_ENV)?,
        })
    }
}

/// One launch: its own identity, and the launch it was started inside, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchContext {
    /// This launch's opaque id, exported to the child as [`LAUNCH_ID_ENV`].
    pub launch_id: String,
    /// The harness this launch starts, exported as [`LAUNCH_HARNESS_ID_ENV`].
    pub harness_id: String,
    /// The enclosing launch, when this launcher runs inside one.
    pub parent: Option<ParentLaunch>,
}

impl LaunchContext {
    /// A launch with an explicit parent. Prefer [`Self::from_env`] in a
    /// launcher; this is for one that read its parent some other way.
    #[must_use]
    pub fn new(
        launch_id: impl Into<String>,
        harness_id: impl Into<String>,
        parent: Option<ParentLaunch>,
    ) -> Self {
        Self {
            launch_id: launch_id.into(),
            harness_id: harness_id.into(),
            parent,
        }
    }

    /// Start a launch of `harness_id` with id `launch_id`, taking the parent
    /// from this process's own environment.
    ///
    /// Call it **before** applying [`Self::child_env`] anywhere in this
    /// process — once the child's values are written into the current
    /// environment, the parent is gone.
    ///
    /// The id is the caller's to mint (a UUID is ideal); [`new_launch_id`] is
    /// available for a launcher that has no generator of its own.
    #[must_use]
    pub fn from_env(launch_id: impl Into<String>, harness_id: impl Into<String>) -> Self {
        Self::new(launch_id, harness_id, ParentLaunch::from_env())
    }

    /// [`Self::from_env`] over an arbitrary variable lookup.
    #[must_use]
    pub fn from_lookup(
        launch_id: impl Into<String>,
        harness_id: impl Into<String>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Self {
        Self::new(launch_id, harness_id, ParentLaunch::from_lookup(lookup))
    }

    /// The variables to set in the launched harness's environment, as
    /// `(name, value)` pairs.
    ///
    /// Always both, always this launch's own values — never the parent's.
    /// That replacement is what keeps the variables one hop deep: a launcher
    /// started inside this child will read *this* launch as its parent.
    #[must_use]
    pub fn child_env(&self) -> [(&'static str, String); 2] {
        [
            (LAUNCH_ID_ENV, self.launch_id.clone()),
            (LAUNCH_HARNESS_ID_ENV, self.harness_id.clone()),
        ]
    }
}

/// A launch id unique within this host, for a launcher with no UUID source.
///
/// Wall-clock nanoseconds, the process id, and a per-process counter, in hex.
/// Unique enough to key a local registry; not a secret and not meant to be
/// unguessable — nothing trusts a launch id (see the module docs).
#[must_use]
pub fn new_launch_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{:x}-{sequence:x}", std::process::id())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    /// A top-level launch — nothing in the environment — has no parent.
    #[test]
    fn a_launch_outside_any_other_has_no_parent() {
        let ctx = LaunchContext::from_lookup("L1", "claude", env(&[]));
        assert_eq!(ctx.parent, None);
        assert_eq!(ctx.launch_id, "L1");
        assert_eq!(ctx.harness_id, "claude");
    }

    /// A launcher inside another launch's tree reads that launch as its
    /// parent, and hands its child its *own* values — one hop, not the root.
    #[test]
    fn a_nested_launch_reads_its_parent_and_exports_itself() {
        let outer = LaunchContext::from_lookup("L1", "claude", env(&[]));
        let outer_env = outer.child_env();
        let inner_pairs: Vec<(&str, &str)> =
            outer_env.iter().map(|(k, v)| (*k, v.as_str())).collect();

        let inner = LaunchContext::from_lookup("L2", "hermes", env(&inner_pairs));
        assert_eq!(
            inner.parent,
            Some(ParentLaunch {
                launch_id: "L1".to_owned(),
                harness_id: "claude".to_owned(),
            })
        );
        assert_eq!(
            inner.child_env(),
            [
                (LAUNCH_ID_ENV, "L2".to_owned()),
                (LAUNCH_HARNESS_ID_ENV, "hermes".to_owned()),
            ]
        );

        // Three deep: the grandchild sees the child, never the root.
        let inner_env = inner.child_env();
        let pairs: Vec<(&str, &str)> = inner_env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let grandchild = LaunchContext::from_lookup("L3", "codex", env(&pairs));
        assert_eq!(grandchild.parent.unwrap().launch_id, "L2");
    }

    /// Half a parent is no parent, and blank is absent.
    #[test]
    fn a_partial_or_blank_parent_is_no_parent() {
        for pairs in [
            vec![(LAUNCH_ID_ENV, "L1")],
            vec![(LAUNCH_HARNESS_ID_ENV, "claude")],
            vec![(LAUNCH_ID_ENV, "  "), (LAUNCH_HARNESS_ID_ENV, "claude")],
            vec![(LAUNCH_ID_ENV, "L1"), (LAUNCH_HARNESS_ID_ENV, "")],
        ] {
            assert_eq!(ParentLaunch::from_lookup(env(&pairs)), None, "{pairs:?}");
        }
    }

    /// Padding is not part of an id.
    #[test]
    fn parent_values_are_trimmed() {
        let parent = ParentLaunch::from_lookup(env(&[
            (LAUNCH_ID_ENV, " L1 "),
            (LAUNCH_HARNESS_ID_ENV, " pi\n"),
        ]))
        .unwrap();
        assert_eq!(parent.launch_id, "L1");
        assert_eq!(parent.harness_id, "pi");
    }

    /// The spellings are the cross-language contract; a rename here is a
    /// rename in every launcher and plugin.
    #[test]
    fn the_variable_names_are_pinned() {
        assert_eq!(LAUNCH_ID_ENV, "TAPES_LAUNCH_ID");
        assert_eq!(LAUNCH_HARNESS_ID_ENV, "TAPES_LAUNCH_HARNESS_ID");
    }

    #[test]
    fn minted_launch_ids_differ() {
        let ids: std::collections::HashSet<String> = (0..64).map(|_| new_launch_id()).collect();
        assert_eq!(ids.len(), 64);
        assert!(ids.iter().all(|id| !id.trim().is_empty()));
    }
}
