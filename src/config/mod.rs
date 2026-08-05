//! Persistent harness-config patch grammars.
//!
//! [`crate::launch`] plans *per-process* config: argv flags and environment
//! that exist only for the launched process and die with it. Some capture
//! shapes cannot ride that channel — a desktop app is not launched by the
//! capture client at all, and an integration that should survive the next
//! unassisted `codex` invocation needs its provider written into the
//! harness's *own* config file. This module owns the grammar of those durable
//! edits: given the current text of a harness config file and a
//! caller-supplied provider description, produce the new text — idempotently,
//! and preserving everything in the file the patch does not own.
//!
//! # Consumers
//!
//! Three consumers share this grammar, which is why it lives here rather than
//! in any one of them:
//!
//! * **paper's codex integration installer** (`paperctl integrations codex`),
//!   whose `merge_codex_config` this module is ported from;
//! * **tapesctl's codex-app desktop-capture slice**, where the app cannot
//!   take `-c` overrides and capture requires the same `config.toml` patch
//!   the paper installer writes;
//! * **a future opencode installer**, which will add an analogous
//!   `config/opencode.rs` beside [`codex`] when opencode grows a persistent
//!   install path.
//!
//! # The ownership split
//!
//! The line is the same one [`crate::launch`] draws: this crate owns **how**
//! to patch, never **what** to point at.
//!
//! | This module | Consumer |
//! | --- | --- |
//! | where in the document the provider table lives | the provider id and display name |
//! | which keys each auth mode sets and *unsets* | the base URL the provider routes to |
//! | that capture needs request compression off | the attribution header's name |
//! | idempotence: reapplying is a byte-level no-op | which env overrides of its own to scrub |
//! | preserving user content the patch does not own | reading the file, writing it atomically |
//!
//! Functions here are pure over TOML text — no filesystem, no environment, no
//! default paths. The consumer reads the config file, calls the grammar, and
//! owns the write (paper writes atomically via a temp file and compares
//! before/after to decide whether a restart notice is due; that policy stays
//! with paper).

pub mod codex;

pub use codex::{
    CodexConfigError, CodexProviderPatch, InstalledCodexProvider, apply_provider,
    installed_provider, is_provider_applied, remove_provider,
};
