//! Default constants for Detrix configuration
//!
//! This module centralizes most of the constants used throughout the codebase,
//! providing a single source of truth for default values.
//! Everything that is configurable should be here.
//!
//! # Organization
//!
//! Constants are grouped by category:
//! - Hosts: Network host defaults
//! - Ports: Network port defaults
//! - Timeouts: Various timeout durations in milliseconds
//! - Intervals: Polling and check intervals
//! - Batching/Buffering: Event processing configuration
//! - Channel Capacities: Internal channel buffer sizes
//! - Limits: Size and count limits
//! - Rate Limiting: Request throttling
//! - Retry/Reconnection: Backoff and retry behavior
//! - Retention: Data lifecycle
//! - Safety: Expression evaluation limits
//! - TUI: Terminal UI settings
//! - Compression: Gzip settings
//! - MCP: Model Context Protocol settings

// ============================================================================
// HOSTS
// ============================================================================

/// Default host for API servers (localhost only for security)
pub const DEFAULT_API_HOST: &str = "127.0.0.1";

/// Default host as IPv4 address (for direct socket connections)
pub const LOCALHOST_IPV4: std::net::Ipv4Addr = std::net::Ipv4Addr::new(127, 0, 0, 1);

// ============================================================================
// ENVIRONMENT VARIABLES
// ============================================================================

/// Auth token for daemon communication
pub const ENV_DETRIX_TOKEN: &str = "DETRIX_TOKEN";

/// Override gRPC port (highest priority, typically for testing)
pub const ENV_DETRIX_GRPC_PORT_OVERRIDE: &str = "DETRIX_GRPC_PORT_OVERRIDE";

/// gRPC port override
pub const ENV_DETRIX_GRPC_PORT: &str = "DETRIX_GRPC_PORT";

/// HTTP/REST port override
pub const ENV_DETRIX_HTTP_PORT: &str = "DETRIX_HTTP_PORT";

/// Override MCP disconnect timeout (milliseconds)
pub const ENV_DETRIX_MCP_DISCONNECT_TIMEOUT_MS: &str = "DETRIX_MCP_DISCONNECT_TIMEOUT_MS";

/// Config file path override
pub const ENV_DETRIX_CONFIG: &str = "DETRIX_CONFIG";

/// Detrix home directory override
pub const ENV_DETRIX_HOME: &str = "DETRIX_HOME";

/// E2E tests: explicit detrix binary path
pub const ENV_DETRIX_BIN: &str = "DETRIX_BIN";

/// E2E tests: stream reporter output live when set
pub const ENV_DETRIX_E2E_STREAM: &str = "DETRIX_E2E_STREAM";

// ============================================================================
// PORTS
// ============================================================================

/// Default port for DAP adapter connections (debugpy, delve, lldb-dap)
pub const DEFAULT_ADAPTER_PORT: u16 = 5678;

/// Default port for gRPC API
pub const DEFAULT_GRPC_PORT: u16 = 50061;

/// Default port for REST/WebSocket API
pub const DEFAULT_REST_PORT: u16 = 8090;

/// Default port for GELF output (Graylog)
pub const DEFAULT_GELF_PORT: u16 = 12201;

// ============================================================================
// API CONFIG
// ============================================================================

/// Default for port fallback (auto-select available port)
pub const DEFAULT_PORT_FALLBACK: bool = true;

/// Default for localhost exemption from rate limiting
pub const DEFAULT_LOCALHOST_EXEMPT: bool = true;

/// gRPC stream maximum duration (0 = unlimited)
pub const DEFAULT_GRPC_STREAM_MAX_DURATION_MS: u64 = 0;

// ============================================================================
// TIMEOUTS (milliseconds)
// ============================================================================

/// Connection establishment timeout
pub const DEFAULT_CONNECTION_TIMEOUT_MS: u64 = 30_000;

/// Health check timeout
pub const DEFAULT_HEALTH_CHECK_TIMEOUT_MS: u64 = 5_000;

/// Port probe timeout for daemon discovery
pub const DEFAULT_PORT_PROBE_TIMEOUT_MS: u64 = 500;

/// Graceful shutdown timeout
pub const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;

/// Generic request timeout
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Retry interval between attempts
pub const DEFAULT_RETRY_INTERVAL_MS: u64 = 100;

/// SQLite busy timeout
pub const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;

/// Connection drain timeout before forceful close
pub const DEFAULT_DRAIN_TIMEOUT_MS: u64 = 5_000;

/// WebSocket idle timeout before closing connection
pub const DEFAULT_WS_IDLE_TIMEOUT_MS: u64 = 300_000;

/// WebSocket ping interval
pub const DEFAULT_WS_PING_INTERVAL_MS: u64 = 30_000;

/// WebSocket pong response timeout
pub const DEFAULT_WS_PONG_TIMEOUT_MS: u64 = 10_000;

/// GELF TCP connection timeout
pub const DEFAULT_GELF_TCP_CONNECT_TIMEOUT_MS: u64 = 5_000;

/// GELF TCP write timeout
pub const DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS: u64 = 10_000;

/// AST/safety analysis timeout
pub const DEFAULT_ANALYSIS_TIMEOUT_MS: u64 = 5_000;

/// Dead letter queue retry interval
pub const DEFAULT_DLQ_RETRY_INTERVAL_MS: u64 = 30_000;

/// Default timeout for output send operations (seconds)
pub const DEFAULT_OUTPUT_SEND_TIMEOUT_SECS: u64 = 10;

// ============================================================================
// BATCHING / BUFFERING
// ============================================================================

/// Number of events to trigger a batch flush
pub const DEFAULT_BATCH_THRESHOLD: usize = 5;

/// Concurrent batch processing limit
pub const DEFAULT_BATCH_CONCURRENCY: usize = 4;

/// Event channel buffer capacity
pub const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 1_000;

/// In-memory event buffer capacity
pub const DEFAULT_EVENT_BUFFER_CAPACITY: usize = 1_000;

/// Events per batch for storage operations
pub const DEFAULT_EVENT_BATCH_SIZE: usize = 100;

/// Flush interval for event batching (balance between latency and batch efficiency)
pub const DEFAULT_EVENT_FLUSH_INTERVAL_MS: u64 = 1_000;

/// GELF HTTP batch size
pub const DEFAULT_GELF_HTTP_BATCH_SIZE: usize = 100;

/// GELF HTTP request timeout (milliseconds)
pub const DEFAULT_GELF_HTTP_TIMEOUT_MS: u64 = 10_000;

/// GELF HTTP flush interval (milliseconds)
pub const DEFAULT_GELF_HTTP_FLUSH_INTERVAL_MS: u64 = 1_000;

/// Dead letter queue batch size
pub const DEFAULT_DLQ_BATCH_SIZE: usize = 50;

// ============================================================================
// LIMITS
// ============================================================================

/// Default query result limit
pub const DEFAULT_QUERY_LIMIT: i64 = 100;

/// Maximum MCP usage history entries per tool
pub const DEFAULT_MCP_USAGE_HISTORY: usize = 20;

/// Maximum query result limit
pub const DEFAULT_MAX_QUERY_LIMIT: i64 = 1_000;

/// Maximum events stored per metric
pub const DEFAULT_MAX_EVENTS_PER_METRIC: usize = 10_000;

/// Database connection pool size (reasonable for SQLite)
pub const DEFAULT_POOL_SIZE: u32 = 5;

/// Maximum WebSocket connections per IP address
pub const DEFAULT_MAX_WS_CONNECTIONS_PER_IP: usize = 10;

/// Maximum total metrics
pub const DEFAULT_MAX_METRICS_TOTAL: usize = 10_000;

/// Maximum metrics per group
pub const DEFAULT_MAX_METRICS_PER_GROUP: usize = 1_000;

/// Maximum expression length in characters
pub const DEFAULT_MAX_EXPRESSION_LENGTH: usize = 10_000;

/// Maximum system events to retain
pub const DEFAULT_SYSTEM_EVENT_MAX_EVENTS: usize = 100_000;

/// Maximum allowed path component length to prevent DoS
pub const MAX_PATH_COMPONENT_LENGTH: usize = 255;

/// Maximum allowed total path length
pub const MAX_PATH_LENGTH: usize = 4096;

/// Maximum GELF UDP message size (after compression)
pub const MAX_GELF_UDP_MESSAGE_SIZE: usize = 8192;

/// GELF chunk magic bytes (identifies chunked messages)
pub const GELF_CHUNK_MAGIC: [u8; 2] = [0x1e, 0x0f];

/// GELF chunk header size: magic (2) + message ID (8) + seq num (1) + seq count (1)
pub const GELF_CHUNK_HEADER_SIZE: usize = 12;

/// Maximum GELF chunk data size (packet size minus header)
pub const GELF_CHUNK_DATA_SIZE: usize = MAX_GELF_UDP_MESSAGE_SIZE - GELF_CHUNK_HEADER_SIZE;

/// Maximum number of chunks per GELF message
pub const GELF_MAX_CHUNKS: usize = 128;

// ============================================================================
// RATE LIMITING
// ============================================================================

/// Requests per second limit
pub const DEFAULT_RATE_LIMIT_PER_SECOND: u64 = 100;

/// Rate limit burst allowance
pub const DEFAULT_RATE_LIMIT_BURST_SIZE: u32 = 50;

/// Maximum reasonable burst size for rate limit validation
pub const MAX_RATE_LIMIT_BURST: u32 = 10_000;

// ============================================================================
// ANCHOR / CODE TRACKING
// ============================================================================

/// Default number of context lines for anchor fingerprinting (before/after target line)
pub const DEFAULT_ANCHOR_CONTEXT_LINES: usize = 2;

/// Default maximum bytes per anchor context field (before, line, after)
pub const DEFAULT_ANCHOR_MAX_CONTEXT_BYTES: usize = 200;

/// Default minimum confidence for anchor fingerprint fuzzy matching (0.0-1.0)
pub const DEFAULT_ANCHOR_MIN_CONFIDENCE: f32 = 0.8;

/// Default debounce time for file change events (milliseconds)
pub const DEFAULT_FILE_CHANGE_DEBOUNCE_MS: u64 = 500;

// ============================================================================
// DAP / DEBUGGER
// ============================================================================

/// Line tolerance for DAP breakpoint matching (allows ±N lines)
pub const DAP_LINE_TOLERANCE: u32 = 5;

/// Delay before sending configurationDone to lldb-dap (milliseconds)
/// This gives lldb-dap time to process the launch request before configuration completes
pub const DEFAULT_LLDB_CONFIG_DONE_DELAY_MS: u64 = 500;

// ============================================================================
// DEBUG LOGGING
// ============================================================================

/// Maximum characters to show in debug log message previews
pub const DEFAULT_LOG_MESSAGE_PREVIEW_LEN: usize = 300;

/// Maximum characters to show in debug log for large payloads (launch requests, etc.)
pub const DEFAULT_LOG_PAYLOAD_PREVIEW_LEN: usize = 500;

// ============================================================================
// TCP KEEPALIVE (prevents Windows from closing idle connections)
// ============================================================================

/// TCP keepalive time - when to start sending keepalive probes (seconds)
/// This prevents Windows from closing idle TCP connections (typically 10-30 seconds)
pub const DEFAULT_TCP_KEEPALIVE_TIME_SECS: u64 = 10;

/// TCP keepalive interval - time between keepalive probes (seconds)
pub const DEFAULT_TCP_KEEPALIVE_INTERVAL_SECS: u64 = 10;

/// TCP keepalive retries - number of probes before giving up
/// (Windows/Linux/FreeBSD/Android only)
pub const DEFAULT_TCP_KEEPALIVE_RETRIES: u32 = 3;

/// Socket read timeout for DAP server connections (seconds)
/// Prevents blocking forever on Windows when client stops sending data.
/// Set high enough to not interrupt normal DAP communication (idle periods expected).
pub const DEFAULT_DAP_SOCKET_READ_TIMEOUT_SECS: u64 = 120;

// ============================================================================
// SQLITE
// ============================================================================

/// SQLite maximum variable number limit (SQLITE_MAX_VARIABLE_NUMBER default)
pub const SQLITE_MAX_VARIABLES: usize = 999;

// ============================================================================
// RETRY / RECONNECTION
// ============================================================================

/// Maximum reconnection attempts
pub const DEFAULT_MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// Maximum "connection refused" attempts before fast-fail
/// When nothing is listening on the port, fail quickly instead of waiting for full timeout
pub const DEFAULT_MAX_CONNECTION_REFUSED_ATTEMPTS: u32 = 3;

/// Initial delay before first reconnection attempt
pub const DEFAULT_INITIAL_RECONNECT_DELAY_MS: u64 = 1_000;

/// Maximum delay between reconnection attempts
pub const DEFAULT_MAX_RECONNECT_DELAY_MS: u64 = 30_000;

/// Backoff multiplier for exponential backoff
pub const DEFAULT_BACKOFF_MULTIPLIER: f64 = 2.0;

/// Jitter ratio for randomized backoff
pub const DEFAULT_JITTER_RATIO: f64 = 0.1;

/// Dead letter queue maximum retries per message
pub const DEFAULT_DLQ_MAX_RETRIES: u32 = 3;

/// Dead letter queue storage pool size (smaller than main storage)
pub const DEFAULT_DLQ_POOL_SIZE: u32 = 5;

/// Dead letter queue busy timeout in milliseconds
pub const DEFAULT_DLQ_BUSY_TIMEOUT_MS: u64 = 3_000;

// ============================================================================
// RETENTION
// ============================================================================

/// Default event retention in hours
pub const DEFAULT_RETENTION_HOURS: u32 = 24;

/// System event retention in hours (1 week)
pub const DEFAULT_SYSTEM_EVENT_RETENTION_HOURS: u32 = 168;

/// Audit event retention in days (90 days)
pub const DEFAULT_AUDIT_RETENTION_DAYS: u32 = 90;

/// Log file retention in days (7 days)
/// Log files older than this will be deleted at daemon startup
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 7;

/// System event cleanup interval in seconds
pub const DEFAULT_SYSTEM_EVENT_CLEANUP_INTERVAL_SECS: u64 = 3_600;

/// Auto-sleep timeout in seconds (1 hour)
pub const DEFAULT_AUTO_SLEEP_SECONDS: u64 = 3_600;

/// JWKS cache TTL in seconds (5 minutes)
pub const DEFAULT_JWKS_CACHE_TTL_SECONDS: u64 = 300;

// ============================================================================
// SAFETY
// ============================================================================

/// Maximum recursion depth for expression evaluation
pub const DEFAULT_MAX_RECURSION_DEPTH: u32 = 50;

/// Maximum memory usage in MB
pub const DEFAULT_MAX_MEMORY_MB: u32 = 100;

/// Maximum call depth for function calls
pub const DEFAULT_MAX_CALL_DEPTH: usize = 10;

/// Maximum evaluation time in milliseconds
pub const DEFAULT_MAX_EVAL_TIME_MS: u32 = 1_000;

/// Maximum entries in the AST parse cache for tree-sitter analysis
pub const DEFAULT_AST_CACHE_MAX_ENTRIES: usize = 10_000;

// ============================================================================
// FILE INSPECTION
// ============================================================================

/// Default number of lines for code context
pub const DEFAULT_CONTEXT_LINES: usize = 10;

/// Default number of preview lines for file overview
pub const DEFAULT_PREVIEW_LINES: usize = 20;

// ============================================================================
// CIRCUIT BREAKER
// ============================================================================

/// Default failure threshold to open the circuit breaker
pub const DEFAULT_CIRCUIT_BREAKER_FAILURE_THRESHOLD: u32 = 5;

/// Default timeout before trying to close the circuit (milliseconds)
pub const DEFAULT_CIRCUIT_BREAKER_RESET_TIMEOUT_MS: u64 = 30_000;

/// Default success threshold to close the circuit
pub const DEFAULT_CIRCUIT_BREAKER_SUCCESS_THRESHOLD: u32 = 2;

// ============================================================================
// GELF / OUTPUT
// ============================================================================

/// Default source hostname for GELF messages
pub const DEFAULT_GELF_SOURCE_HOST: &str = "detrix";

/// Default GELF transport protocol
pub const DEFAULT_GELF_TRANSPORT: &str = "tcp";

/// Default GELF HTTP path
pub const DEFAULT_GELF_HTTP_PATH: &str = "/gelf";

// ============================================================================
// CONFIG MODE DEFAULTS (string enum values)
// ============================================================================

/// Default whitelist mode for safety configuration
pub const DEFAULT_WHITELIST_MODE: &str = "strict";

/// Default on_error behavior for sampling configuration
pub const DEFAULT_ON_ERROR: &str = "log";

/// Default runtime mode for the daemon
pub const DEFAULT_RUNTIME_MODE: &str = "sleep";

/// Default storage type (sqlite)
pub const DEFAULT_STORAGE_TYPE: &str = "sqlite";

// ============================================================================
// SAMPLING
// ============================================================================

/// Default sample rate (samples per metric)
pub const DEFAULT_SAMPLE_RATE: u32 = 10;

/// Default sample interval in seconds
pub const DEFAULT_SAMPLE_INTERVAL_SECONDS: u32 = 30;

/// Default maximum samples per second
pub const DEFAULT_MAX_PER_SECOND: u32 = 100;

// ============================================================================
// TUI
// ============================================================================

/// TUI event buffer size
pub const DEFAULT_TUI_EVENT_BUFFER_SIZE: usize = 10_000;

/// TUI refresh rate when active (milliseconds)
pub const DEFAULT_TUI_REFRESH_RATE_ACTIVE_MS: u64 = 16;

/// TUI refresh rate when idle (milliseconds)
pub const DEFAULT_TUI_REFRESH_RATE_IDLE_MS: u64 = 250;

/// TUI idle detection timeout (milliseconds)
pub const DEFAULT_TUI_IDLE_TIMEOUT_MS: u64 = 500;

/// TUI data refresh interval when connected (milliseconds)
pub const DEFAULT_TUI_DATA_REFRESH_CONNECTED_MS: u64 = 2000;

/// TUI data refresh interval when disconnected (milliseconds)
pub const DEFAULT_TUI_DATA_REFRESH_DISCONNECTED_MS: u64 = 5000;

/// TUI gRPC timeout (milliseconds)
pub const DEFAULT_TUI_GRPC_TIMEOUT_MS: u64 = 500;

// ============================================================================
// COMPRESSION
// ============================================================================

/// Default gzip compression level (0-9)
pub const DEFAULT_COMPRESSION_LEVEL: u32 = 6;

// ============================================================================
// CHANNEL CAPACITIES
// ============================================================================

/// Default capacity for system event broadcast channels
pub const DEFAULT_SYSTEM_EVENT_CHANNEL_CAPACITY: usize = 1_000;

/// Default channel buffer size for file watcher events
pub const DEFAULT_FILE_WATCHER_CHANNEL_SIZE: usize = 256;

// ============================================================================
// INTERVALS (milliseconds)
// ============================================================================

/// Health check interval
pub const DEFAULT_HEALTH_CHECK_INTERVAL_MS: u64 = 5_000;

/// Restart delay after reconnection
pub const DEFAULT_RESTART_DELAY_MS: u64 = 500;

/// Poll interval for daemon startup/shutdown checks
pub const DEFAULT_DAEMON_POLL_INTERVAL_MS: u64 = 100;

/// Idle check interval for WebSocket connections
pub const DEFAULT_WS_IDLE_CHECK_INTERVAL_MS: u64 = 10_000;

// ============================================================================
// MCP (Model Context Protocol)
// ============================================================================

/// MCP client heartbeat timeout (seconds)
/// If no heartbeat received within this duration, client is considered dead
pub const DEFAULT_MCP_HEARTBEAT_TIMEOUT_SECS: u64 = 30;

/// MCP daemon spawn timeout (seconds)
/// Maximum time to wait for daemon to become healthy after spawning
pub const DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS: u64 = 30;

/// MCP daemon health poll interval (milliseconds)
/// How often to poll daemon health during startup
pub const DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS: u64 = 100;

/// MCP bridge HTTP timeout (milliseconds)
/// Must be longer than adapter connection timeout (30s) to allow proper error propagation
pub const DEFAULT_MCP_BRIDGE_TIMEOUT_MS: u64 = 60_000;

/// MCP bridge heartbeat interval (seconds)
/// How often the bridge sends heartbeats to keep the client registered
/// Should be less than MCP_HEARTBEAT_TIMEOUT_SECS (30s)
pub const DEFAULT_MCP_BRIDGE_HEARTBEAT_INTERVAL_SECS: u64 = 10;

// Compile-time assertion: heartbeat interval must be shorter than heartbeat timeout
const _: () = assert!(
    DEFAULT_MCP_BRIDGE_HEARTBEAT_INTERVAL_SECS < DEFAULT_MCP_HEARTBEAT_TIMEOUT_SECS,
    "MCP heartbeat interval must be shorter than heartbeat timeout"
);

/// MCP bridge disconnect timeout (milliseconds)
/// Timeout for sending disconnect notification to daemon before exit
/// Increased from 500ms to allow for network latency on slow connections
pub const DEFAULT_MCP_DISCONNECT_TIMEOUT_MS: u64 = 3_000;

/// MCP bridge max heartbeat failures before attempting daemon restart
/// After this many consecutive heartbeat failures, the bridge will proactively
/// try to restart the daemon to ensure it's available when needed
pub const DEFAULT_MCP_HEARTBEAT_MAX_FAILURES: u32 = 2;

/// Shutdown grace period (seconds)
/// Grace period before shutdown after all clients disconnect
pub const DEFAULT_SHUTDOWN_GRACE_PERIOD_SECS: u64 = 10;

/// MCP cleanup interval (seconds)
/// How often to check for and remove stale MCP clients
pub const DEFAULT_MCP_CLEANUP_INTERVAL_SECS: u64 = 5;

/// MCP tool operation timeout (milliseconds)
/// Timeout for individual tool operations (add metric, toggle, etc.)
/// Should be shorter than DEFAULT_MCP_BRIDGE_TIMEOUT_MS
pub const DEFAULT_MCP_TOOL_TIMEOUT_MS: u64 = 5_000;

// Compile-time assertion: tool timeout must be shorter than bridge timeout
const _: () = assert!(
    DEFAULT_MCP_TOOL_TIMEOUT_MS < DEFAULT_MCP_BRIDGE_TIMEOUT_MS,
    "MCP tool timeout must be shorter than bridge timeout"
);

/// MCP quick operation timeout (milliseconds)
/// Timeout for very quick operations like ping or simple status checks
pub const DEFAULT_MCP_QUICK_TIMEOUT_MS: u64 = 1_000;

// ============================================================================
// TUI CAPACITIES
// ============================================================================

/// Maximum system events to keep in TUI history
pub const DEFAULT_TUI_SYSTEM_EVENT_CAPACITY: usize = 1_000;

/// Maximum events to track for rate calculation in TUI
pub const DEFAULT_TUI_EVENT_TIMES_CAPACITY: usize = 100;

/// Maximum TUI log entries to keep
pub const DEFAULT_TUI_LOG_CAPACITY: usize = 100;

// ============================================================================
// DAP DISPLAY LIMITS
// ============================================================================

/// Maximum length for displayed variable values in DAP
pub const DEFAULT_DAP_VALUE_DISPLAY_LIMIT: usize = 100;

// ============================================================================
// STACK TRACE CAPTURE LIMITS
// ============================================================================

/// Default maximum stack frames to capture
pub const DEFAULT_MAX_STACK_FRAMES: u32 = 50;

/// Maximum stack frames for "full" stack trace capture
pub const FULL_STACK_TRACE_MAX_FRAMES: u32 = 1000;

/// Default number of frames for stack trace slices (when only partial trace needed)
pub const DEFAULT_STACK_SLICE_FRAMES: u32 = 10;

// ============================================================================
// MEMORY SNAPSHOT LIMITS
// ============================================================================

/// Maximum variables to capture in memory snapshots
pub const MAX_VARIABLE_CAPTURE_COUNT: u32 = 100;

// ============================================================================
// TUI LAYOUT CONSTANTS
// ============================================================================
// Column widths and display limits for TUI table layouts.
// These values ensure consistent rendering across different tabs.

// --- Metrics Tab ---
/// Column width for metric name in metrics table
pub const TUI_METRICS_NAME_WIDTH: u16 = 20;
/// Minimum column width for location in metrics table
pub const TUI_METRICS_LOCATION_MIN_WIDTH: u16 = 30;
/// Column width for mode in metrics table
pub const TUI_METRICS_MODE_WIDTH: u16 = 12;
/// Column width for enabled status in metrics table
pub const TUI_METRICS_ENABLED_WIDTH: u16 = 8;
/// Column width for connection ID in metrics table
pub const TUI_METRICS_CONNECTION_WIDTH: u16 = 15;
/// Maximum path length for truncation in metrics tab
pub const TUI_METRICS_PATH_TRUNCATE_LEN: usize = 25;

// --- Events Tab ---
/// Percentage split for events list (left panel)
pub const TUI_EVENTS_LIST_PERCENT: u16 = 60;
/// Percentage split for events details (right panel)
pub const TUI_EVENTS_DETAILS_PERCENT: u16 = 40;
/// Column width for pin marker in events table
pub const TUI_EVENTS_PIN_WIDTH: u16 = 2;
/// Column width for timestamp in events table
pub const TUI_EVENTS_TIME_WIDTH: u16 = 12;
/// Column width for metric name in events table
pub const TUI_EVENTS_METRIC_WIDTH: u16 = 20;
/// Minimum column width for value in events table
pub const TUI_EVENTS_VALUE_MIN_WIDTH: u16 = 15;
/// Column width for connection ID in events table
pub const TUI_EVENTS_CONNECTION_WIDTH: u16 = 15;
/// Maximum value length for truncation in events tab
pub const TUI_EVENTS_VALUE_TRUNCATE_LEN: usize = 30;

// --- Connections Tab ---
/// Column width for connection ID
pub const TUI_CONN_ID_WIDTH: u16 = 18;
/// Column width for address (host:port)
pub const TUI_CONN_ADDRESS_WIDTH: u16 = 22;
/// Column width for language/adapter
pub const TUI_CONN_LANGUAGE_WIDTH: u16 = 12;
/// Column width for connection status
pub const TUI_CONN_STATUS_WIDTH: u16 = 14;
/// Column width for metrics count
pub const TUI_CONN_METRICS_WIDTH: u16 = 8;
/// Minimum column width for last active timestamp
pub const TUI_CONN_LAST_ACTIVE_MIN_WIDTH: u16 = 12;
/// Maximum error message length in status display
pub const TUI_CONN_STATUS_ERROR_TRUNCATE_LEN: usize = 10;

// --- System Tab ---
/// Height for daemon info panel (with MCP clients)
pub const TUI_SYSTEM_DAEMON_HEIGHT_WITH_MCP: u16 = 6;
/// Height for daemon info panel (without MCP clients)
pub const TUI_SYSTEM_DAEMON_HEIGHT_NO_MCP: u16 = 3;
/// Percentage split for system events panel
pub const TUI_SYSTEM_EVENTS_PERCENT: u16 = 40;
/// Percentage split for TUI logs panel
pub const TUI_SYSTEM_LOGS_PERCENT: u16 = 40;

// MCP clients table widths
/// Column width for parent process in MCP clients table
pub const TUI_MCP_PARENT_WIDTH: u16 = 24;
/// Column width for duration in MCP clients table
pub const TUI_MCP_DURATION_WIDTH: u16 = 12;
/// Column width for activity in MCP clients table
pub const TUI_MCP_ACTIVITY_WIDTH: u16 = 10;
/// Column width for client ID in MCP clients table
pub const TUI_MCP_CLIENT_ID_WIDTH: u16 = 18;
/// Maximum client ID length for truncation
pub const TUI_MCP_CLIENT_ID_TRUNCATE_LEN: usize = 16;

// System events table widths
/// Column width for timestamp in system events
pub const TUI_SYSEVENT_TIME_WIDTH: u16 = 12;
/// Column width for event type
pub const TUI_SYSEVENT_TYPE_WIDTH: u16 = 20;
/// Column width for connection ID in system events
pub const TUI_SYSEVENT_CONNECTION_WIDTH: u16 = 15;
/// Minimum column width for details
pub const TUI_SYSEVENT_DETAILS_MIN_WIDTH: u16 = 20;

// TUI logs table widths
/// Column width for age in TUI logs
pub const TUI_LOGS_AGE_WIDTH: u16 = 8;
/// Column width for log level
pub const TUI_LOGS_LEVEL_WIDTH: u16 = 6;
/// Minimum column width for log message
pub const TUI_LOGS_MESSAGE_MIN_WIDTH: u16 = 30;

// --- Common Truncation Lengths ---
/// Default truncation length for connection IDs across tabs
pub const TUI_CONNECTION_ID_TRUNCATE_LEN: usize = 15;
/// Default truncation length for timestamps
pub const TUI_TIMESTAMP_TRUNCATE_LEN: usize = 19;
