//! LSP protocol message types
//!
//! Defines JSON-RPC message structures for LSP communication.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC version constant
pub const JSONRPC_VERSION: &str = "2.0";

/// Base LSP message (can be request, response, or notification)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LspMessage {
    Request(LspRequest),
    Response(LspResponse),
    Notification(LspNotification),
}

/// JSON-RPC request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl LspRequest {
    /// Create a new LSP request
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<LspResponseError>,
}

impl LspResponse {
    /// Check if this is a successful response
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Get the result or error
    pub fn into_result(self) -> Result<Value, LspResponseError> {
        if let Some(error) = self.error {
            Err(error)
        } else {
            Ok(self.result.unwrap_or(Value::Null))
        }
    }
}

/// JSON-RPC response error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspResponseError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC notification (no response expected)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl LspNotification {
    /// Create a new LSP notification
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// Request ID (can be integer or string)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Int(i64),
    String(String),
}

impl From<i64> for RequestId {
    fn from(id: i64) -> Self {
        RequestId::Int(id)
    }
}

impl From<String> for RequestId {
    fn from(id: String) -> Self {
        RequestId::String(id)
    }
}

impl From<&str> for RequestId {
    fn from(id: &str) -> Self {
        RequestId::String(id.to_string())
    }
}

// ============================================================================
// LSP-specific types for Call Hierarchy
// ============================================================================

/// Position in a text document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// Range in a text document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Location in a text document
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// Text document identifier
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentIdentifier {
    pub uri: String,
}

/// Call hierarchy item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyItem {
    /// Name of the symbol
    pub name: String,
    /// Kind of the symbol (function, method, etc.)
    pub kind: SymbolKind,
    /// Tags (deprecated, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<u32>>,
    /// More detail like signature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// URI of the document
    pub uri: String,
    /// Range of the symbol
    pub range: Range,
    /// Selection range (name range)
    pub selection_range: Range,
    /// Custom data (preserved across requests)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Outgoing call from a call hierarchy item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyOutgoingCall {
    /// The item being called
    pub to: CallHierarchyItem,
    /// Ranges where this call happens
    pub from_ranges: Vec<Range>,
}

/// Symbol kind enum (subset of LSP SymbolKind)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolKind(pub u32);

#[allow(dead_code)]
impl SymbolKind {
    pub const FILE: SymbolKind = SymbolKind(1);
    pub const MODULE: SymbolKind = SymbolKind(2);
    pub const NAMESPACE: SymbolKind = SymbolKind(3);
    pub const PACKAGE: SymbolKind = SymbolKind(4);
    pub const CLASS: SymbolKind = SymbolKind(5);
    pub const METHOD: SymbolKind = SymbolKind(6);
    pub const PROPERTY: SymbolKind = SymbolKind(7);
    pub const FIELD: SymbolKind = SymbolKind(8);
    pub const CONSTRUCTOR: SymbolKind = SymbolKind(9);
    pub const ENUM: SymbolKind = SymbolKind(10);
    pub const INTERFACE: SymbolKind = SymbolKind(11);
    pub const FUNCTION: SymbolKind = SymbolKind(12);
    pub const VARIABLE: SymbolKind = SymbolKind(13);
    pub const CONSTANT: SymbolKind = SymbolKind(14);
}

// ============================================================================
// LSP request/response parameter types
// ============================================================================

/// Parameters for initialize request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Process ID of the parent process
    pub process_id: Option<i32>,
    /// Root path (deprecated, use root_uri)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    /// Root URI
    pub root_uri: Option<String>,
    /// Client capabilities
    pub capabilities: ClientCapabilities,
    /// Initial workspace folders
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_folders: Option<Vec<WorkspaceFolder>>,
}

/// Client capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document: Option<TextDocumentClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceClientCapabilities>,
}

/// Text document client capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_hierarchy: Option<CallHierarchyClientCapabilities>,
}

/// Call hierarchy client capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyClientCapabilities {
    #[serde(default)]
    pub dynamic_registration: bool,
}

/// Workspace client capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_folders: Option<bool>,
}

/// Workspace folder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFolder {
    pub uri: String,
    pub name: String,
}

/// Initialize result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<ServerInfo>,
}

/// Server capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_hierarchy_provider: Option<bool>,
    // Add more as needed
}

/// Server info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Parameters for textDocument/prepareCallHierarchy
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyPrepareParams {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
}

/// Parameters for callHierarchy/outgoingCalls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallHierarchyOutgoingCallsParams {
    pub item: CallHierarchyItem,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = LspRequest::new(1i64, "initialize", None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn test_response_success() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#;
        let resp: LspResponse = serde_json::from_str(json).unwrap();
        assert!(resp.is_success());
        assert!(resp.result.is_some());
    }

    #[test]
    fn test_response_error() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid request"}}"#;
        let resp: LspResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.is_success());
        assert_eq!(resp.error.as_ref().unwrap().code, -32600);
    }

    #[test]
    fn test_call_hierarchy_item() {
        let json = r#"{
            "name": "get_data",
            "kind": 12,
            "uri": "file:///test.py",
            "range": {"start": {"line": 10, "character": 0}, "end": {"line": 15, "character": 0}},
            "selectionRange": {"start": {"line": 10, "character": 4}, "end": {"line": 10, "character": 12}}
        }"#;
        let item: CallHierarchyItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.name, "get_data");
        assert_eq!(item.kind, SymbolKind::FUNCTION);
    }
}
