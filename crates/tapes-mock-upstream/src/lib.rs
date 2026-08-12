//! Internal test support for the harness regression matrix.
//!
//! **This crate is not published and is not a public API.** It exists so that a
//! test — in this repository or in a consumer's `[dev-dependencies]` — can
//! launch a real harness binary against a real HTTP server and assert on what
//! actually crossed the wire. Nothing here belongs in a production build, and
//! the crate makes no compatibility promise between revisions.
//!
//! # The problem it exists for
//!
//! Five harnesses, two client CLIs, and nothing that launched a real harness
//! binary through a real client automatically. Every layer had tests and every
//! layer was green: the launch recipes were unit-tested as pure functions, the
//! envelope was checked against a shared fixture corpus, the attribution lanes
//! were exercised with synthetic session files. What no test covered was the
//! *composition* — whether pointing a real harness at a real capture proxy
//! actually produces an attributed turn — and that is where the breakages were.
//!
//! The pieces here are the floor under that gap:
//!
//! * [`upstream`] — one mock provider that speaks Anthropic Messages, OpenAI
//!   Responses (at both the API-key and ChatGPT-plan paths), and OpenAI
//!   chat-completions, streaming real event sequences one flushed chunk at a
//!   time.
//! * [`ingest`] — the mock a captured turn is posted to, with an envelope
//!   reader checked against the same vendored fixture corpus the producer side
//!   is checked against.
//!
//! # Skips are outcomes, not absences
//!
//! A harness binary that is not installed, or a client CLI whose path was not
//! supplied, produces a *skip with a reason* that appears in the run's output
//! and in its manifest. This is the single most important convention in the
//! crate. A matrix that silently omitted the cells it could not run would look
//! identical whether it covered five harnesses or one, and a coverage claim
//! nobody can audit is worse than no claim.
//!
//! # Layout
//!
//! ```text
//! http      a small blocking HTTP/1.1 server, and the request/response types
//! upstream  the provider surfaces, built on `http`
//! ingest    the turn sink and the envelope reader, built on `http`
//! ```

pub mod http;
pub mod ingest;
pub mod upstream;

use std::time::Duration;

pub use ingest::{IngestPolicy, MockIngest};
pub use upstream::MockUpstream;

/// How long a matrix cell waits for a harness to complete its turn.
///
/// Generous because the first launch of a harness on a cold CI runner does real
/// work — resolving a config tree, loading an extension runtime — before it
/// sends anything, and a timeout tuned to a warm developer machine turns that
/// into a flake that looks like a capture failure.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(120);

/// A started mock upstream and mock ingest, the pair every cell runs against.
///
/// They are started together because they are only meaningful together: the
/// upstream shows what the harness sent, the ingest shows what the capture
/// client concluded, and the matrix's assertions are almost always about the
/// relationship between the two.
#[derive(Debug)]
pub struct MockPair {
    /// The provider the harness talks to.
    pub upstream: MockUpstream,
    /// The sink a captured turn is posted to.
    pub ingest: MockIngest,
}

impl MockPair {
    /// Start both mocks on ephemeral loopback ports.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when either socket cannot be bound.
    pub fn start() -> std::io::Result<Self> {
        Ok(Self {
            upstream: MockUpstream::start()?,
            ingest: MockIngest::start()?,
        })
    }

    /// Start both mocks with a non-default ingest policy.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when either socket cannot be bound.
    pub fn with_ingest_policy(policy: IngestPolicy) -> std::io::Result<Self> {
        Ok(Self {
            upstream: MockUpstream::start()?,
            ingest: MockIngest::with_policy(policy)?,
        })
    }
}
