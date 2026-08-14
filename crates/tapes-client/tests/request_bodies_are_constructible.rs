//! Every request shape is buildable from outside this crate.
//!
//! The rule under test is written down in `core::models`: a response is the
//! server's to grow and is `#[non_exhaustive]`; a request is the caller's to
//! build and is not. Only half of that rule can be checked from inside the
//! crate, because `#[non_exhaustive]` is invisible to the module that declares
//! it — a unit test would keep compiling with the marker on, and the breakage
//! would show up in a consumer's build instead of this one. An integration
//! test compiles as its own crate against the published surface, which is
//! exactly the vantage point where the marker bites.
//!
//! So this file does one thing, and does it in the most literal way available:
//! it names every field of every request shape in a struct literal. That form
//! is chosen over `..Default::default()` on purpose — it fails to compile both
//! when a shape becomes unconstructible *and* when the vendored contract grows
//! a property, so a field added in a contract refresh has to be decided about
//! here rather than defaulted past. The bodies are then serialized, because a
//! type that can be built but not sent has not actually solved anything.
//!
//! The parameter structs are held to the same standard for the same reason:
//! they are caller-built too, and the in-crate check that they match the
//! contract cannot see the marker either.

// A test that cannot build its own fixture has nothing left to assert.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use tapes_client::core::models::params::ContractParams;
use tapes_client::core::models::{
    CreateSkillRequest, ExportDetail, ExportSessionParams, ExportSessionsParams,
    GenerateSkillRequest, GenerateSkillRequestHint, McpRequest, PayloadDetail, PublishSkillRequest,
    RawTurnAttributionRepairRequest, SearchSpansParams, SeedDemoRequest, SessionListParams,
    SessionTracesParams, SessionUpdateRequest, SkillScope, SkillSort, SkillsListParams,
    SortDirection, StatsParams, TraceListParams, TraceParams, UpdateSkillRequest,
};

#[test]
fn every_request_body_can_be_built_and_sent_by_a_consumer() {
    let bodies: Vec<serde_json::Value> = vec![
        serde_json::to_value(CreateSkillRequest {
            content: "# Gum".to_owned(),
            description: "Chews well".to_owned(),
            name: "gum".to_owned(),
            tags: vec!["candy".to_owned()],
            type_: "skill".to_owned(),
        })
        .unwrap(),
        serde_json::to_value(UpdateSkillRequest {
            content: Some("# Gum".to_owned()),
            description: Some("Chews well".to_owned()),
            name: Some("gum".to_owned()),
            tags: Some(vec!["candy".to_owned()]),
            type_: Some("skill".to_owned()),
            visibility: Some("private".to_owned()),
        })
        .unwrap(),
        serde_json::to_value(PublishSkillRequest {
            changelog: "First cut".to_owned(),
            content: "# Gum".to_owned(),
        })
        .unwrap(),
        serde_json::to_value(GenerateSkillRequest {
            hint: GenerateSkillRequestHint {
                description: "Chews well".to_owned(),
                name: "gum".to_owned(),
                tags: vec!["candy".to_owned()],
                type_: "skill".to_owned(),
            },
            session_ids: vec!["s-1".to_owned()],
        })
        .unwrap(),
        serde_json::to_value(SessionUpdateRequest {
            display_name: Some("A better title".to_owned()),
        })
        .unwrap(),
        serde_json::to_value(RawTurnAttributionRepairRequest {
            harness_id: "claude".to_owned(),
            harness_session_id: "hs-1".to_owned(),
            paper_proxy_request_id: "req-1".to_owned(),
            parent_harness_session_id: "hs-0".to_owned(),
            raw_turn_id: 1,
            reason: "misattributed at ingest".to_owned(),
            thread_id: "t-1".to_owned(),
        })
        .unwrap(),
        serde_json::to_value(SeedDemoRequest { overwrite: true }).unwrap(),
        serde_json::to_value(McpRequest {
            id: Some("1".to_owned()),
            jsonrpc: "2.0".to_owned(),
            method: "tools/call".to_owned(),
            params: BTreeMap::new(),
        })
        .unwrap(),
    ];

    for body in &bodies {
        assert!(body.is_object(), "a request body serializes as a document");
    }
}

#[test]
fn every_parameter_set_can_be_built_by_a_consumer() {
    // Built, then read back through the crate's own accessor, so the fields a
    // consumer sets are the fields that reach a request rather than a literal
    // the compiler accepted and nothing consumed.
    let sets: Vec<Vec<(&'static str, String)>> = vec![
        SessionListParams {
            limit: Some(1),
            cursor: Some("c".to_owned()),
            sort: Some("last_active".to_owned()),
            direction: Some(SortDirection::Desc),
            since: Some("2020-01-01T00:00:00Z".to_owned()),
            until: Some("2020-01-02T00:00:00Z".to_owned()),
            harness_id: Some("claude".to_owned()),
            harness_session_id: Some("hs-1".to_owned()),
            auth_subject: Some("user".to_owned()),
        }
        .values(),
        SessionTracesParams {
            payload: Some(PayloadDetail::Full),
        }
        .values(),
        TraceParams {
            payload: Some(PayloadDetail::Preview),
        }
        .values(),
        TraceListParams {
            session_id: "s-1".to_owned(),
        }
        .values(),
        SearchSpansParams {
            query: "gum glow charm".to_owned(),
            top_k: Some(5),
        }
        .values(),
        ExportSessionParams {
            detail: Some(ExportDetail::Spans),
        }
        .values(),
        ExportSessionsParams {
            since: Some("2020-01-01T00:00:00Z".to_owned()),
            until: Some("2020-01-02T00:00:00Z".to_owned()),
            detail: Some(ExportDetail::Traces),
        }
        .values(),
        SkillsListParams {
            limit: Some(1),
            cursor: Some("c".to_owned()),
            q: Some("rust".to_owned()),
            scope: Some(SkillScope::Mine),
            sort: Some(SkillSort::Downloads),
        }
        .values(),
        StatsParams {
            since: Some("2020-01-01T00:00:00Z".to_owned()),
            until: Some("2020-01-02T00:00:00Z".to_owned()),
            auth_subject: Some("user".to_owned()),
        }
        .values(),
    ];

    for set in &sets {
        assert!(!set.is_empty(), "a populated parameter set sends something");
    }
}

#[test]
fn a_partial_update_built_out_here_omits_what_it_does_not_set() {
    // The consumer-side statement of the rule the models' own tests pin: the
    // fields a caller leaves alone are absent from the bytes, so the server
    // has nothing to apply for them.
    let rename = UpdateSkillRequest {
        name: Some("gum glow charm".to_owned()),
        ..Default::default()
    };

    assert_eq!(
        serde_json::to_value(&rename).unwrap(),
        serde_json::json!({"name": "gum glow charm"}),
    );
}
