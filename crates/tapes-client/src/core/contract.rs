//! The vendored core contract, and the surface reduced from it.
//!
//! # One reducer, two document sources
//!
//! The generated cassette surface answers "what can this server do?" by
//! reducing an OpenAPI document to callable methods — discovered at runtime,
//! because the cassette set is deployment configuration. The core tapes API is
//! the opposite kind of fact: it is a *published contract*, sealed in the tapes
//! repository (`api/CONTRACT`) and attached to releases, so the right copy to
//! build from is the vendored one in `contracts/tapes-api.yaml`, pinned by
//! fingerprint (see `contracts/PROVENANCE.md`).
//!
//! Both feed [`crate::cassettes::spec::reduce_methods`]. What used to be a
//! set of hand-written URL builders in each client is a lookup into this
//! surface: the verb, the path template, and the set of declared parameters all
//! come from the contract bytes, and a request naming a parameter the contract
//! does not declare is refused before it is sent.
//!
//! # Why the contract is vendored rather than fetched
//!
//! Neither client builds against the tapes working tree; both build against a
//! published release asset. Vendoring it here — once — is what stops two
//! clients holding two copies that nothing checks for agreement.

use std::sync::LazyLock;

use crate::cassettes::spec::{self, Location, Method, ReducerConfig};
use crate::transport::Call;
use serde_json::Value;
use snafu::OptionExt;

use crate::error::{Result, error};

/// The vendored read-API contract, byte-for-byte what
/// `contracts/tapes-api.yaml` holds.
pub const TAPES_API_YAML: &str = include_str!("../../contracts/tapes-api.yaml");

/// Operation ids of the vendored contract, named once so client methods,
/// coverage tables, and tests cannot drift apart on a string.
pub mod ops {
    /// `GET /v1/sessions`
    pub const LIST_SESSIONS: &str = "listSessions";
    /// `GET /v1/sessions/{id}`
    pub const GET_SESSION: &str = "getSession";
    /// `GET /v1/sessions/{id}/traces`
    pub const GET_SESSION_TRACES: &str = "getSessionTraces";
    /// `GET /v1/sessions/{id}/raw_turns`
    pub const LIST_RAW_TURNS: &str = "listRawTurns";
    /// `GET /v1/sessions/{id}/export`
    pub const EXPORT_SESSION: &str = "exportSession";
    /// `GET /v1/traces`
    pub const LIST_TRACES: &str = "listTraces";
    /// `GET /v1/traces/{trace_id}`
    pub const GET_TRACE: &str = "getTrace";
    /// `GET /v1/traces/{trace_id}/spans/{span_id}`
    pub const GET_SPAN: &str = "getSpan";
    /// `GET /v1/search/spans`
    pub const SEARCH_SPANS: &str = "searchSpans";
    /// `POST /v1/admin/seed/demo`
    pub const SEED_DEMO: &str = "seedDemo";
    /// `GET /v1/cassettes`
    pub const LIST_CASSETTES: &str = "listCassettes";
    /// `PATCH /v1/sessions/{id}`
    pub const UPDATE_SESSION: &str = "updateSession";
    /// `DELETE /v1/sessions/{id}`
    pub const DELETE_SESSION: &str = "deleteSession";
    /// `GET /v1/sessions/export`
    pub const EXPORT_SESSIONS: &str = "exportSessions";
    /// `GET /v1/sessions/{id}/skills`
    pub const LIST_SESSION_SKILLS: &str = "listSessionSkills";
    /// `GET /v1/stats`
    pub const GET_STATS: &str = "getStats";
    /// `GET /v1/skills`
    pub const LIST_SKILLS: &str = "listSkills";
    /// `POST /v1/skills`
    pub const CREATE_SKILL: &str = "createSkill";
    /// `GET /v1/skills/{id}`
    pub const GET_SKILL: &str = "getSkill";
    /// `PUT /v1/skills/{id}`
    pub const UPDATE_SKILL: &str = "updateSkill";
    /// `DELETE /v1/skills/{id}`
    pub const DELETE_SKILL: &str = "deleteSkill";
    /// `POST /v1/skills/{id}/duplicate`
    pub const DUPLICATE_SKILL: &str = "duplicateSkill";
    /// `GET /v1/skills/{id}/versions`
    pub const LIST_SKILL_VERSIONS: &str = "listSkillVersions";
    /// `POST /v1/skills/{id}/versions`
    pub const PUBLISH_SKILL: &str = "publishSkill";
    /// `POST /v1/skills/generate`
    pub const GENERATE_SKILL: &str = "generateSkill";
}

/// The core read surface, reduced from the vendored contract.
#[derive(Debug)]
pub struct CoreSurface {
    methods: Vec<Method>,
}

impl CoreSurface {
    /// Reduce the vendored contract under a consumer's own reducer
    /// configuration.
    ///
    /// The configuration only shapes the *presentation* names
    /// ([`crate::cassettes::spec::Param::flag`]); wire names and
    /// locations, which is all [`call_for`] reads, are the document's
    /// regardless. A consumer that renders this surface on a command line
    /// passes its reserved flags here; one that only calls operations can use
    /// [`core`](crate::core::contract::core).
    #[must_use]
    pub fn reduce(reducer: &ReducerConfig<'_>) -> Option<Self> {
        Self::from_yaml(TAPES_API_YAML, reducer)
    }

    /// Reduce a contract document from its YAML bytes.
    fn from_yaml(yaml: &str, reducer: &ReducerConfig<'_>) -> Option<Self> {
        let document: Value = serde_yaml::from_str(yaml).ok()?;
        let methods = spec::reduce_methods(&document, reducer);
        if methods.is_empty() {
            // An empty surface means the bytes were YAML but not a contract;
            // treat it exactly like a parse failure rather than serving a
            // client where every operation lookup fails one at a time.
            return None;
        }
        Some(Self { methods })
    }

    /// Look one operation up by the contract's own `operationId`.
    pub fn method(&self, operation_id: &str) -> Result<&Method> {
        self.methods
            .iter()
            .find(|method| method.operation_id.as_deref() == Some(operation_id))
            .context(error::ContractOperationSnafu {
                operation: operation_id,
            })
    }

    /// Every `operationId` in the vendored document, for the coverage gate.
    pub fn operation_ids(&self) -> impl Iterator<Item = &str> {
        self.methods
            .iter()
            .filter_map(|method| method.operation_id.as_deref())
    }
}

/// The surface, reduced once per process under the default reducer. `None`
/// only for a build whose embedded document is corrupt, which this crate's
/// contract tests fail long before.
static CORE: LazyLock<Option<CoreSurface>> =
    LazyLock::new(|| CoreSurface::from_yaml(TAPES_API_YAML, &ReducerConfig::default()));

/// The core surface, or the build-defect error.
pub fn core() -> Result<&'static CoreSurface> {
    CORE.as_ref().context(error::VendoredContractSnafu {
        surface: "tapes-api",
    })
}

/// Build the [`Call`] for one operation from wire-named values.
///
/// Equivalent to [`call_for_with_body`] with no body, which is what every read
/// operation wants. An operation whose `requestBody` the contract marks
/// required is refused here rather than sent without one — use
/// [`call_for_with_body`] for those.
pub fn call_for<'m>(method: &'m Method, values: Vec<(&str, String)>) -> Result<Call<'m>> {
    call_for_with_body(method, values, None)
}

/// Build the [`Call`] for one operation from wire-named values and a body.
///
/// This is where "drive through the contract" becomes enforceable. The verb
/// and path template are the document's, and every value is routed by the
/// document's declared location for that name. Four things are refused before
/// anything is sent, and they are refusals rather than best-effort requests
/// because each one produces a request that *looks* fine on the wire:
///
/// - a name the document does not declare — the drift a vendored contract
///   exists to catch, which a server that ignores unknown query parameters
///   would otherwise hide;
/// - a path placeholder left without a value, which cannot produce a URL at
///   all;
/// - a query or header parameter the document marks **required** and that has
///   no value. This one is the quietest: the URL is perfectly well-formed, and
///   the server answers with a 400 in its own words — or, worse, on an
///   operation whose required filter is what scopes the result, answers a
///   different question than the caller believes it asked;
/// - a body that disagrees with the operation's `requestBody` in either
///   direction. A required body left absent arrives as a syntactically valid
///   request that means nothing; a body sent to an operation declaring none is
///   dropped somewhere before the handler. Both look correct at the call site.
///
/// Values are given under their wire names — the same names the hand-written
/// builders this replaced used — so the call sites read as the requests they
/// make.
pub fn call_for_with_body<'m>(
    method: &'m Method,
    values: Vec<(&str, String)>,
    body: Option<String>,
) -> Result<Call<'m>> {
    let operation = || {
        method
            .operation_id
            .clone()
            .unwrap_or_else(|| method.name.clone())
    };

    let mut call = Call {
        method: &method.http_method,
        path: &method.path,
        ..Default::default()
    };

    for (wire, value) in values {
        let declared = method
            .params
            .iter()
            .find(|param| param.wire == wire)
            .with_context(|| error::ContractParameterSnafu {
                operation: operation(),
                parameter: wire,
            })?;
        let pair = (declared.wire.clone(), value);
        match declared.location {
            Location::Path => call.path_params.push(pair),
            Location::Query => call.query.push(pair),
            Location::Header => call.headers.push(pair),
        }
    }

    // Every declared parameter that must have a value, checked in one pass so
    // the three locations cannot drift apart in what they enforce. Path is
    // checked first by construction — the reducer orders path parameters ahead
    // of the rest — and keeps its own error, because "no URL could be built"
    // is a different problem from "this is not the request the contract
    // describes".
    for param in &method.params {
        let supplied = match param.location {
            Location::Path => &call.path_params,
            Location::Query => &call.query,
            Location::Header => &call.headers,
        }
        .iter()
        .any(|(name, _)| *name == param.wire);
        if supplied {
            continue;
        }
        match param.location {
            // A path placeholder without a value cannot produce a callable
            // URL; the substitution would leave a literal `{id}` segment
            // addressing nothing.
            Location::Path => {
                return error::ContractPathParameterSnafu {
                    operation: operation(),
                    parameter: param.wire.clone(),
                }
                .fail();
            }
            Location::Query if param.required => {
                return error::ContractRequiredParameterSnafu {
                    operation: operation(),
                    parameter: param.wire.clone(),
                    location: "query",
                }
                .fail();
            }
            Location::Header if param.required => {
                return error::ContractRequiredParameterSnafu {
                    operation: operation(),
                    parameter: param.wire.clone(),
                    location: "header",
                }
                .fail();
            }
            // An optional parameter left unset is the omit-when-unset rule:
            // the server's own default applies, and this client never has to
            // be updated when one of them changes.
            Location::Query | Location::Header => {}
        }
    }

    // `Method::body` is `Some(true)` when the contract requires a body,
    // `Some(false)` when it accepts an optional one, and `None` when the
    // operation takes none at all.
    match (method.body, body) {
        (Some(true), None) => {
            return error::ContractBodySnafu {
                operation: operation(),
                detail: "requires a request body and none was supplied",
            }
            .fail();
        }
        (None, Some(_)) => {
            return error::ContractBodySnafu {
                operation: operation(),
                detail: "declares no request body, so one cannot be sent",
            }
            .fail();
        }
        (_, supplied) => call.body = supplied,
    }

    Ok(call)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_vendored_contract_parses_and_reduces() {
        // The one place a corrupt vendored document is allowed to fail loudly.
        let surface = core().expect("contracts/tapes-api.yaml must parse");
        assert!(surface.operation_ids().count() > 0);
    }

    #[test]
    fn an_unknown_operation_is_an_error_not_a_guessed_route() {
        let err = core().unwrap().method("launchMissiles").unwrap_err();
        assert!(err.to_string().contains("launchMissiles"), "got: {err}");
    }

    #[test]
    fn a_value_is_routed_by_the_contracts_declared_location() {
        let surface = core().unwrap();
        let method = surface.method(ops::GET_SESSION_TRACES).unwrap();
        let call = call_for(
            method,
            vec![("id", "s-1".to_owned()), ("payload", "preview".to_owned())],
        )
        .unwrap();

        assert_eq!(call.method, "GET");
        assert_eq!(call.path, "/v1/sessions/{id}/traces");
        assert_eq!(call.path_params, vec![("id".to_owned(), "s-1".to_owned())]);
        assert_eq!(
            call.query,
            vec![("payload".to_owned(), "preview".to_owned())]
        );
    }

    #[test]
    fn an_undeclared_parameter_is_refused_before_any_request() {
        // Sending it anyway is exactly the drift the vendored contract exists
        // to catch; the server ignoring an unknown query param would hide it.
        let surface = core().unwrap();
        let method = surface.method(ops::GET_SESSION).unwrap();
        let err = call_for(
            method,
            vec![("id", "s-1".to_owned()), ("payolad", "full".to_owned())],
        )
        .unwrap_err();
        assert!(err.to_string().contains("payolad"), "got: {err}");
    }

    #[test]
    fn a_missing_path_parameter_is_refused_because_no_url_could_be_built() {
        let surface = core().unwrap();
        let method = surface.method(ops::GET_SPAN).unwrap();
        let err = call_for(method, vec![("trace_id", "t-1".to_owned())]).unwrap_err();
        assert!(err.to_string().contains("span_id"), "got: {err}");
    }

    #[test]
    fn a_missing_required_query_parameter_is_refused_like_a_missing_path_one() {
        // The asymmetry this closes: a missing path value cannot produce a
        // URL, so it was always caught, while a missing required query value
        // produces a perfectly well-formed URL that is not the request the
        // contract describes. `searchSpans` without `query` would have gone
        // out and come back as the server's own 400.
        let surface = core().unwrap();
        for (operation, missing) in [
            (ops::SEARCH_SPANS, "query"),
            (ops::LIST_TRACES, "session_id"),
        ] {
            let method = surface.method(operation).unwrap();
            let err = call_for(method, Vec::new()).unwrap_err();
            assert!(
                err.to_string().contains(missing),
                "{operation} must name {missing:?}: {err}",
            );
            assert!(
                err.to_string().contains("query parameter"),
                "{operation} must say where the parameter travels: {err}",
            );
        }
    }

    #[test]
    fn supplying_a_required_query_parameter_is_all_that_is_asked() {
        // The other half of the gate: enforcement may not start demanding
        // optional parameters. `searchSpans` requires `query` and not
        // `top_k`, and every caller that sends the required one must still
        // build.
        let surface = core().unwrap();
        let call = call_for(
            surface.method(ops::SEARCH_SPANS).unwrap(),
            vec![("query", "gum glow charm".to_owned())],
        )
        .unwrap();
        assert_eq!(call.path, "/v1/search/spans");
        assert_eq!(
            call.query,
            vec![("query".to_owned(), "gum glow charm".to_owned())],
        );
    }

    #[test]
    fn an_optional_parameter_left_unset_is_still_simply_omitted() {
        // The omit-when-unset rule predates this gate and must survive it:
        // an unset optional parameter is left out so the server's own default
        // applies, rather than pinned to whatever today's default happens to
        // be.
        let surface = core().unwrap();
        let call = call_for(surface.method(ops::LIST_SESSIONS).unwrap(), Vec::new()).unwrap();
        assert!(call.query.is_empty(), "got: {:?}", call.query);
    }

    #[test]
    fn an_operation_that_requires_a_body_is_refused_without_one() {
        // Contract-invalid and invisible: the request is syntactically fine
        // and means nothing. Every operation in this position is one this
        // crate's consumers do not expose today, which is exactly why the
        // gap could sit here unnoticed until one of them does.
        let surface = core().unwrap();
        let method = surface.method("createSkill").unwrap();
        let err = call_for(method, Vec::new()).unwrap_err();
        assert!(
            err.to_string().contains("requires a request body"),
            "got: {err}",
        );
    }

    #[test]
    fn an_operation_that_declares_no_body_refuses_one() {
        let surface = core().unwrap();
        let method = surface.method(ops::GET_SESSION).unwrap();
        let err = call_for_with_body(
            method,
            vec![("id", "s-1".to_owned())],
            Some("{}".to_owned()),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("declares no request body"),
            "got: {err}",
        );
    }

    #[test]
    fn a_required_body_is_carried_on_the_call_when_it_is_supplied() {
        let surface = core().unwrap();
        let method = surface.method("createSkill").unwrap();
        let call =
            call_for_with_body(method, Vec::new(), Some(r#"{"name":"x"}"#.to_owned())).unwrap();
        assert_eq!(call.method, "POST");
        assert_eq!(call.body.as_deref(), Some(r#"{"name":"x"}"#));
    }

    #[test]
    fn an_optional_body_may_be_present_or_absent() {
        // `seedDemo` is the one operation a consumer drives today that takes
        // a body at all, and its body is optional — so both spellings have to
        // keep working, or the seed command breaks on a rule meant for
        // operations nobody calls yet.
        let surface = core().unwrap();
        let method = surface.method(ops::SEED_DEMO).unwrap();
        assert_eq!(call_for(method, Vec::new()).unwrap().body, None);
        assert_eq!(
            call_for_with_body(method, Vec::new(), Some("{}".to_owned()))
                .unwrap()
                .body
                .as_deref(),
            Some("{}"),
        );
    }

    #[test]
    fn every_named_operation_id_resolves_in_the_vendored_contract() {
        // The `ops` constants are the crate's own claim about the document;
        // a contract bump that renamed one must fail here rather than at the
        // first user who runs that command.
        let surface = core().unwrap();
        for id in [
            ops::LIST_SESSIONS,
            ops::GET_SESSION,
            ops::GET_SESSION_TRACES,
            ops::LIST_RAW_TURNS,
            ops::EXPORT_SESSION,
            ops::LIST_TRACES,
            ops::GET_TRACE,
            ops::GET_SPAN,
            ops::SEARCH_SPANS,
            ops::SEED_DEMO,
            ops::LIST_CASSETTES,
            ops::UPDATE_SESSION,
            ops::DELETE_SESSION,
            ops::EXPORT_SESSIONS,
            ops::LIST_SESSION_SKILLS,
            ops::GET_STATS,
            ops::LIST_SKILLS,
            ops::CREATE_SKILL,
            ops::GET_SKILL,
            ops::UPDATE_SKILL,
            ops::DELETE_SKILL,
            ops::DUPLICATE_SKILL,
            ops::LIST_SKILL_VERSIONS,
            ops::PUBLISH_SKILL,
            ops::GENERATE_SKILL,
        ] {
            assert!(surface.method(id).is_ok(), "{id:?} did not resolve");
        }
    }

    #[test]
    fn a_reducer_configuration_changes_presentation_without_moving_a_wire_name() {
        // Consumers reduce this document under their own reserved-flag lists.
        // `call_for` reads only wire names and locations, so two consumers
        // with different reserved lists still build byte-identical requests —
        // which is what lets `core()` serve a single cached reduction.
        let reserved = ReducerConfig {
            reserved_flags: &["limit", "id", "help"],
        };
        let mine = CoreSurface::reduce(&reserved).unwrap();
        let theirs = core().unwrap();

        let wires = |surface: &CoreSurface, id: &str| -> Vec<(String, Location)> {
            surface
                .method(id)
                .unwrap()
                .params
                .iter()
                .map(|p| (p.wire.clone(), p.location))
                .collect()
        };
        assert_eq!(
            wires(&mine, ops::LIST_SESSIONS),
            wires(theirs, ops::LIST_SESSIONS),
        );

        // And the presentation really did move, so the test is not vacuous.
        let flags: Vec<&str> = mine
            .method(ops::LIST_SESSIONS)
            .unwrap()
            .params
            .iter()
            .map(|p| p.flag.as_str())
            .collect();
        assert!(flags.contains(&"param-limit"), "got: {flags:?}");
    }
}
