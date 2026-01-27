//! DAP (Debug Adapter Protocol) message types
//!
//! Based on <https://microsoft.github.io/debug-adapter-protocol/specification>
//!
//! The protocol uses JSON-RPC style messages with Content-Length headers:
//! ```text
//! Content-Length: 119\r\n
//! \r\n
//! {"seq":1,"type":"request","command":"initialize","arguments":{"adapterID":"python"}}
//! ```

use serde::{Deserialize, Serialize};

// ============================================================
// BASE PROTOCOL MESSAGE
// ============================================================

/// Base protocol message - all DAP messages extend this
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProtocolMessage {
    /// Request message from client to adapter
    Request(Request),
    /// Response message from adapter to client
    Response(Response),
    /// Event notification from adapter to client
    Event(Event),
}

impl ProtocolMessage {
    /// Get the sequence number of this message
    pub fn seq(&self) -> i64 {
        match self {
            ProtocolMessage::Request(r) => r.seq,
            ProtocolMessage::Response(r) => r.seq,
            ProtocolMessage::Event(e) => e.seq,
        }
    }

    /// Create a request message
    pub fn request(seq: i64, command: String, arguments: serde_json::Value) -> Self {
        ProtocolMessage::Request(Request {
            seq,
            command,
            arguments: Some(arguments),
        })
    }

    /// Create a response message
    pub fn response(
        seq: i64,
        request_seq: i64,
        command: String,
        success: bool,
        body: Option<serde_json::Value>,
        message: Option<String>,
    ) -> Self {
        ProtocolMessage::Response(Response {
            seq,
            request_seq,
            command,
            success,
            message,
            body,
        })
    }

    /// Create an event message
    pub fn event(seq: i64, event: String, body: Option<serde_json::Value>) -> Self {
        ProtocolMessage::Event(Event { seq, event, body })
    }
}

// ============================================================
// REQUEST
// ============================================================

/// Request message sent from client to adapter
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Sequence number for message ordering
    pub seq: i64,
    /// Command to execute
    pub command: String,
    /// Command-specific arguments
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
}

impl Request {
    pub fn new(seq: i64, command: impl Into<String>) -> Self {
        Self {
            seq,
            command: command.into(),
            arguments: None,
        }
    }

    pub fn with_arguments(mut self, arguments: serde_json::Value) -> Self {
        self.arguments = Some(arguments);
        self
    }
}

// ============================================================
// RESPONSE
// ============================================================

/// Response message sent from adapter to client
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// Sequence number
    pub seq: i64,
    /// Sequence number of the corresponding request
    pub request_seq: i64,
    /// Command this response is for
    pub command: String,
    /// Success indicator
    pub success: bool,
    /// Error message if not successful
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Response body
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

impl Response {
    pub fn success(seq: i64, request_seq: i64, command: impl Into<String>) -> Self {
        Self {
            seq,
            request_seq,
            command: command.into(),
            success: true,
            message: None,
            body: None,
        }
    }

    pub fn error(
        seq: i64,
        request_seq: i64,
        command: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            seq,
            request_seq,
            command: command.into(),
            success: false,
            message: Some(message.into()),
            body: None,
        }
    }

    pub fn with_body(mut self, body: serde_json::Value) -> Self {
        self.body = Some(body);
        self
    }
}

// ============================================================
// EVENT
// ============================================================

/// Event notification sent from adapter to client
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Sequence number
    pub seq: i64,
    /// Event type
    pub event: String,
    /// Event-specific data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

impl Event {
    pub fn new(seq: i64, event: impl Into<String>) -> Self {
        Self {
            seq,
            event: event.into(),
            body: None,
        }
    }

    pub fn with_body(mut self, body: serde_json::Value) -> Self {
        self.body = Some(body);
        self
    }
}

// ============================================================
// INITIALIZE REQUEST/RESPONSE
// ============================================================

/// Arguments for initialize request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequestArguments {
    /// Unique ID of the client (e.g. "vscode")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Human-readable client name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    /// ID of the debug adapter ("python", "go", "rust")
    #[serde(rename = "adapterID")]
    pub adapter_id: String,
    /// Locale (ISO 639)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Lines start at 1 (default) or 0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_start_at1: Option<bool>,
    /// Columns start at 1 (default) or 0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns_start_at1: Option<bool>,
    /// Path format ("path" or "uri")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_format: Option<String>,
}

/// Capabilities returned in initialize response
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Adapter supports conditional breakpoints
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_conditional_breakpoints: Option<bool>,
    /// Adapter supports hit conditional breakpoints
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_hit_conditional_breakpoints: Option<bool>,
    /// Adapter supports logpoints (log messages without breaking)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_log_points: Option<bool>,
    /// Adapter supports function breakpoints
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_function_breakpoints: Option<bool>,
    /// Adapter supports continue to location
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_goto_targets_request: Option<bool>,
}

// ============================================================
// SETBREAKPOINTS REQUEST
// ============================================================

/// Arguments for setBreakpoints request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBreakpointsArguments {
    /// Source file location
    pub source: Source,
    /// Breakpoint specifications
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakpoints: Option<Vec<SourceBreakpoint>>,
    /// Source has been modified (optional optimization)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_modified: Option<bool>,
}

/// Source file reference
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// File path (absolute or relative)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Source name (for display)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Source reference ID (for sources without path)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<i64>,
}

impl Source {
    pub fn from_path(path: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            name: None,
            source_reference: None,
        }
    }
}

/// Source breakpoint specification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBreakpoint {
    /// Line number (1-based by default)
    pub line: u32,
    /// Column number (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Condition expression - only break if true
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Hit condition - break after N hits
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    /// Log message - emit this instead of breaking (LOGPOINT!)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

impl SourceBreakpoint {
    /// Create a simple breakpoint at a line
    pub fn at_line(line: u32) -> Self {
        Self {
            line,
            column: None,
            condition: None,
            hit_condition: None,
            log_message: None,
        }
    }

    /// Create a logpoint (non-breaking) that logs a message
    pub fn logpoint(line: u32, message: impl Into<String>) -> Self {
        Self {
            line,
            column: None,
            condition: None,
            hit_condition: None,
            log_message: Some(message.into()),
        }
    }

    /// Add a condition to this breakpoint
    pub fn with_condition(mut self, condition: impl Into<String>) -> Self {
        self.condition = Some(condition.into());
        self
    }
}

/// Response body for setBreakpoints
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBreakpointsResponseBody {
    /// Actual breakpoints created
    pub breakpoints: Vec<Breakpoint>,
}

/// Breakpoint information returned by adapter
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    /// Breakpoint ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// Whether breakpoint was successfully verified
    pub verified: bool,
    /// Optional message about breakpoint status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Actual source location (may differ from requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Actual line (may differ from requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

// ============================================================
// OUTPUT EVENT (for capturing logpoint output)
// ============================================================

/// Output event body
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputEventBody {
    /// Category of output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<OutputCategory>,
    /// Output text
    pub output: String,
    /// Source location that produced output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Line number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Variables reference for structured output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables_reference: Option<i64>,
}

/// Output category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputCategory {
    Console,
    Stdout,
    Stderr,
    Telemetry,
}

// ============================================================
// CONTINUE, LAUNCH, ATTACH
// ============================================================

/// Continue request arguments
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueArguments {
    /// Thread to continue
    pub thread_id: i64,
    /// Continue only this thread
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_thread: Option<bool>,
}

/// Launch request arguments (program-specific)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequestArguments {
    /// Launch without debugging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_debug: Option<bool>,
    /// Restart data from previous session
    #[serde(skip_serializing_if = "Option::is_none", rename = "__restart")]
    pub restart: Option<serde_json::Value>,
    /// Additional adapter-specific arguments stored as JSON
    #[serde(flatten)]
    pub additional: serde_json::Map<String, serde_json::Value>,
}

/// Attach request arguments (process-specific)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachRequestArguments {
    /// Restart data from previous session
    #[serde(skip_serializing_if = "Option::is_none", rename = "__restart")]
    pub restart: Option<serde_json::Value>,
    /// Additional adapter-specific arguments stored as JSON
    #[serde(flatten)]
    pub additional: serde_json::Map<String, serde_json::Value>,
}

// ============================================================
// STOPPED EVENT (for introspection capture)
// ============================================================

/// Stopped event body - sent when execution stops at a breakpoint
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoppedEventBody {
    /// Reason for stopping (e.g., "breakpoint", "step", "exception")
    pub reason: String,
    /// Optional description of the reason
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Thread that stopped
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    /// If true, all threads are stopped
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_threads_stopped: Option<bool>,
    /// Breakpoint IDs that caused the stop
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_breakpoint_ids: Option<Vec<i64>>,
}

// ============================================================
// STACK TRACE (for introspection)
// ============================================================

/// Arguments for stackTrace request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTraceArguments {
    /// Thread to get stack trace for
    pub thread_id: i64,
    /// Start frame index (for pagination)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_frame: Option<i64>,
    /// Maximum number of frames to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub levels: Option<i64>,
    /// Format for values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<StackFrameFormat>,
}

/// Stack frame format options
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrameFormat {
    /// Include function parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<bool>,
    /// Include parameter types
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_types: Option<bool>,
    /// Include parameter names
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_names: Option<bool>,
    /// Include parameter values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_values: Option<bool>,
    /// Include line numbers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<bool>,
    /// Include module name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<bool>,
}

/// Stack trace response body
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTraceResponseBody {
    /// Stack frames
    pub stack_frames: Vec<DapStackFrame>,
    /// Total number of frames (if not all were returned)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_frames: Option<i64>,
}

/// A stack frame from DAP
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DapStackFrame {
    /// Unique frame ID
    pub id: i64,
    /// Name of the frame (typically function name)
    pub name: String,
    /// Source location
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Line number in source
    pub line: i64,
    /// Column number in source
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<i64>,
    /// End line (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i64>,
    /// End column (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<i64>,
    /// Module name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_id: Option<serde_json::Value>,
    /// Presentation hint
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation_hint: Option<String>,
}

// ============================================================
// SCOPES (for variable inspection)
// ============================================================

/// Arguments for scopes request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopesArguments {
    /// Frame ID to get scopes for
    pub frame_id: i64,
}

/// Scopes response body
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopesResponseBody {
    /// Available scopes
    pub scopes: Vec<DapScope>,
}

/// A scope (locals, globals, etc.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DapScope {
    /// Scope name
    pub name: String,
    /// Presentation hint
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation_hint: Option<String>,
    /// Variables reference (use with variables request)
    pub variables_reference: i64,
    /// Number of named variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_variables: Option<i64>,
    /// Number of indexed variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_variables: Option<i64>,
    /// True if this scope's variables are expensive to fetch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expensive: Option<bool>,
    /// Source location for this scope
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Start line
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    /// Start column
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<i64>,
    /// End line
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i64>,
    /// End column
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<i64>,
}

// ============================================================
// VARIABLES (for memory snapshot)
// ============================================================

/// Arguments for variables request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariablesArguments {
    /// Variables reference from scope or other variable
    pub variables_reference: i64,
    /// Filter variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Start index (for indexed variables)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    /// Number of variables to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// Format for values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ValueFormat>,
}

/// Value format options
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueFormat {
    /// Display value as hex
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hex: Option<bool>,
}

/// Variables response body
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariablesResponseBody {
    /// Variables
    pub variables: Vec<DapVariable>,
}

/// A variable from DAP
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DapVariable {
    /// Variable name
    pub name: String,
    /// Variable value
    pub value: String,
    /// Variable type
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub var_type: Option<String>,
    /// Presentation hint
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation_hint: Option<VariablePresentationHint>,
    /// Evaluate name (for evaluating this variable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluate_name: Option<String>,
    /// Variables reference (non-zero if variable has children)
    #[serde(default)]
    pub variables_reference: i64,
    /// Named child count
    #[serde(skip_serializing_if = "Option::is_none")]
    pub named_variables: Option<i64>,
    /// Indexed child count
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_variables: Option<i64>,
    /// Memory reference
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_reference: Option<String>,
}

/// Variable presentation hint
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariablePresentationHint {
    /// Kind of variable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Attributes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<String>>,
    /// Visibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Lazy evaluation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lazy: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_message_request_serialization() {
        let req = Request::new(1, "initialize")
            .with_arguments(serde_json::json!({"adapterID": "python"}));

        let msg = ProtocolMessage::Request(req);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"request"#));
        assert!(json.contains(r#""command":"initialize"#));
        assert!(json.contains(r#""adapterID":"python"#));

        // Verify deserialization round-trip
        let parsed: ProtocolMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_protocol_message_response_serialization() {
        let resp = Response::success(2, 1, "initialize")
            .with_body(serde_json::json!({"supportsLogPoints": true}));

        let msg = ProtocolMessage::Response(resp);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"response"#));
        assert!(json.contains(r#""success":true"#));
        assert!(json.contains(r#""supportsLogPoints":true"#));

        let parsed: ProtocolMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_protocol_message_event_serialization() {
        let event = Event::new(3, "output")
            .with_body(serde_json::json!({"output": "Hello, world!", "category": "stdout"}));

        let msg = ProtocolMessage::Event(event);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"event"#));
        assert!(json.contains(r#""event":"output"#));
        assert!(json.contains(r#""output":"Hello, world!"#));

        let parsed: ProtocolMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_initialize_request_arguments() {
        let args = InitializeRequestArguments {
            client_id: Some("detrix".to_string()),
            client_name: Some("Detrix CLI".to_string()),
            adapter_id: "python".to_string(),
            locale: None,
            lines_start_at1: Some(true),
            columns_start_at1: Some(true),
            path_format: Some("path".to_string()),
        };

        let json = serde_json::to_string(&args).unwrap();
        // adapterID is explicitly renamed (not camelCase) per DAP spec
        assert!(json.contains(r#""adapterID":"python"#));
        assert!(json.contains(r#""clientId":"detrix"#));

        let parsed: InitializeRequestArguments = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, args);
    }

    #[test]
    fn test_source_breakpoint_logpoint() {
        let logpoint = SourceBreakpoint::logpoint(42, "{\"user_id\": user.id}");

        assert_eq!(logpoint.line, 42);
        assert_eq!(
            logpoint.log_message,
            Some("{\"user_id\": user.id}".to_string())
        );
        assert!(logpoint.condition.is_none());

        let json = serde_json::to_string(&logpoint).unwrap();
        assert!(json.contains(r#""line":42"#));
        assert!(json.contains(r#""logMessage"#));
    }

    #[test]
    fn test_source_breakpoint_with_condition() {
        let bp = SourceBreakpoint::at_line(10).with_condition("x > 5");

        assert_eq!(bp.line, 10);
        assert_eq!(bp.condition, Some("x > 5".to_string()));
        assert!(bp.log_message.is_none());
    }

    #[test]
    fn test_setbreakpoints_arguments() {
        let args = SetBreakpointsArguments {
            source: Source::from_path("/path/to/file.py"),
            breakpoints: Some(vec![
                SourceBreakpoint::at_line(1),
                SourceBreakpoint::logpoint(5, "value={value}"),
            ]),
            source_modified: Some(false),
        };

        let json = serde_json::to_string(&args).unwrap();
        assert!(json.contains(r#""path":"/path/to/file.py"#));
        assert!(json.contains(r#""line":1"#));
        assert!(json.contains(r#""line":5"#));
        assert!(json.contains(r#""logMessage":"value={value}"#));

        let parsed: SetBreakpointsArguments = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, args);
    }

    #[test]
    fn test_output_event_body() {
        let output = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "metric value: 42\n".to_string(),
            source: Some(Source::from_path("app.py")),
            line: Some(127),
            column: None,
            variables_reference: None,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains(r#""category":"stdout"#));
        assert!(json.contains(r#""output":"metric value: 42\n"#));
        assert!(json.contains(r#""line":127"#));

        let parsed: OutputEventBody = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, output);
    }

    #[test]
    fn test_capabilities_serialization() {
        let caps = Capabilities {
            supports_conditional_breakpoints: Some(true),
            supports_hit_conditional_breakpoints: Some(true),
            supports_log_points: Some(true),
            supports_function_breakpoints: Some(false),
            supports_goto_targets_request: None,
        };

        let json = serde_json::to_string(&caps).unwrap();
        assert!(json.contains(r#""supportsLogPoints":true"#));
        assert!(json.contains(r#""supportsConditionalBreakpoints":true"#));

        let parsed: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, caps);
    }

    #[test]
    fn test_protocol_message_seq() {
        let req_msg = ProtocolMessage::request(10, "initialize".to_string(), serde_json::json!({}));
        assert_eq!(req_msg.seq(), 10);

        let resp_msg =
            ProtocolMessage::response(11, 10, "initialize".to_string(), true, None, None);
        assert_eq!(resp_msg.seq(), 11);

        let event_msg = ProtocolMessage::event(12, "initialized".to_string(), None);
        assert_eq!(event_msg.seq(), 12);
    }

    #[test]
    fn test_response_error() {
        let resp = Response::error(5, 4, "setBreakpoints", "File not found");

        assert!(!resp.success);
        assert_eq!(resp.message, Some("File not found".to_string()));
        assert_eq!(resp.request_seq, 4);
        assert_eq!(resp.command, "setBreakpoints");
    }

    #[test]
    fn test_continue_arguments() {
        let args = ContinueArguments {
            thread_id: 1,
            single_thread: Some(false),
        };

        let json = serde_json::to_string(&args).unwrap();
        assert!(json.contains(r#""threadId":1"#));
        assert!(json.contains(r#""singleThread":false"#));

        let parsed: ContinueArguments = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, args);
    }
}
