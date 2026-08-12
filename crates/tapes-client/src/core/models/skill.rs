//! Skill shapes.
//!
//! These are the one corner of the contract that speaks camelCase — the
//! console's skills schemas predate the snake_case convention the rest of tapes
//! uses. The models carry snake_case field names with the wire spelling
//! attached, so a Rust call site reads like Rust and the bytes stay the
//! document's.

use serde::{Deserialize, Serialize};

use super::ContractModel;

/// The unified Skill shape the console expects (camelCase).
///
/// Models the contract's `skillResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillResponse {
    /// The contract's `authorId`.
    #[serde(rename = "authorId")]
    pub author_id: String,

    /// The contract's `content`.
    pub content: String,

    /// The contract's `createdAt`.
    #[serde(rename = "createdAt")]
    pub created_at: String,

    /// The contract's `description`.
    pub description: String,

    /// The contract's `downloadCount`.
    #[serde(rename = "downloadCount")]
    pub download_count: i64,

    /// The contract's `id`.
    pub id: String,

    /// The contract's `isAiGenerated`.
    #[serde(rename = "isAiGenerated")]
    pub is_ai_generated: bool,

    /// The contract's `name`.
    pub name: String,

    /// The contract's `originatingSessionIds`.
    #[serde(
        rename = "originatingSessionIds",
        deserialize_with = "super::null_default"
    )]
    pub originating_session_ids: Vec<String>,

    /// The contract's `parentId`.
    #[serde(rename = "parentId")]
    pub parent_id: String,

    /// The contract's `slug`.
    pub slug: String,

    /// The contract's `tags`.
    #[serde(deserialize_with = "super::null_default")]
    pub tags: Vec<String>,

    /// The contract's `type`.
    #[serde(rename = "type")]
    pub type_: String,

    /// The contract's `updatedAt`.
    #[serde(rename = "updatedAt")]
    pub updated_at: String,

    /// The contract's `version`.
    pub version: String,

    /// The contract's `visibility`.
    pub visibility: String,
}

impl ContractModel for SkillResponse {
    const SCHEMA: &'static str = "skillResponse";
}

/// The paginated list envelope: one keyset page plus
/// the opaque next_cursor (mirroring /v1/sessions) and the per-tab counts for
/// the active search.
///
/// Models the contract's `skillsListResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillsListResponse {
    /// The contract's `counts`.
    #[serde(deserialize_with = "super::null_default")]
    pub counts: SkillCounts,

    /// The contract's `items`.
    #[serde(deserialize_with = "super::null_default")]
    pub items: Vec<SkillResponse>,

    /// The contract's `next_cursor`.
    pub next_cursor: String,
}

impl ContractModel for SkillsListResponse {
    const SCHEMA: &'static str = "skillsListResponse";
}

/// The tab counts for the current search: all matching,
/// authored by the caller (mine), and everyone else's (team = all - mine).
///
/// Models the contract's `skillCountsResp` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillCounts {
    /// The contract's `all`.
    pub all: i64,

    /// The contract's `mine`.
    pub mine: i64,

    /// The contract's `team`.
    pub team: i64,
}

impl ContractModel for SkillCounts {
    const SCHEMA: &'static str = "skillCountsResp";
}

/// One immutable published snapshot.
///
/// Models the contract's `skillVersionResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillVersionResponse {
    /// The contract's `authorId`.
    #[serde(rename = "authorId")]
    pub author_id: String,

    /// The contract's `changelog`.
    pub changelog: String,

    /// The contract's `content`.
    pub content: String,

    /// The contract's `id`.
    pub id: String,

    /// The contract's `publishedAt`.
    #[serde(rename = "publishedAt")]
    pub published_at: String,

    /// The contract's `semver`.
    pub semver: String,

    /// The contract's `skillId`.
    #[serde(rename = "skillId")]
    pub skill_id: String,

    /// The contract's `versionNumber`.
    #[serde(rename = "versionNumber")]
    pub version_number: i32,
}

impl ContractModel for SkillVersionResponse {
    const SCHEMA: &'static str = "skillVersionResponse";
}

/// The full version history for one skill, newest
/// first.
///
/// Models the contract's `skillVersionsResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SkillVersionsResponse {
    /// The contract's `totalCount`.
    #[serde(rename = "totalCount")]
    pub total_count: i32,

    /// The contract's `versions`.
    #[serde(deserialize_with = "super::null_default")]
    pub versions: Vec<SkillVersionResponse>,
}

impl ContractModel for SkillVersionsResponse {
    const SCHEMA: &'static str = "skillVersionsResponse";
}

/// The envelope for the skills attributed to one
/// session.
///
/// Models the contract's `sessionSkillsResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SessionSkillsResponse {
    /// The contract's `items`.
    #[serde(deserialize_with = "super::null_default")]
    pub items: Vec<SkillResponse>,
}

impl ContractModel for SessionSkillsResponse {
    const SCHEMA: &'static str = "sessionSkillsResponse";
}

/// The POST /v1/skills body for an authored-from-
/// scratch skill — only a name is required; the rest default to an empty
/// private draft.
///
/// Models the contract's `createSkillRequest` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateSkillRequest {
    /// The contract's `content`.
    pub content: String,

    /// The contract's `description`.
    pub description: String,

    /// The contract's `name`.
    pub name: String,

    /// The contract's `tags`.
    #[serde(deserialize_with = "super::null_default")]
    pub tags: Vec<String>,

    /// The contract's `type`.
    #[serde(rename = "type")]
    pub type_: String,
}

impl ContractModel for CreateSkillRequest {
    const SCHEMA: &'static str = "createSkillRequest";
}

/// The PUT /v1/skills/:slug body — all fields optional;
/// only present fields are applied onto the existing record.
///
/// Models the contract's `updateSkillRequest` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSkillRequest {
    /// The contract's `content`.
    pub content: String,

    /// The contract's `description`.
    pub description: String,

    /// The contract's `name`.
    pub name: String,

    /// The contract's `tags`.
    #[serde(deserialize_with = "super::null_default")]
    pub tags: Vec<String>,

    /// The contract's `type`.
    #[serde(rename = "type")]
    pub type_: String,

    /// The contract's `visibility`.
    pub visibility: String,
}

impl ContractModel for UpdateSkillRequest {
    const SCHEMA: &'static str = "updateSkillRequest";
}

/// The POST /v1/skills/:slug/versions body.
///
/// Models the contract's `publishSkillRequest` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PublishSkillRequest {
    /// The contract's `changelog`.
    pub changelog: String,

    /// The contract's `content`.
    pub content: String,
}

impl ContractModel for PublishSkillRequest {
    const SCHEMA: &'static str = "publishSkillRequest";
}

/// The POST /v1/skills/generate body. It mirrors the
/// console's GenerateSkillInput: the client nominates source sessions plus
/// optional hints, and the server is authoritative on the skill body.
///
/// Models the contract's `generateSkillRequest` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenerateSkillRequest {
    /// The contract's `hint`.
    #[serde(deserialize_with = "super::null_default")]
    pub hint: GenerateSkillRequestHint,

    /// The contract's `sessionIds`.
    #[serde(rename = "sessionIds", deserialize_with = "super::null_default")]
    pub session_ids: Vec<String>,
}

impl ContractModel for GenerateSkillRequest {
    const SCHEMA: &'static str = "generateSkillRequest";
}

/// The optional authoring hints on a generate request.
///
/// Models the contract's `GenerateSkillRequestHint` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct GenerateSkillRequestHint {
    /// The contract's `description`.
    pub description: String,

    /// The contract's `name`.
    pub name: String,

    /// The contract's `tags`.
    #[serde(deserialize_with = "super::null_default")]
    pub tags: Vec<String>,

    /// The contract's `type`.
    #[serde(rename = "type")]
    pub type_: String,
}

impl SkillsListResponse {
    /// This listing as one page of the crate's pagination convention.
    ///
    /// See [`crate::core::models::SessionListResponse::into_page`]: the skills
    /// envelope pages the same way, so it walks through the same loop.
    #[must_use]
    pub fn into_page(self) -> crate::page::Page<SkillResponse> {
        crate::page::Page {
            items: self.items,
            next_cursor: Some(self.next_cursor),
        }
    }
}
