//! The schema-coverage gate.
//!
//! # What this gate is for
//!
//! [`crate::core::coverage`] catches an operation the contract grew and the
//! client never exposed. This one catches the quieter half of the same failure:
//! an operation whose *shape* grew — a field added to `SessionItem`, a schema
//! added beside it — while the models kept decoding happily, dropping the new
//! data on the floor. Nothing fails at runtime when that happens. The response
//! still parses; the field is simply never seen again.
//!
//! So the models are held to the vendored document mechanically:
//!
//! 1. **Every schema is accounted for.** Modelled, or allow-listed with the
//!    reason it is not — the same partition, and the same failure, as the
//!    operation gate.
//! 2. **Every property survives a round trip.** A document synthesised from the
//!    schema is decoded into the model and re-serialised; anything the model
//!    does not carry comes back missing, and is reported by path.
//! 3. **The decoding rules hold.** A schema's optional properties really are
//!    optional (the whole document decodes from `{}`), a required one really is
//!    required, and a composite property really does tolerate `null` — see
//!    [`super`] for why each of those matters.
//!
//! The synthesised document is the trick that makes this work without a
//! hand-written description of each model. A hand-written one would be a second
//! copy of the contract, kept by hand, which is the thing being prevented.
//! Serde is the description: what the model can carry is exactly what survives
//! decoding and re-encoding.
//!
//! # Why the tables live here and not with the consumer
//!
//! Deliberately the opposite of [`crate::core::coverage`], and for the same
//! reason. Coverage of *operations* is a statement about one client's surface,
//! so sharing the tables would make the gate report on a union and protect
//! nobody. Coverage of *schemas* is a statement about these models, which ship
//! in this crate — so the tables ship with them, and a consumer gets the gate
//! by depending on the crate rather than by maintaining a copy of it.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Map, Value, json};

use super::ContractModel;
use super::params::{ContractEnum, ContractParams};
use crate::core::contract::{TAPES_API_YAML, core};
use crate::error::{Result, error};
use snafu::OptionExt;

/// A coverage table: schema name paired with prose for the reviewer.
pub type Table<'a> = &'a [(&'a str, &'a str)];

/// Schemas this crate deliberately does not model, and why.
///
/// The cassette surface models the discovery document itself — partially and on
/// purpose, since a deployment's configuration is not part of the generated
/// command surface. Modelling it a second time here is exactly the duplication
/// this crate exists to end, so these are allow-listed rather than copied.
pub const UNMODELLED: Table<'static> = &[
    (
        "Discovery",
        "modelled by the cassette surface, which reads only the fields it acts on",
    ),
    (
        "DiscoveryEntry",
        "part of the discovery document; see Discovery",
    ),
    (
        "DiscoveryDepends",
        "part of the discovery document; see Discovery",
    ),
    (
        "DiscoverySetting",
        "part of the discovery document; see Discovery",
    ),
    ("Rejection", "part of the discovery document; see Discovery"),
];

/// One modelled schema, and the checks its model can be put through.
#[derive(Clone, Copy)]
pub struct Entry {
    schema: &'static str,
    run: fn(&Value, &Map<String, Value>) -> Vec<String>,
}

impl Entry {
    /// Register one model against the schema it claims.
    #[must_use]
    pub fn of<M: ContractModel>() -> Self {
        Self {
            schema: M::SCHEMA,
            run: audit::<M>,
        }
    }

    /// The schema this entry covers.
    #[must_use]
    pub fn schema(&self) -> &'static str {
        self.schema
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("schema", &self.schema)
            .finish()
    }
}

/// Every schema this crate models, in one table.
///
/// The registry is a function rather than a `const` so a model is registered by
/// naming its type — `Entry::of::<SessionItem>()` — which cannot disagree with
/// the type's own [`ContractModel::SCHEMA`] the way a repeated string could.
#[must_use]
pub fn registry() -> Vec<Entry> {
    use super::{admin, protocol, raw_turn, session, skill, span, trace};
    vec![
        Entry::of::<session::SessionItem>(),
        Entry::of::<session::SessionRollup>(),
        Entry::of::<session::SessionUsage>(),
        Entry::of::<session::ModelUsage>(),
        Entry::of::<session::TreeTask>(),
        Entry::of::<session::SessionListResponse>(),
        Entry::of::<session::SessionDetailResponse>(),
        Entry::of::<session::SessionTracesResponse>(),
        Entry::of::<session::SessionUpdateRequest>(),
        Entry::of::<trace::TraceItem>(),
        Entry::of::<trace::TraceUsage>(),
        Entry::of::<trace::MainUsage>(),
        Entry::of::<trace::TraceDetail>(),
        Entry::of::<trace::TraceListResponse>(),
        Entry::of::<span::SpanItem>(),
        Entry::of::<span::SpanLinkItem>(),
        Entry::of::<span::SpanSearchOutput>(),
        Entry::of::<span::SpanSearchResult>(),
        Entry::of::<raw_turn::RawTurnHeaderItem>(),
        Entry::of::<raw_turn::RawTurnListResponse>(),
        Entry::of::<raw_turn::RawTurnAttribution>(),
        Entry::of::<raw_turn::RawTurnAttributionRepairRequest>(),
        Entry::of::<raw_turn::RawTurnAttributionRepairResult>(),
        Entry::of::<raw_turn::RepairPendingSession>(),
        Entry::of::<skill::SkillResponse>(),
        Entry::of::<skill::SkillsListResponse>(),
        Entry::of::<skill::SkillCounts>(),
        Entry::of::<skill::SkillVersionResponse>(),
        Entry::of::<skill::SkillVersionsResponse>(),
        Entry::of::<skill::SessionSkillsResponse>(),
        Entry::of::<skill::CreateSkillRequest>(),
        Entry::of::<skill::UpdateSkillRequest>(),
        Entry::of::<skill::PublishSkillRequest>(),
        Entry::of::<skill::GenerateSkillRequest>(),
        Entry::of::<admin::SeedResult>(),
        Entry::of::<admin::SeedDemoRequest>(),
        Entry::of::<admin::DeriveRunResponse>(),
        Entry::of::<admin::RederiveReport>(),
        Entry::of::<admin::ReconcileStats>(),
        Entry::of::<admin::StatsResponse>(),
        Entry::of::<protocol::ErrorResponse>(),
        Entry::of::<protocol::McpRequest>(),
        Entry::of::<protocol::McpResponse>(),
        Entry::of::<protocol::McpError>(),
    ]
}

/// What a coverage run found wrong.
///
/// Every category is reported at once, because a gate that surfaces one problem
/// per run turns a contract bump into a sequence of runs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SchemaReport {
    /// Schemas in the contract that are neither modelled nor allow-listed.
    pub unmodelled: Vec<String>,
    /// Schemas named by a table that the contract does not have.
    pub stale: Vec<String>,
    /// Schemas both modelled and allow-listed.
    pub contradictory: Vec<String>,
    /// Ways a model disagreed with the schema it claims.
    pub disagreements: Vec<String>,
}

impl SchemaReport {
    /// Whether the models and the contract agree.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.unmodelled.is_empty()
            && self.stale.is_empty()
            && self.contradictory.is_empty()
            && self.disagreements.is_empty()
    }
}

impl fmt::Display for SchemaReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.unmodelled.is_empty() {
            write!(
                f,
                "schemas in the vendored tapes-api contract that this crate neither models nor \
                 allow-lists: {:?} — add a model (and register it) or allow-list it with the \
                 reason it stays unmodelled. ",
                self.unmodelled,
            )?;
        }
        if !self.stale.is_empty() {
            write!(
                f,
                "schemas named by a coverage table that the vendored contract does not have: \
                 {:?} — the contract dropped or renamed them, and the models must move in the \
                 same change. ",
                self.stale,
            )?;
        }
        if !self.contradictory.is_empty() {
            write!(
                f,
                "schemas both modelled and allow-listed: {:?}. ",
                self.contradictory
            )?;
        }
        for disagreement in &self.disagreements {
            write!(f, "{disagreement} ")?;
        }
        Ok(())
    }
}

/// Compare this crate's models against the vendored contract's schemas.
///
/// Returns the report whether or not it is clean; [`check`] is the assertion
/// form.
///
/// # Errors
///
/// Fails only when the vendored contract cannot be read at all, which this
/// crate's contract tests catch long before.
pub fn report(modelled: &[Entry], unmodelled: Table<'_>) -> Result<SchemaReport> {
    let schemas = schemas()?;
    let known: BTreeSet<&str> = schemas.keys().map(String::as_str).collect();
    let modelled_ids: BTreeSet<&str> = modelled.iter().map(|entry| entry.schema).collect();
    let unmodelled_ids: BTreeSet<&str> = unmodelled.iter().map(|(id, _)| *id).collect();

    let owned =
        |ids: BTreeSet<&str>| -> Vec<String> { ids.into_iter().map(ToOwned::to_owned).collect() };

    let mut disagreements = Vec::new();
    for entry in modelled {
        let Some(schema) = schemas.get(entry.schema) else {
            continue; // reported as stale below
        };
        disagreements.extend((entry.run)(schema, &schemas));
    }

    Ok(SchemaReport {
        unmodelled: owned(
            known
                .iter()
                .filter(|id| !modelled_ids.contains(*id) && !unmodelled_ids.contains(*id))
                .copied()
                .collect(),
        ),
        stale: owned(
            modelled_ids
                .union(&unmodelled_ids)
                .filter(|id| !known.contains(*id))
                .copied()
                .collect(),
        ),
        contradictory: owned(
            modelled_ids
                .intersection(&unmodelled_ids)
                .copied()
                .collect(),
        ),
        disagreements,
    })
}

/// The assertion form of [`report`], over this crate's own tables.
///
/// # Errors
///
/// The rendered report, when the models and the contract disagree.
pub fn check() -> std::result::Result<(), String> {
    let report = report(&registry(), UNMODELLED).map_err(|error| error.to_string())?;
    if report.is_clean() {
        return Ok(());
    }
    Err(report.to_string())
}

/// Hold one operation's parameter type to the parameters the contract declares.
///
/// `params` must be fully populated: the check is two-directional, so a value
/// left unset reads as a parameter the type cannot express.
///
/// # Errors
///
/// A message naming every parameter the type sends that the contract does not
/// declare, and every non-path parameter the contract declares that the type
/// cannot send.
pub fn check_params<P: ContractParams>(params: &P) -> std::result::Result<(), String> {
    let surface = core().map_err(|error| error.to_string())?;
    let method = surface.method(P::OPERATION).map_err(|e| e.to_string())?;
    let declared: BTreeSet<&str> = method
        .params
        .iter()
        .filter(|param| param.location != crate::cassettes::spec::Location::Path)
        .map(|param| param.wire.as_str())
        .collect();
    let sent: BTreeSet<&str> = params.values().into_iter().map(|(wire, _)| wire).collect();

    let undeclared: Vec<&str> = sent.difference(&declared).copied().collect();
    let unsendable: Vec<&str> = declared.difference(&sent).copied().collect();
    if undeclared.is_empty() && unsendable.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} parameters disagree with the contract: sends {undeclared:?} which the contract does \
         not declare; cannot send {unsendable:?} which it does.",
        P::OPERATION,
    ))
}

/// One Rust enum's claim on a contract-declared value set.
#[derive(Debug, Clone, Copy)]
pub struct ClaimedEnum {
    declared_by: &'static [(&'static str, &'static str)],
    values: &'static [&'static str],
}

impl ClaimedEnum {
    /// Register one parameter enum.
    #[must_use]
    pub fn of<E: ContractEnum>() -> Self {
        Self {
            declared_by: E::DECLARED_BY,
            values: E::VALUES,
        }
    }
}

/// Hold the parameter enums to the value sets the contract closes.
///
/// Two-directional, like the schema gate: a value the contract added and the
/// Rust enum lacks is unreachable from a typed call site, and a
/// contract-declared set that no enum claims is a parameter still spelled by
/// hand.
///
/// # Errors
///
/// A message naming every disagreement.
pub fn check_enums(claimed: &[ClaimedEnum]) -> std::result::Result<(), String> {
    let document = document().map_err(|error| error.to_string())?;
    let mut problems = Vec::new();
    let mut covered: BTreeSet<(&str, &str)> = BTreeSet::new();

    for claim in claimed {
        for (operation, parameter) in claim.declared_by {
            covered.insert((operation, parameter));
            problems.extend(compare_enum(&document, claim, operation, parameter));
        }
    }

    for (operation, parameter, _) in every_declared_enum(&document) {
        if !covered.contains(&(operation.as_str(), parameter.as_str())) {
            problems.push(format!(
                "{operation}'s {parameter} closes a value set that no typed enum claims."
            ));
        }
    }

    if problems.is_empty() {
        return Ok(());
    }
    Err(problems.join(" "))
}

/// The vendored document, parsed.
fn document() -> Result<Value> {
    serde_yaml::from_str(TAPES_API_YAML)
        .ok()
        .context(error::VendoredContractSnafu {
            surface: "tapes-api",
        })
}

/// The vendored document's `components.schemas`.
fn schemas() -> Result<Map<String, Value>> {
    let document = document()?;
    document
        .get("components")
        .and_then(|components| components.get("schemas"))
        .and_then(Value::as_object)
        .cloned()
        .context(error::VendoredContractSnafu {
            surface: "tapes-api",
        })
}

/// One claim against one declaration, as a problem or nothing.
fn compare_enum(
    document: &Value,
    claim: &ClaimedEnum,
    operation: &str,
    parameter: &str,
) -> Option<String> {
    let Some(values) = declared_enum(document, operation, parameter) else {
        return Some(format!(
            "{operation}'s {parameter} is claimed as a closed set, but the contract declares no \
             enum for it."
        ));
    };
    let ours: BTreeSet<&str> = claim.values.iter().copied().collect();
    let theirs: BTreeSet<&str> = values.iter().map(String::as_str).collect();
    if ours == theirs {
        return None;
    }
    Some(format!(
        "{operation}'s {parameter} accepts {theirs:?} but the typed enum offers {ours:?}."
    ))
}

/// The `enum` a document declares for one operation's parameter, if any.
fn declared_enum(document: &Value, operation: &str, parameter: &str) -> Option<Vec<String>> {
    every_declared_enum(document)
        .into_iter()
        .find(|(op, name, _)| op == operation && name == parameter)
        .map(|(_, _, values)| values)
}

/// Every `(operation, parameter, values)` the document closes with an `enum`.
fn every_declared_enum(document: &Value) -> Vec<(String, String, Vec<String>)> {
    let mut found = Vec::new();
    for (operation, _, params) in operations(document) {
        for param in params {
            let Some(name) = param.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(values) = param
                .get("schema")
                .and_then(|schema| schema.get("enum"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            found.push((
                operation.clone(),
                name.to_owned(),
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect(),
            ));
        }
    }
    found
}

/// `(operationId, path, parameters)` for every operation in the document.
fn operations(document: &Value) -> Vec<(String, String, Vec<Value>)> {
    let mut found = Vec::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return found;
    };
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for operation in item.values() {
            let Some(id) = operation_id(operation) else {
                continue;
            };
            let params = operation
                .get("parameters")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            found.push((id, path.clone(), params));
        }
    }
    found
}

fn operation_id(operation: &Value) -> Option<String> {
    operation
        .get("operationId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// Put one model through every check the schema supports.
fn audit<M: ContractModel>(schema: &Value, schemas: &Map<String, Value>) -> Vec<String> {
    let name = M::SCHEMA;
    let mut problems = Vec::new();
    let populated = sample(schema, schemas, 0);

    // 1. Everything the schema declares survives decode + re-encode.
    match serde_json::from_value::<M>(populated.clone()) {
        Err(error) => problems.push(format!(
            "{name} does not decode a document built from its own schema: {error}.",
        )),
        Ok(model) => match serde_json::to_value(&model) {
            Err(error) => problems.push(format!("{name} does not re-encode: {error}.")),
            Ok(encoded) => survived(name, &populated, &encoded, &mut problems),
        },
    }

    // 2. Optional properties really are optional; required ones really are
    //    required. The contract declares required-ness per schema, so this
    //    reads it rather than assuming today's answer (which is "none").
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut minimal = Map::new();
    for property in &required {
        if let Some(value) = populated.get(*property) {
            minimal.insert((*property).to_owned(), value.clone());
        }
    }
    if serde_json::from_value::<M>(Value::Object(minimal)).is_err() {
        problems.push(format!(
            "{name} does not decode a document carrying only the properties the contract \
             requires; an optional property is modelled as mandatory.",
        ));
    }
    for property in &required {
        let mut without = populated.as_object().cloned().unwrap_or_default();
        without.remove(*property);
        if serde_json::from_value::<M>(Value::Object(without)).is_ok() {
            problems.push(format!(
                "{name}.{property} is required by the contract but decodes when absent.",
            ));
        }
    }

    // 3. A composite property tolerates an explicit null.
    for (property, declared) in properties(schema) {
        if !is_composite(declared) {
            continue;
        }
        let mut nulled = populated.as_object().cloned().unwrap_or_default();
        nulled.insert(property.clone(), Value::Null);
        if serde_json::from_value::<M>(Value::Object(nulled)).is_err() {
            problems.push(format!(
                "{name}.{property} does not tolerate a null; a nil map, slice, or struct pointer \
                 the server did not omit would blank the whole response.",
            ));
        }
    }

    problems
}

/// Report every value that did not survive the round trip, by path.
fn survived(path: &str, sent: &Value, back: &Value, problems: &mut Vec<String>) {
    match (sent, back) {
        (Value::Object(sent), Value::Object(back)) => {
            for (key, value) in sent {
                match back.get(key) {
                    None => problems.push(format!(
                        "{path}.{key} is in the contract but not carried by the model.",
                    )),
                    Some(got) => survived(&format!("{path}.{key}"), value, got, problems),
                }
            }
        }
        (Value::Array(sent), Value::Array(back)) => {
            for (index, value) in sent.iter().enumerate() {
                match back.get(index) {
                    None => problems.push(format!("{path}[{index}] was dropped by the model.")),
                    Some(got) => survived(&format!("{path}[{index}]"), value, got, problems),
                }
            }
        }
        (sent, back) if sent != back => {
            problems.push(format!("{path} decoded as {back} rather than {sent}."));
        }
        _ => {}
    }
}

/// A schema's declared properties, resolving one level of `$ref`.
fn properties(schema: &Value) -> Vec<(String, &Value)> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.iter().map(|(k, v)| (k.clone(), v)).collect())
        .unwrap_or_default()
}

/// Whether a property is one of the positions a `null` can legitimately arrive
/// in: an array, a map, an object, or another schema.
fn is_composite(schema: &Value) -> bool {
    if schema.get("$ref").is_some() {
        return true;
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("array" | "object") => true,
        Some(_) => false,
        // An untyped schema accepts anything, `null` included.
        None => true,
    }
}

fn resolve<'a>(schema: &Value, schemas: &'a Map<String, Value>) -> Option<&'a Value> {
    let name = schema.get("$ref")?.as_str()?.rsplit('/').next()?;
    schemas.get(name)
}

/// Build a document that exercises every property a schema declares.
///
/// Values are chosen to be exactly representable after a JSON round trip, so a
/// faithful model returns them unchanged and the comparison stays a statement
/// about the model rather than about float formatting.
fn sample(schema: &Value, schemas: &Map<String, Value>, depth: usize) -> Value {
    // The document nests about ten deep at its worst (a listing, of sessions,
    // of rollups, of per-model spend). The cap is well past that and exists
    // only so a schema that ever references itself terminates — loudly, as a
    // decode failure, rather than by recursing until the stack ends.
    if depth > 24 {
        return Value::Null;
    }
    if let Some(target) = resolve(schema, schemas) {
        return sample(target, schemas, depth + 1);
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => match schema.get("format").and_then(Value::as_str) {
            Some("date-time") => json!("2020-01-02T03:04:05Z"),
            _ => json!("sample"),
        },
        Some("boolean") => json!(true),
        Some("integer") => json!(1),
        Some("number") => json!(1.5),
        Some("array") => {
            let items = schema.get("items").cloned().unwrap_or_else(|| json!({}));
            json!([sample(&items, schemas, depth + 1)])
        }
        Some("object") | None => {
            if let Some(props) = schema.get("properties").and_then(Value::as_object) {
                let mut object = Map::new();
                for (name, declared) in props {
                    object.insert(name.clone(), sample(declared, schemas, depth + 1));
                }
                return Value::Object(object);
            }
            match schema.get("additionalProperties") {
                Some(additional) if additional.as_object().is_some_and(Map::is_empty) => {
                    json!({"key": "sample"})
                }
                Some(additional) => json!({"key": sample(additional, schemas, depth + 1)}),
                None if schema.get("type").is_none() => json!("sample"),
                None => json!({}),
            }
        }
        Some(_) => json!("sample"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::core::models::params::{
        ExportDetail, ExportSessionParams, ExportSessionsParams, PayloadDetail, SearchSpansParams,
        SessionListParams, SessionTracesParams, SkillScope, SkillSort, SkillsListParams,
        SortDirection, StatsParams, TraceListParams, TraceParams,
    };
    use serde::{Deserialize, Serialize};

    #[test]
    fn the_models_cover_the_vendored_contracts_schemas() {
        // The gate itself. A contract bump that adds a schema, adds a field to
        // one, or changes a field's type fails here — at build time, where
        // somebody can decide about it — rather than by quietly dropping data.
        assert_eq!(check(), Ok(()));
    }

    #[test]
    fn a_schema_in_neither_table_is_reported_as_unmodelled() {
        let report = report(&[Entry::of::<super::super::SessionItem>()], &[]).unwrap();
        assert!(!report.is_clean());
        assert!(
            report.unmodelled.contains(&"SpanItem".to_owned()),
            "got: {report:?}",
        );
    }

    #[test]
    fn a_table_entry_the_contract_does_not_have_is_reported_as_stale() {
        let report = report(&[], &[("LaunchCodes", "nowhere")]).unwrap();
        assert_eq!(report.stale, vec!["LaunchCodes".to_owned()]);
    }

    #[test]
    fn a_model_that_drops_a_contract_field_is_reported_by_path() {
        // The perturbation this gate exists to catch, pinned as a test rather
        // than as a claim: a model missing one property of its schema names
        // that property in the failure.
        #[derive(Debug, Default, Serialize, Deserialize)]
        #[serde(default)]
        struct HalfASession {
            id: String,
        }
        impl ContractModel for HalfASession {
            const SCHEMA: &'static str = "SessionItem";
        }

        let report = report(&[Entry::of::<HalfASession>()], UNMODELLED).unwrap();
        assert!(
            report
                .disagreements
                .iter()
                .any(|problem| problem.contains("SessionItem.display_title")
                    && problem.contains("not carried by the model")),
            "got: {report:?}",
        );
    }

    #[test]
    fn a_model_that_mistypes_a_field_is_reported_as_a_decode_failure() {
        #[derive(Debug, Default, Serialize, Deserialize)]
        #[serde(default)]
        struct MistypedUsage {
            input_tokens: String,
        }
        impl ContractModel for MistypedUsage {
            const SCHEMA: &'static str = "SessionUsage";
        }

        let report = report(&[Entry::of::<MistypedUsage>()], UNMODELLED).unwrap();
        assert!(
            report
                .disagreements
                .iter()
                .any(|problem| problem.contains("does not decode a document built from its own")),
            "got: {report:?}",
        );
    }

    #[test]
    fn a_composite_that_refuses_a_null_is_reported() {
        // The rule that keeps one nil projection from costing a caller the
        // whole document.
        #[derive(Debug, Default, Serialize, Deserialize)]
        #[serde(default)]
        struct StrictItems {
            items: Vec<Value>,
        }
        impl ContractModel for StrictItems {
            const SCHEMA: &'static str = "RawTurnListResponse";
        }

        let report = report(&[Entry::of::<StrictItems>()], UNMODELLED).unwrap();
        assert!(
            report
                .disagreements
                .iter()
                .any(|problem| problem.contains("RawTurnListResponse.items")
                    && problem.contains("does not tolerate a null")),
            "got: {report:?}",
        );
    }

    #[test]
    fn a_required_property_modelled_as_optional_is_reported() {
        // The contract requires nothing today. The rule is read from the
        // document rather than assumed, so this exercises the branch that will
        // matter the first time a schema does mark one.
        let schema = json!({
            "type": "object",
            "required": ["id"],
            "properties": {"id": {"type": "string"}},
        });
        let schemas = Map::new();

        #[derive(Debug, Default, Serialize, Deserialize)]
        #[serde(default)]
        struct Lenient {
            id: String,
        }
        impl ContractModel for Lenient {
            const SCHEMA: &'static str = "Synthetic";
        }

        let problems = audit::<Lenient>(&schema, &schemas);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("required by the contract but decodes when absent")),
            "got: {problems:?}",
        );
    }

    #[test]
    fn every_typed_parameter_set_matches_the_contracts_declaration() {
        // Two-directional, and the struct literals are exhaustive: a parameter
        // added to the contract fails the check, and a field added to one of
        // these structs fails the compile until it is decided about here.
        check_params(&SessionListParams {
            limit: Some(1),
            cursor: Some("c".to_owned()),
            sort: Some("last_active".to_owned()),
            direction: Some(SortDirection::Desc),
            since: Some("2020-01-01T00:00:00Z".to_owned()),
            until: Some("2020-01-02T00:00:00Z".to_owned()),
            harness_id: Some("claude".to_owned()),
            harness_session_id: Some("hs-1".to_owned()),
            auth_subject: Some("user".to_owned()),
        })
        .unwrap();
        check_params(&SessionTracesParams {
            payload: Some(PayloadDetail::Full),
        })
        .unwrap();
        check_params(&TraceParams {
            payload: Some(PayloadDetail::Preview),
        })
        .unwrap();
        check_params(&TraceListParams {
            session_id: "s-1".to_owned(),
        })
        .unwrap();
        check_params(&SearchSpansParams {
            query: "gum glow charm".to_owned(),
            top_k: Some(5),
        })
        .unwrap();
        check_params(&ExportSessionParams {
            detail: Some(ExportDetail::Spans),
        })
        .unwrap();
        check_params(&ExportSessionsParams {
            since: Some("2020-01-01T00:00:00Z".to_owned()),
            until: Some("2020-01-02T00:00:00Z".to_owned()),
            detail: Some(ExportDetail::Traces),
        })
        .unwrap();
        check_params(&SkillsListParams {
            limit: Some(1),
            cursor: Some("c".to_owned()),
            q: Some("rust".to_owned()),
            scope: Some(SkillScope::Mine),
            sort: Some(SkillSort::Downloads),
        })
        .unwrap();
        check_params(&StatsParams {
            since: Some("2020-01-01T00:00:00Z".to_owned()),
            until: Some("2020-01-02T00:00:00Z".to_owned()),
        })
        .unwrap();
    }

    #[test]
    fn a_parameter_the_contract_does_not_declare_is_reported() {
        struct Typo;
        impl ContractParams for Typo {
            const OPERATION: &'static str = "getSessionTraces";
            fn values(&self) -> Vec<(&'static str, String)> {
                vec![("payolad", "full".to_owned())]
            }
        }
        let err = check_params(&Typo).unwrap_err();
        assert!(err.contains("payolad"), "got: {err}");
    }

    #[test]
    fn every_closed_value_set_in_the_contract_has_a_typed_enum() {
        assert_eq!(
            check_enums(&[
                ClaimedEnum::of::<PayloadDetail>(),
                ClaimedEnum::of::<ExportDetail>(),
                ClaimedEnum::of::<SortDirection>(),
                ClaimedEnum::of::<SkillScope>(),
                ClaimedEnum::of::<SkillSort>(),
            ]),
            Ok(())
        );
    }

    #[test]
    fn a_value_set_no_typed_enum_claims_is_reported() {
        let err = check_enums(&[]).unwrap_err();
        assert!(err.contains("no typed enum claims"), "got: {err}");
    }
}
