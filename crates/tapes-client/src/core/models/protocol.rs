//! Shapes that belong to a protocol rather than to the read model: the error
//! body every non-success answer carries, and the JSON-RPC frames of the MCP
//! endpoint.
//!
//! The MCP frames are modelled because they are response schemas of a sealed
//! operation and the gate accounts for every one of those. Typing a frame is
//! not the same as offering an MCP session, and this crate does not offer one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContractModel;

/// An error body, as every non-success tapes response spells one.
///
/// Models the contract's `ErrorResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ErrorResponse {
    /// The contract's `error`.
    pub error: String,
}

impl ContractModel for ErrorResponse {
    const SCHEMA: &'static str = "ErrorResponse";
}

/// A JSON-RPC 2.0 request to the streamable MCP endpoint.
///
/// Models the contract's `MCPRequest` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpRequest {
    /// ID correlates a response with this request. `None` is a notification,
    /// and is omitted from the wire rather than sent as an empty string —
    /// JSON-RPC reads a present id as "answer me", so a notification that
    /// carried one would not be a notification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// The protocol version, always "2.0".
    pub jsonrpc: String,

    /// The MCP method being invoked, such as tools/call.
    pub method: String,

    /// The method's arguments.
    #[serde(deserialize_with = "super::null_default")]
    pub params: BTreeMap<String, Value>,
}

impl ContractModel for McpRequest {
    const SCHEMA: &'static str = "MCPRequest";
}

/// A JSON-RPC 2.0 response from the streamable MCP endpoint.
///
/// Models the contract's `MCPResponse` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct McpResponse {
    /// The contract's `error`.
    #[serde(deserialize_with = "super::null_default")]
    pub error: McpError,

    /// The id of the request this answers.
    pub id: String,

    /// The protocol version, always "2.0".
    pub jsonrpc: String,

    /// The method's return value on success.
    #[serde(deserialize_with = "super::null_default")]
    pub result: BTreeMap<String, Value>,
}

impl ContractModel for McpResponse {
    const SCHEMA: &'static str = "MCPResponse";
}

/// A JSON-RPC 2.0 error object.
///
/// Models the contract's `MCPError` schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct McpError {
    /// The JSON-RPC error code.
    pub code: i32,

    /// A short description of the failure.
    pub message: String,
}

impl ContractModel for McpError {
    const SCHEMA: &'static str = "MCPError";
}
