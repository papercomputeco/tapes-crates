//! What the envelope needs to know about a harness's session.
//!
//! The envelope's job is to turn a resolved session identity into the
//! `X-Tapes-*` header set. To do that it needs six things — a harness id, a
//! session id, and four optional fields — and it needs them from *every*
//! harness, present and future.
//!
//! Before this trait it got them by naming one: the producer imported Claude's
//! session-file struct and read its fields directly. That is what made the
//! envelope un-shareable. It sat in the crate that declares harnesses, so
//! adding a harness could change it, and the harness registry could not take
//! its ids from the envelope without the two crates depending on each other.
//!
//! [`HarnessSession`] states the requirement instead of importing a supplier of
//! it. A harness crate implements it for whatever shape it already parses — a
//! foreign trait on a local type, which is always allowed — and the envelope
//! constructs from `&impl HarnessSession`, naming nobody. The next harness
//! implements the same trait without a line changing here.
//!
//! # Absence is a first-class answer
//!
//! Every field but the two required ones defaults to "this harness has no such
//! thing". A harness that never names a session, or ships no version string,
//! implements nothing extra and the corresponding header is simply omitted —
//! which is the envelope's existing meaning for an absent optional (see
//! `X-Tapes-*` field docs: absent and empty stay distinguishable downstream).
//! Nothing is ever filled with a placeholder to satisfy the shape.

/// A harness session, as the envelope producer sees it.
///
/// Implement this on the type a harness crate already parses out of whatever
/// the harness publishes — a session file, a rollout record, a lifecycle
/// report. The methods are a projection, not a parser: they hand back what the
/// implementor already holds.
///
/// Only [`harness_id`](Self::harness_id) and [`session_id`](Self::session_id)
/// are required, because an envelope without them is not an identity at all —
/// the producer's completeness rule rejects exactly that pair being absent.
/// Everything else defaults to absent.
pub trait HarnessSession {
    /// The harness this session belongs to — the `X-Tapes-Harness-Id` value.
    ///
    /// Returned by the implementor rather than passed in by the caller so one
    /// session type cannot be stamped under two different harness ids by two
    /// call sites. The `HARNESS_ID_*` constants are the vocabulary; a harness
    /// crate takes its id from there and reports it here.
    fn harness_id(&self) -> &str;

    /// The harness's own session identifier — `X-Tapes-Harness-Session-Id`.
    ///
    /// Required, and deliberately not `Option`: a harness that cannot name its
    /// session has no envelope to produce, and its capture client should emit
    /// the `unknown` sentinel rather than an identity with a hole in it.
    fn session_id(&self) -> &str;

    /// Harness version string — `X-Tapes-Harness-Version`.
    fn version(&self) -> Option<&str> {
        None
    }

    /// Working directory the harness is running in — `X-Tapes-Cwd`.
    fn cwd(&self) -> Option<&str> {
        None
    }

    /// User-given session name — `X-Tapes-Session-Name`.
    fn name(&self) -> Option<&str> {
        None
    }

    /// Everything else this harness wants carried in
    /// `X-Tapes-Harness-Metadata`, as the JSON object it is encoded from.
    ///
    /// Returned by value rather than borrowed because the map is *assembled*,
    /// not stored: a harness typically has a few modelled fields plus a
    /// verbatim passthrough of whatever its session file carried that the
    /// crate does not model, and the two are one object on the wire. Which
    /// keys those are, and how they are spelled, is harness knowledge and
    /// belongs on the implementor's side of this boundary — the producer only
    /// caps, encodes, and drops.
    fn metadata(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The minimum a harness must state. Everything else takes the default,
    /// which is the shape a harness with no analogue for a field gets for
    /// free — no placeholder, no dummy string.
    struct Minimal;

    impl HarnessSession for Minimal {
        fn harness_id(&self) -> &str {
            "minimal"
        }
        fn session_id(&self) -> &str {
            "sid-1"
        }
    }

    #[test]
    fn a_harness_with_no_optional_fields_states_only_the_two_required_ones() {
        let session = Minimal;
        assert_eq!(session.harness_id(), "minimal");
        assert_eq!(session.session_id(), "sid-1");
        assert_eq!(session.version(), None);
        assert_eq!(session.cwd(), None);
        assert_eq!(session.name(), None);
        assert!(session.metadata().is_empty());
    }
}
