//! Generated types from OpenAPI specification.
//!
//! This file is auto-generated. Do NOT edit manually.
//!
//! To regenerate: cd clients && task generate-rust-types

#![allow(clippy::all)]
#![allow(unused_imports)]

use serde::{Deserialize, Serialize};

/// Error types.
pub mod error {
    /// Error from a `TryFrom` or `FromStr` implementation.
    pub struct ConversionError(::std::borrow::Cow<'static, str>);
    impl ::std::error::Error for ConversionError {}
    impl ::std::fmt::Display for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Display::fmt(&self.0, f)
        }
    }
    impl ::std::fmt::Debug for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Debug::fmt(&self.0, f)
        }
    }
    impl From<&'static str> for ConversionError {
        fn from(value: &'static str) -> Self {
            Self(value.into())
        }
    }
    impl From<String> for ConversionError {
        fn from(value: String) -> Self {
            Self(value.into())
        }
    }
}
/**Client state machine state:
- **sleeping**: Initial/inactive state. Control plane running, debugger not started.
- **waking**: Transitional state during wake operation.
- **awake**: Active state. Debugger listening, registered with daemon.
*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Client state machine state:\n- **sleeping**: Initial/inactive state. Control plane running, debugger not started.\n- **waking**: Transitional state during wake operation.\n- **awake**: Active state. Debugger listening, registered with daemon.\n",
///  "type": "string",
///  "enum": [
///    "sleeping",
///    "waking",
///    "awake"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ClientState {
    #[serde(rename = "sleeping")]
    Sleeping,
    #[serde(rename = "waking")]
    Waking,
    #[serde(rename = "awake")]
    Awake,
}
impl ::std::convert::From<&Self> for ClientState {
    fn from(value: &ClientState) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for ClientState {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Sleeping => f.write_str("sleeping"),
            Self::Waking => f.write_str("waking"),
            Self::Awake => f.write_str("awake"),
        }
    }
}
impl ::std::str::FromStr for ClientState {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "sleeping" => Ok(Self::Sleeping),
            "waking" => Ok(Self::Waking),
            "awake" => Ok(Self::Awake),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ClientState {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ClientState {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ClientState {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///Error response
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Error response",
///  "type": "object",
///  "required": [
///    "error"
///  ],
///  "properties": {
///    "error": {
///      "description": "Error message",
///      "type": "string",
///      "example": "Daemon not reachable at http://127.0.0.1:8090"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct ErrorResponse {
    ///Error message
    pub error: ::std::string::String,
}
impl ::std::convert::From<&ErrorResponse> for ErrorResponse {
    fn from(value: &ErrorResponse) -> Self {
        value.clone()
    }
}
///Health check response
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Health check response",
///  "type": "object",
///  "required": [
///    "service",
///    "status"
///  ],
///  "properties": {
///    "service": {
///      "description": "Service identifier",
///      "type": "string",
///      "enum": [
///        "detrix-client"
///      ]
///    },
///    "status": {
///      "description": "Health status (always \"ok\" if the control plane is running)",
///      "type": "string",
///      "enum": [
///        "ok"
///      ]
///    }
///  },
///  "example": {
///    "service": "detrix-client",
///    "status": "ok"
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct HealthResponse {
    ///Service identifier
    pub service: HealthResponseService,
    ///Health status (always "ok" if the control plane is running)
    pub status: HealthResponseStatus,
}
impl ::std::convert::From<&HealthResponse> for HealthResponse {
    fn from(value: &HealthResponse) -> Self {
        value.clone()
    }
}
///Service identifier
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Service identifier",
///  "type": "string",
///  "enum": [
///    "detrix-client"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum HealthResponseService {
    #[serde(rename = "detrix-client")]
    DetrixClient,
}
impl ::std::convert::From<&Self> for HealthResponseService {
    fn from(value: &HealthResponseService) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for HealthResponseService {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::DetrixClient => f.write_str("detrix-client"),
        }
    }
}
impl ::std::str::FromStr for HealthResponseService {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "detrix-client" => Ok(Self::DetrixClient),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for HealthResponseService {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for HealthResponseService {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for HealthResponseService {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///Health status (always "ok" if the control plane is running)
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Health status (always \"ok\" if the control plane is running)",
///  "type": "string",
///  "enum": [
///    "ok"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum HealthResponseStatus {
    #[serde(rename = "ok")]
    Ok,
}
impl ::std::convert::From<&Self> for HealthResponseStatus {
    fn from(value: &HealthResponseStatus) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for HealthResponseStatus {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Ok => f.write_str("ok"),
        }
    }
}
impl ::std::str::FromStr for HealthResponseStatus {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "ok" => Ok(Self::Ok),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for HealthResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for HealthResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for HealthResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///Static client process information
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Static client process information",
///  "type": "object",
///  "required": [
///    "name",
///    "pid"
///  ],
///  "properties": {
///    "go_version": {
///      "description": "Go version (Go client only)",
///      "type": "string",
///      "example": "go1.22.0"
///    },
///    "name": {
///      "description": "Connection name for this client",
///      "type": "string",
///      "example": "my-service-12345"
///    },
///    "pid": {
///      "description": "Process ID of the client",
///      "type": "integer",
///      "format": "int64",
///      "example": 12345
///    },
///    "python_executable": {
///      "description": "Python executable path (Python client only)",
///      "type": "string",
///      "example": "/usr/bin/python3"
///    },
///    "python_version": {
///      "description": "Python version (Python client only)",
///      "type": "string",
///      "example": "3.12.0 (main, Oct  2 2023, 00:00:00)"
///    },
///    "rust_version": {
///      "description": "Rust version (Rust client only)",
///      "type": "string",
///      "example": "1.75.0"
///    }
///  },
///  "example": {
///    "name": "my-service-12345",
///    "pid": 12345,
///    "python_executable": "/usr/bin/python3",
///    "python_version": "3.12.0"
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct InfoResponse {
    ///Go version (Go client only)
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub go_version: ::std::option::Option<::std::string::String>,
    ///Connection name for this client
    pub name: ::std::string::String,
    ///Process ID of the client
    pub pid: i64,
    ///Python executable path (Python client only)
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub python_executable: ::std::option::Option<::std::string::String>,
    ///Python version (Python client only)
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub python_version: ::std::option::Option<::std::string::String>,
    ///Rust version (Rust client only)
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub rust_version: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&InfoResponse> for InfoResponse {
    fn from(value: &InfoResponse) -> Self {
        value.clone()
    }
}
///Response from sleep operation
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Response from sleep operation",
///  "type": "object",
///  "required": [
///    "status"
///  ],
///  "properties": {
///    "status": {
///      "description": "\"sleeping\" if the client transitioned from awake to sleeping.\n\"already_sleeping\" if the client was already in sleeping state.\n",
///      "type": "string",
///      "enum": [
///        "sleeping",
///        "already_sleeping"
///      ]
///    }
///  },
///  "example": {
///    "status": "sleeping"
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct SleepResponse {
    /**"sleeping" if the client transitioned from awake to sleeping.
    "already_sleeping" if the client was already in sleeping state.
    */
    pub status: SleepResponseStatus,
}
impl ::std::convert::From<&SleepResponse> for SleepResponse {
    fn from(value: &SleepResponse) -> Self {
        value.clone()
    }
}
/**"sleeping" if the client transitioned from awake to sleeping.
"already_sleeping" if the client was already in sleeping state.
*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "\"sleeping\" if the client transitioned from awake to sleeping.\n\"already_sleeping\" if the client was already in sleeping state.\n",
///  "type": "string",
///  "enum": [
///    "sleeping",
///    "already_sleeping"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum SleepResponseStatus {
    #[serde(rename = "sleeping")]
    Sleeping,
    #[serde(rename = "already_sleeping")]
    AlreadySleeping,
}
impl ::std::convert::From<&Self> for SleepResponseStatus {
    fn from(value: &SleepResponseStatus) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for SleepResponseStatus {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Sleeping => f.write_str("sleeping"),
            Self::AlreadySleeping => f.write_str("already_sleeping"),
        }
    }
}
impl ::std::str::FromStr for SleepResponseStatus {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "sleeping" => Ok(Self::Sleeping),
            "already_sleeping" => Ok(Self::AlreadySleeping),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for SleepResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for SleepResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for SleepResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
///Current client status
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Current client status",
///  "type": "object",
///  "required": [
///    "control_host",
///    "control_port",
///    "daemon_url",
///    "debug_port",
///    "debug_port_active",
///    "name",
///    "state"
///  ],
///  "properties": {
///    "connection_id": {
///      "description": "Connection ID assigned by the daemon upon registration.\nNull if not currently registered (state is \"sleeping\").\n",
///      "type": "string",
///      "example": "conn_abc123",
///      "nullable": true
///    },
///    "control_host": {
///      "description": "Host the control plane is bound to",
///      "type": "string",
///      "example": "127.0.0.1"
///    },
///    "control_port": {
///      "description": "Actual port the control plane is listening on.\n0 if the control plane has not been started yet.\n",
///      "type": "integer",
///      "format": "int32",
///      "maximum": 65535.0,
///      "minimum": 0.0,
///      "example": 8091
///    },
///    "daemon_url": {
///      "description": "URL of the Detrix daemon this client connects to",
///      "type": "string",
///      "example": "http://127.0.0.1:8090"
///    },
///    "debug_port": {
///      "description": "Debug adapter port. 0 if debugger has never been started.\nMay be non-zero even when sleeping due to debugger limitations.\n",
///      "type": "integer",
///      "format": "int32",
///      "maximum": 65535.0,
///      "minimum": 0.0,
///      "example": 5678
///    },
///    "debug_port_active": {
///      "description": "True if the debug port is actually open and accepting connections.\nThis may be true even when state is \"sleeping\" because some debuggers\n(e.g., debugpy) cannot stop their listener once started.\n",
///      "type": "boolean",
///      "example": true
///    },
///    "name": {
///      "description": "Connection name for this client",
///      "type": "string",
///      "example": "my-service-12345"
///    },
///    "state": {
///      "$ref": "#/components/schemas/ClientState"
///    }
///  },
///  "example": {
///    "connection_id": "conn_abc123",
///    "control_host": "127.0.0.1",
///    "control_port": 8091,
///    "daemon_url": "http://127.0.0.1:8090",
///    "debug_port": 5678,
///    "debug_port_active": true,
///    "name": "my-service-12345",
///    "state": "awake"
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct StatusResponse {
    /**Connection ID assigned by the daemon upon registration.
    Null if not currently registered (state is "sleeping").
    */
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub connection_id: ::std::option::Option<::std::string::String>,
    ///Host the control plane is bound to
    pub control_host: ::std::string::String,
    /**Actual port the control plane is listening on.
    0 if the control plane has not been started yet.
    */
    pub control_port: i32,
    ///URL of the Detrix daemon this client connects to
    pub daemon_url: ::std::string::String,
    /**Debug adapter port. 0 if debugger has never been started.
    May be non-zero even when sleeping due to debugger limitations.
    */
    pub debug_port: i32,
    /**True if the debug port is actually open and accepting connections.
    This may be true even when state is "sleeping" because some debuggers
    (e.g., debugpy) cannot stop their listener once started.
    */
    pub debug_port_active: bool,
    ///Connection name for this client
    pub name: ::std::string::String,
    pub state: ClientState,
}
impl ::std::convert::From<&StatusResponse> for StatusResponse {
    fn from(value: &StatusResponse) -> Self {
        value.clone()
    }
}
///Optional parameters for wake operation
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Optional parameters for wake operation",
///  "type": "object",
///  "properties": {
///    "daemon_url": {
///      "description": "Override the daemon URL for this wake operation.\nIf not provided, uses the URL configured during init().\n",
///      "type": "string",
///      "format": "uri",
///      "example": "http://192.168.1.100:8090"
///    }
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct WakeRequest {
    /**Override the daemon URL for this wake operation.
    If not provided, uses the URL configured during init().
    */
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub daemon_url: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&WakeRequest> for WakeRequest {
    fn from(value: &WakeRequest) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for WakeRequest {
    fn default() -> Self {
        Self {
            daemon_url: Default::default(),
        }
    }
}
///Response from wake operation
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "Response from wake operation",
///  "type": "object",
///  "required": [
///    "connection_id",
///    "debug_port",
///    "status"
///  ],
///  "properties": {
///    "connection_id": {
///      "description": "Connection ID assigned by the daemon",
///      "type": "string",
///      "example": "conn_abc123"
///    },
///    "debug_port": {
///      "description": "Port the debug adapter is listening on",
///      "type": "integer",
///      "format": "int32",
///      "maximum": 65535.0,
///      "minimum": 1.0,
///      "example": 5678
///    },
///    "status": {
///      "description": "\"awake\" if the client transitioned from sleeping to awake.\n\"already_awake\" if the client was already in awake state.\n",
///      "type": "string",
///      "enum": [
///        "awake",
///        "already_awake"
///      ]
///    }
///  },
///  "example": {
///    "connection_id": "conn_abc123",
///    "debug_port": 5678,
///    "status": "awake"
///  }
///}
/// ```
/// </details>
#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct WakeResponse {
    ///Connection ID assigned by the daemon
    pub connection_id: ::std::string::String,
    ///Port the debug adapter is listening on
    pub debug_port: i32,
    /**"awake" if the client transitioned from sleeping to awake.
    "already_awake" if the client was already in awake state.
    */
    pub status: WakeResponseStatus,
}
impl ::std::convert::From<&WakeResponse> for WakeResponse {
    fn from(value: &WakeResponse) -> Self {
        value.clone()
    }
}
/**"awake" if the client transitioned from sleeping to awake.
"already_awake" if the client was already in awake state.
*/
///
/// <details><summary>JSON schema</summary>
///
/// ```json
///{
///  "description": "\"awake\" if the client transitioned from sleeping to awake.\n\"already_awake\" if the client was already in awake state.\n",
///  "type": "string",
///  "enum": [
///    "awake",
///    "already_awake"
///  ]
///}
/// ```
/// </details>
#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum WakeResponseStatus {
    #[serde(rename = "awake")]
    Awake,
    #[serde(rename = "already_awake")]
    AlreadyAwake,
}
impl ::std::convert::From<&Self> for WakeResponseStatus {
    fn from(value: &WakeResponseStatus) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for WakeResponseStatus {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Awake => f.write_str("awake"),
            Self::AlreadyAwake => f.write_str("already_awake"),
        }
    }
}
impl ::std::str::FromStr for WakeResponseStatus {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "awake" => Ok(Self::Awake),
            "already_awake" => Ok(Self::AlreadyAwake),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for WakeResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for WakeResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for WakeResponseStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
