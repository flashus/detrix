//! HTTP path constants

pub const HEALTH: &str = "/health";
pub const STATUS: &str = "/status";
pub const PROMETHEUS_METRICS: &str = "/metrics";

pub const WS: &str = "/ws";

pub const MCP: &str = "/mcp";
pub const MCP_HEARTBEAT: &str = "/mcp/heartbeat";
pub const MCP_DISCONNECT: &str = "/mcp/disconnect";

pub const API_V1_WAKE: &str = "/api/v1/wake";
pub const API_V1_SLEEP: &str = "/api/v1/sleep";
pub const API_V1_STATUS: &str = "/api/v1/status";

pub const API_V1_METRICS: &str = "/api/v1/metrics";
pub const API_V1_METRIC_BY_ID: &str = "/api/v1/metrics/{id}";
pub const API_V1_METRIC_ENABLE: &str = "/api/v1/metrics/{id}/enable";
pub const API_V1_METRIC_DISABLE: &str = "/api/v1/metrics/{id}/disable";
pub const API_V1_METRIC_VALUE: &str = "/api/v1/metrics/{id}/value";
pub const API_V1_METRIC_HISTORY: &str = "/api/v1/metrics/{id}/history";

pub const API_V1_EVENTS: &str = "/api/v1/events";

pub const API_V1_GROUPS: &str = "/api/v1/groups";
pub const API_V1_GROUP_METRICS: &str = "/api/v1/groups/{name}/metrics";
pub const API_V1_GROUP_ENABLE: &str = "/api/v1/groups/{name}/enable";
pub const API_V1_GROUP_DISABLE: &str = "/api/v1/groups/{name}/disable";

pub const API_V1_CONNECTIONS: &str = "/api/v1/connections";
pub const API_V1_CONNECTION_BY_ID: &str = "/api/v1/connections/{id}";
pub const API_V1_CONNECTIONS_CLEANUP: &str = "/api/v1/connections/cleanup";

pub const API_V1_CONFIG: &str = "/api/v1/config";
pub const API_V1_CONFIG_RELOAD: &str = "/api/v1/config/reload";
pub const API_V1_CONFIG_VALIDATE: &str = "/api/v1/config/validate";

pub const API_V1_VALIDATE_EXPRESSION: &str = "/api/v1/validate_expression";
pub const API_V1_INSPECT_FILE: &str = "/api/v1/inspect_file";

pub const API_V1_MCP_USAGE: &str = "/api/v1/mcp/usage";
