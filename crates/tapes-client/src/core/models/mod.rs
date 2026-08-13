//! The sealed contract's response and request shapes, as Rust types.
//!
//! # Why the crate holds these at all
//!
//! [`crate::decode`] takes no view on what a response decodes into, and that
//! was right while nothing in this crate knew the shape of an answer. It is
//! wrong once the shape is *sealed*: `SessionItem` is not a consumer's opinion,
//! it is a published contract vendored into this crate byte-for-byte. Every
//! consumer that modelled it separately was maintaining a private copy of a
//! shared fact — and private copies of a shared fact drift silently, which is
//! the failure this whole crate exists to end.
//!
//! So the typed surface is the **default**: [`crate::core::CoreClient`]'s named
//! methods return these types. The generic seam stays exactly where it was —
//! [`crate::core::CoreClient::call`] is still generic in its response type, and
//! [`crate::decode::typed`] still decodes into whatever a caller names. That is
//! the escape hatch, and it is the right tool for the fidelity operations: an
//! archive written from a typed decode is an archive of the fields this build
//! happened to know about.
//!
//! # The decoding rules, and why each one is what it is
//!
//! The contract's schemas declare no required properties: the server omits an
//! empty field rather than sending it. Every rule below follows from that, and
//! from one more: **an additive server change must never break a consumer.**
//!
//! - **Unknown fields pass silently.** No `deny_unknown_fields`, anywhere. A
//!   field this build has never heard of is a newer server, not a malformed
//!   response, and refusing the document would turn a routine deploy into an
//!   outage for every older client. What catches the addition instead is the
//!   [`coverage`] gate, at build time, where a human can decide about it.
//! - **An absent field decodes to its default.** Container-level
//!   `#[serde(default)]`, on every model.
//! - **A null in a composite position decodes to its default too.** A nil map,
//!   slice, or struct pointer that is not omitted arrives as `null`, and a
//!   model that errored on one would let a single empty projection blank an
//!   entire page. Scalars stay strict: the contract declares no nullable
//!   scalar, so a null in one is a real disagreement worth surfacing.
//! - **Response models are [`non_exhaustive`], request models are not.** A
//!   response is the server's to grow; a request body is the caller's to build,
//!   and a body nobody outside this crate could construct would be useless.
//!   This holds for the *components* of a request body too — an inner struct
//!   marked `non_exhaustive` makes its fields unreachable just as surely as
//!   marking the outer one would, and leaves a caller with nothing but
//!   `Default`. A test outside the crate constructs every request body by
//!   struct literal, which is the only place the marker's effect is visible.
//! - **A request field whose absence means something is an [`Option`] that is
//!   omitted.** The partial-update bodies are the reason: the server applies
//!   the properties a `PUT`/`PATCH` body carries and leaves the rest alone, so
//!   a model that always serialized every field would turn every one-field
//!   update into a wipe of the others. `#[serde(skip_serializing_if =
//!   "Option::is_none")]` is what makes an unset field genuinely absent from
//!   the bytes rather than present and empty. Where absence carries no
//!   distinct meaning — a create body, whose fields land on a fresh record —
//!   the field stays plain, because an `Option` there would be ceremony
//!   without a distinction behind it.
//!
//! # What is deliberately not typed further
//!
//! - **Timestamps stay `String`.** The contract says `string`/`date-time`, and
//!   parsing one into a datetime type would make an unparseable value a decode
//!   failure at the *response* level — one odd timestamp blanking a whole page
//!   — in exchange for a convenience every consumer can add itself.
//! - **Enumerable strings stay `String`.** `status`, `kind`, `call_kind`,
//!   `verdict` and their kin are declared as plain strings; the document names
//!   no closed set. A Rust enum here would invent a contract the server never
//!   made, and would fail exactly when the server added a variant.
//! - **Opaque objects stay [`serde_json::Value`].** Where the contract says
//!   `type: object` with no properties — a span's content blocks, a raw turn's
//!   metadata — there is nothing to model, and inventing a shape would be the
//!   drift this module exists to prevent.
//!
//! # The gate
//!
//! [`coverage`] walks the vendored document's schemas and holds these types to
//! them: every schema is modelled or deliberately allow-listed, every property
//! survives a round trip through its model, and the decoding rules above are
//! asserted rather than assumed. A contract bump that adds a field fails the
//! build, the same way one that adds an operation fails [`crate::core::coverage`].

pub mod admin;
pub mod coverage;
pub mod params;
pub mod protocol;
pub mod raw_turn;
pub mod session;
pub mod skill;
pub mod span;
pub mod trace;

use serde::{Deserialize, Deserializer};

pub use admin::{
    DeriveRunResponse, ReconcileStats, RederiveReport, SeedDemoRequest, SeedResult, StatsResponse,
};
pub use params::{
    ExportDetail, ExportSessionParams, ExportSessionsParams, PayloadDetail, SearchSpansParams,
    SessionListParams, SessionTracesParams, SkillScope, SkillSort, SkillsListParams, SortDirection,
    StatsParams, TraceListParams, TraceParams,
};
pub use protocol::{ErrorResponse, McpError, McpRequest, McpResponse};
pub use raw_turn::{
    RawTurnAttribution, RawTurnAttributionRepairRequest, RawTurnAttributionRepairResult,
    RawTurnHeaderItem, RawTurnListResponse, RepairPendingSession,
};
pub use session::{
    ModelUsage, SessionDetailResponse, SessionItem, SessionListResponse, SessionRollup,
    SessionTracesResponse, SessionUpdateRequest, SessionUsage, TreeTask,
};
pub use skill::{
    CreateSkillRequest, GenerateSkillRequest, GenerateSkillRequestHint, PublishSkillRequest,
    SessionSkillsResponse, SkillCounts, SkillResponse, SkillVersionResponse, SkillVersionsResponse,
    SkillsListResponse, UpdateSkillRequest,
};
pub use span::{SpanItem, SpanLinkItem, SpanSearchOutput, SpanSearchResult};
pub use trace::{MainUsage, TraceDetail, TraceItem, TraceListResponse, TraceUsage};

/// A type that models one named schema of the vendored contract.
///
/// The association is what makes the [`coverage`] gate possible: without a
/// declared schema name, "is every response schema modelled?" would be a
/// question only a human could answer, and the answer would rot. It is also the
/// documentation a reader wants — which published shape *is* this type.
pub trait ContractModel: serde::Serialize + serde::de::DeserializeOwned {
    /// The schema's own name in `contracts/tapes-api.yaml`.
    const SCHEMA: &'static str;
}

/// Decode `null` as the type's default rather than as a failure.
///
/// Applied to every composite field — see the module docs for why a null
/// arrives at all, and why scalars deliberately do not get this treatment.
///
/// # Errors
///
/// Propagates the underlying decode failure for anything that is neither
/// `null` nor a valid value of the field's type.
pub fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use serde_json::json;

    #[test]
    fn an_absent_field_decodes_to_its_default_rather_than_failing() {
        // The contract requires nothing, so `{}` is a legal answer for every
        // shape in it — and a model that refused one would fail on a session
        // the deriver has not reached yet.
        let session: SessionItem = serde_json::from_value(json!({})).unwrap();
        assert_eq!(session.id, "");
        assert_eq!(session.rollup.turn_count, 0);
    }

    #[test]
    fn an_unknown_field_passes_rather_than_failing_the_document() {
        // A newer server, not a malformed response. The build-time gate is
        // what reports the addition; the runtime must not.
        let session: SessionItem =
            serde_json::from_value(json!({"id": "s-1", "a_field_from_the_future": 7})).unwrap();
        assert_eq!(session.id, "s-1");
    }

    #[test]
    fn a_null_composite_decodes_to_empty_rather_than_blanking_the_response() {
        // A nil map or slice that is not omitted arrives as `null`. One of
        // them must not cost the caller the whole document.
        let session: SessionItem = serde_json::from_value(json!({
            "id": "s-1",
            "harness_metadata": null,
            "rollup": null,
        }))
        .unwrap();
        assert_eq!(session.id, "s-1");
        assert!(session.harness_metadata.is_empty());
        assert_eq!(session.rollup, SessionRollup::default());
    }

    #[test]
    fn a_one_field_update_sends_exactly_that_field() {
        // The failure this pins: `updateSkillRequest` is applied property by
        // property, so a body that spelled all six would rename the skill and
        // erase its content, description, tags, type, and visibility in the
        // same call.
        let rename = UpdateSkillRequest {
            name: Some("gum glow charm".to_owned()),
            ..Default::default()
        };
        let sent = serde_json::to_value(&rename).unwrap();

        assert_eq!(sent, json!({"name": "gum glow charm"}));
    }

    #[test]
    fn an_empty_partial_update_sends_an_empty_document() {
        // Nothing set means nothing said — not six empty properties, which is
        // the same erasure spelled with a default constructor.
        assert_eq!(
            serde_json::to_value(UpdateSkillRequest::default()).unwrap(),
            json!({}),
        );
        assert_eq!(
            serde_json::to_value(SessionUpdateRequest::default()).unwrap(),
            json!({}),
        );
    }

    #[test]
    fn a_rename_body_tells_clearing_apart_from_not_touching() {
        // The contract gives the two states different outcomes — an absent
        // field is a 400 (nothing to update), an empty one clears the rename
        // back to the auto-derived title — so the type has to be able to say
        // both, and say them differently.
        let clear = SessionUpdateRequest {
            display_name: Some(String::new()),
        };
        let untouched = SessionUpdateRequest::default();

        assert_eq!(
            serde_json::to_value(&clear).unwrap(),
            json!({"display_name": ""}),
        );
        assert_eq!(serde_json::to_value(&untouched).unwrap(), json!({}));
    }

    #[test]
    fn a_present_but_empty_update_field_still_reaches_the_wire() {
        // The other half of the rule: omitting is what `None` means, and an
        // explicitly emptied field must not be mistaken for one. Clearing a
        // skill's tag list is a legitimate edit.
        let untag = UpdateSkillRequest {
            tags: Some(Vec::new()),
            ..Default::default()
        };

        assert_eq!(serde_json::to_value(&untag).unwrap(), json!({"tags": []}));
    }

    #[test]
    fn a_notification_frame_carries_no_id() {
        // JSON-RPC reads a present id as "answer me", so an empty-string id
        // would make every notification a request awaiting a response.
        let notification = McpRequest {
            id: None,
            jsonrpc: "2.0".to_owned(),
            method: "notifications/initialized".to_owned(),
            params: BTreeMap::new(),
        };
        let sent = serde_json::to_value(&notification).unwrap();

        assert_eq!(sent.get("id"), None, "got: {sent}");
        assert_eq!(sent["method"], "notifications/initialized");
    }

    #[test]
    fn a_nullable_object_keeps_the_distinction_the_contract_draws() {
        // `verdict` is the one property the document marks nullable, and the
        // null is meaningful: it says the span was not judged.
        let judged: SpanItem =
            serde_json::from_value(json!({"verdict": {"decision": "allow"}})).unwrap();
        let unjudged: SpanItem = serde_json::from_value(json!({"verdict": null})).unwrap();
        assert!(judged.verdict.is_some());
        assert!(unjudged.verdict.is_none());
    }
}
