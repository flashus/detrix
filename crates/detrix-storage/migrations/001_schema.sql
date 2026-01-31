-- Detrix Database Schema v1.0
-- All timestamps use INTEGER (microseconds since epoch)

-- ============================================================
-- CONNECTIONS TABLE
-- ============================================================
-- Must be created first since metrics references connection_id

CREATE TABLE IF NOT EXISTS connections (
    id TEXT PRIMARY KEY,                -- ConnectionId (e.g., "127_0_0_1_5678" or custom)
    host TEXT NOT NULL,                 -- Host address (e.g., "127.0.0.1")
    port INTEGER NOT NULL,              -- Port number (>= 1024)
    status TEXT NOT NULL,               -- "Disconnected", "Connecting", "Connected", "Failed(...)"
    created_at INTEGER NOT NULL,        -- Timestamp when connection was created (microseconds)
    last_active INTEGER NOT NULL,       -- Timestamp of last activity (microseconds)
    name TEXT DEFAULT NULL,             -- User-friendly alias
    language TEXT NOT NULL,                    -- Language/adapter type (required, no default)
    auto_reconnect INTEGER NOT NULL DEFAULT 1,  -- Whether to auto-reconnect
    last_connected_at INTEGER DEFAULT NULL,     -- Timestamp of last successful connection
    safe_mode INTEGER NOT NULL DEFAULT 0,       -- SafeMode: only allow logpoints (non-blocking)

    CHECK(port >= 1024),                -- Enforce port >= 1024 (not in reserved range)
    CHECK(host != '')                   -- Host cannot be empty
);

CREATE INDEX IF NOT EXISTS idx_connections_status ON connections(status);
CREATE INDEX IF NOT EXISTS idx_connections_status_lower ON connections(LOWER(status));
CREATE INDEX IF NOT EXISTS idx_connections_last_active ON connections(last_active);
CREATE INDEX IF NOT EXISTS idx_connections_host_port ON connections(host, port);
CREATE INDEX IF NOT EXISTS idx_connections_language ON connections(language);
CREATE INDEX IF NOT EXISTS idx_connections_auto_reconnect ON connections(auto_reconnect, status);
CREATE INDEX IF NOT EXISTS idx_connections_safe_mode ON connections(safe_mode);

-- ============================================================
-- METRICS REGISTRY
-- ============================================================

CREATE TABLE IF NOT EXISTS metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    connection_id TEXT NOT NULL,  -- Which connection this metric belongs to (required, no default)
    group_name TEXT,
    location TEXT NOT NULL,
    expression TEXT NOT NULL,
    expression_hash TEXT NOT NULL,
    language TEXT NOT NULL,                    -- Required, no default
    enabled BOOLEAN NOT NULL DEFAULT 1,
    mode_type TEXT NOT NULL DEFAULT 'stream',
    mode_config TEXT,
    condition_expr TEXT,
    safety_level TEXT NOT NULL DEFAULT 'strict',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    created_by TEXT,
    -- Runtime introspection fields
    capture_stack_trace BOOLEAN NOT NULL DEFAULT 0,
    stack_trace_ttl INTEGER,            -- TTL in seconds, NULL = continuous
    stack_trace_slice TEXT,             -- JSON: {full, head, tail}
    capture_memory_snapshot BOOLEAN NOT NULL DEFAULT 0,
    snapshot_scope TEXT,                -- 'local', 'global', 'both'
    snapshot_ttl INTEGER,               -- TTL in seconds, NULL = continuous

    -- Metric location anchors for tracking code changes
    -- Enables metrics to "follow" code like print statements do.
    -- Uses hybrid approach: symbol + offset (primary) with context fingerprint (fallback).
    anchor_symbol TEXT,                 -- Enclosing function/method name
    anchor_symbol_kind INTEGER,         -- LSP SymbolKind (12=Function, 6=Method, etc.)
    anchor_symbol_range TEXT,           -- "start_line:end_line"
    anchor_offset INTEGER,              -- Lines from symbol start to metric line
    anchor_fingerprint TEXT,            -- SHA256 of context_before + source_line + context_after
    anchor_context_before TEXT,         -- 2 lines before metric (~200 bytes max)
    anchor_source_line TEXT,            -- The metric line itself (~100 bytes)
    anchor_context_after TEXT,          -- 2 lines after metric (~200 bytes max)
    anchor_last_verified INTEGER,       -- Timestamp (microseconds) of last verification
    anchor_original_location TEXT,      -- Original @file#line for audit trail
    anchor_status TEXT NOT NULL DEFAULT 'unanchored'
    -- NOTE: Uniqueness is enforced by idx_metrics_location_connection unique index (location + connection_id)
    -- This allows same metric name at different locations, matching DAP's one-logpoint-per-line constraint
);

CREATE INDEX IF NOT EXISTS idx_metrics_name ON metrics(name);
CREATE INDEX IF NOT EXISTS idx_metrics_group ON metrics(group_name);
CREATE INDEX IF NOT EXISTS idx_metrics_location ON metrics(location);
CREATE INDEX IF NOT EXISTS idx_metrics_enabled ON metrics(enabled);
CREATE INDEX IF NOT EXISTS idx_metrics_language ON metrics(language);
CREATE INDEX IF NOT EXISTS idx_metrics_connection_id ON metrics(connection_id);
CREATE INDEX IF NOT EXISTS idx_metrics_connection_name ON metrics(connection_id, name);
-- Unique index on location + connection_id prevents multiple metrics at same file:line per connection
CREATE UNIQUE INDEX IF NOT EXISTS idx_metrics_location_connection ON metrics(location, connection_id);
-- Anchor indexes
CREATE INDEX IF NOT EXISTS idx_metrics_anchor_status ON metrics(anchor_status);
CREATE INDEX IF NOT EXISTS idx_metrics_anchor_fingerprint ON metrics(anchor_fingerprint)
    WHERE anchor_fingerprint IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_metrics_anchor_needs_attention ON metrics(anchor_status)
    WHERE anchor_status IN ('orphaned', 'relocated');

-- ============================================================
-- METRIC EVENTS (Main Event Log)
-- ============================================================

CREATE TABLE IF NOT EXISTS metric_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    metric_id INTEGER NOT NULL,
    metric_name TEXT NOT NULL DEFAULT '',
    connection_id TEXT NOT NULL,  -- Required, no default
    timestamp INTEGER NOT NULL,
    thread_name TEXT,
    thread_id INTEGER,

    value_json TEXT NOT NULL,
    value_numeric REAL,
    value_string TEXT,
    value_boolean INTEGER,

    is_error BOOLEAN NOT NULL DEFAULT 0,
    error_type TEXT,
    error_message TEXT,
    error_traceback TEXT,

    request_id TEXT,
    session_id TEXT,

    -- Introspection data (JSON serialized)
    stack_trace_json TEXT,
    memory_snapshot_json TEXT,

    FOREIGN KEY (metric_id) REFERENCES metrics(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_events_metric_id ON metric_events(metric_id);
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON metric_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_events_thread_name ON metric_events(thread_name);
CREATE INDEX IF NOT EXISTS idx_events_is_error ON metric_events(is_error);
CREATE INDEX IF NOT EXISTS idx_events_metric_timestamp ON metric_events(metric_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_events_metric_timestamp_desc ON metric_events(metric_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_events_request_id ON metric_events(request_id);
CREATE INDEX IF NOT EXISTS idx_events_connection_id ON metric_events(connection_id);
CREATE INDEX IF NOT EXISTS idx_events_connection_metric ON metric_events(connection_id, metric_id);

-- Partial index for error queries
CREATE INDEX IF NOT EXISTS idx_events_error ON metric_events(is_error, timestamp DESC)
    WHERE is_error = 1;

-- ============================================================
-- PURITY CACHE
-- ============================================================
-- Stores purity analysis results to avoid repeated LSP calls

CREATE TABLE IF NOT EXISTS purity_cache (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Cache key components
    function_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    -- Analysis result
    purity_level TEXT NOT NULL CHECK (purity_level IN ('pure', 'unknown', 'impure')),
    impure_calls_json TEXT NOT NULL DEFAULT '[]',
    unknown_calls_json TEXT NOT NULL DEFAULT '[]',
    max_depth_reached INTEGER NOT NULL DEFAULT 0,
    -- Metadata
    max_depth_used INTEGER NOT NULL,
    created_at INTEGER NOT NULL,

    UNIQUE(function_name, file_path, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_purity_cache_file_path ON purity_cache(file_path);
CREATE INDEX IF NOT EXISTS idx_purity_cache_lookup ON purity_cache(function_name, file_path, content_hash);

-- ============================================================
-- RUNTIME STATE
-- ============================================================

CREATE TABLE IF NOT EXISTS runtime_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Default state
INSERT OR IGNORE INTO runtime_state (key, value, updated_at) VALUES
    ('mode', 'sleep', 0),
    ('wake_count', '0', 0),
    ('total_events', '0', 0),
    ('server_id', '', 0);

-- ============================================================
-- SYSTEM EVENTS & AUDIT TRAIL
-- ============================================================
-- Stores system-level events (crashes, connections, metric CRUD) for:
-- 1. MCP client catch-up queries (reconnecting agents)
-- 2. Audit trail for critical events
-- 3. Debugging and observability
--
-- Event types:
-- - metric_added/removed/toggled/relocated/orphaned: Metric lifecycle events
-- - connection_created/closed/lost/restored: Connection events
-- - debugger_crash: DAP adapter crash
-- - config_updated/config_validation_failed: Config audit events
-- - api_call_executed/api_call_failed: API audit events
-- - authentication_failed: Security audit events
-- - expression_validated: Expression validation audit

CREATE TABLE IF NOT EXISTS system_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type TEXT NOT NULL,
    connection_id TEXT,                    -- Associated connection (may be NULL for global events)
    metric_id INTEGER,                     -- Associated metric ID (for metric events)
    metric_name TEXT,                      -- Metric name (for metric events)
    timestamp INTEGER NOT NULL,            -- Microseconds since epoch
    details_json TEXT,                     -- Event-specific details (JSON)
    acknowledged INTEGER NOT NULL DEFAULT 0,  -- Whether client acknowledged receipt
    -- Audit trail columns
    actor TEXT,                            -- Who triggered the action (user ID, client ID, "system")
    resource_type TEXT,                    -- Type of resource affected (e.g., "config", "metric")
    resource_id TEXT,                      -- ID of the affected resource
    old_value TEXT,                        -- Previous state (JSON, for change tracking)
    new_value TEXT,                        -- New state (JSON, for change tracking)

    CHECK(event_type IN (
        -- System event types
        'metric_added', 'metric_removed', 'metric_toggled',
        'metric_relocated', 'metric_orphaned',
        'connection_created', 'connection_closed', 'connection_lost',
        'connection_restored', 'debugger_crash',
        -- Audit event types
        'config_updated', 'config_validation_failed',
        'api_call_executed', 'api_call_failed',
        'authentication_failed', 'expression_validated'
    ))
);

CREATE INDEX IF NOT EXISTS idx_system_events_timestamp ON system_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_system_events_timestamp_desc ON system_events(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_system_events_type ON system_events(event_type);
CREATE INDEX IF NOT EXISTS idx_system_events_connection ON system_events(connection_id);
CREATE INDEX IF NOT EXISTS idx_system_events_acknowledged ON system_events(acknowledged);
CREATE INDEX IF NOT EXISTS idx_system_events_unacknowledged ON system_events(acknowledged, timestamp)
    WHERE acknowledged = 0;
-- Audit trail indexes
CREATE INDEX IF NOT EXISTS idx_system_events_actor ON system_events(actor);
CREATE INDEX IF NOT EXISTS idx_system_events_resource ON system_events(resource_type, resource_id);
CREATE INDEX IF NOT EXISTS idx_system_events_type_timestamp ON system_events(event_type, timestamp);

-- ============================================================
-- MCP USAGE EVENTS
-- ============================================================
-- Stores tool invocations, errors, and workflow patterns to enable
-- data-driven improvements to MCP tool descriptions.

CREATE TABLE IF NOT EXISTS mcp_usage_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,           -- Microseconds since epoch
    tool_name TEXT NOT NULL,              -- Tool that was called
    status TEXT NOT NULL,                 -- 'success' or 'error'
    error_code TEXT,                      -- McpErrorCode as string (NULL on success)
    latency_ms INTEGER NOT NULL,          -- Call latency in milliseconds
    session_id TEXT,                      -- Session ID for correlation (optional)
    prior_tools TEXT,                     -- JSON array of prior tool names

    CONSTRAINT valid_status CHECK (status IN ('success', 'error'))
);

-- Index for time-range queries (most common query pattern)
CREATE INDEX IF NOT EXISTS idx_mcp_usage_timestamp ON mcp_usage_events(timestamp);
CREATE INDEX IF NOT EXISTS idx_mcp_usage_timestamp_desc ON mcp_usage_events(timestamp DESC);

-- Index for tool-specific analysis
CREATE INDEX IF NOT EXISTS idx_mcp_usage_tool ON mcp_usage_events(tool_name);
CREATE INDEX IF NOT EXISTS idx_mcp_usage_tool_status ON mcp_usage_events(tool_name, status);

-- Index for error analysis
CREATE INDEX IF NOT EXISTS idx_mcp_usage_error ON mcp_usage_events(error_code)
    WHERE error_code IS NOT NULL;

-- Index for error rate queries
CREATE INDEX IF NOT EXISTS idx_mcp_usage_status ON mcp_usage_events(status);
