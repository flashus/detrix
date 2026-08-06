-- Relax the connections.port CHECK to allow port 0 for agent-managed (eBPF)
-- connections, which have no network port. Port 0 is a sentinel meaning
-- "not applicable". Non-agent connections are still validated in the domain
-- layer (Connection::new_with_identity) and keep the port >= 1024 guarantee.

-- SQLite cannot alter a CHECK constraint, so recreate the table with a
-- relaxed constraint. Defer FK checks to commit time (sqlx runs migrations
-- inside a transaction, where PRAGMA foreign_keys is a no-op).

CREATE TABLE connections_new (
    id TEXT PRIMARY KEY,                -- ConnectionId (UUID from identity hash)
    host TEXT NOT NULL,                 -- Host address (e.g., "127.0.0.1")
    port INTEGER NOT NULL,              -- Port number (0 = agent-managed, else >= 1024)
    status TEXT NOT NULL,               -- "Disconnected", "Connecting", "Connected", "Failed(...)"
    created_at INTEGER NOT NULL,        -- Timestamp when connection was created (microseconds)
    last_active INTEGER NOT NULL,       -- Timestamp of last activity (microseconds)
    name TEXT DEFAULT NULL,             -- User-friendly alias
    language TEXT NOT NULL,             -- Language/adapter type (required, no default)
    auto_reconnect INTEGER NOT NULL DEFAULT 1,  -- Whether to auto-reconnect
    safe_mode INTEGER NOT NULL DEFAULT 0,       -- SafeMode: only allow logpoints (non-blocking)
    last_connected_at INTEGER DEFAULT NULL,     -- Timestamp of last successful connection
    workspace_root TEXT NOT NULL DEFAULT '',     -- Workspace directory (absolute path)
    hostname TEXT NOT NULL DEFAULT '',           -- Machine hostname for multi-host isolation
    control_plane_url TEXT DEFAULT NULL,         -- App control plane URL for file fetching (e.g., "http://app:8091")
    build_commit TEXT DEFAULT NULL,              -- Git commit SHA at build time (for verification + future git source)
    build_tag TEXT DEFAULT NULL,                 -- Build version tag (e.g., "v1.2.3")
    user_id TEXT DEFAULT NULL,                -- Client identity of the creator (from X-Detrix-Client-Id header)

    CHECK(port = 0 OR port >= 1024),    -- 0 = agent-managed (eBPF), otherwise avoid reserved range
    CHECK(host != '')                   -- Host cannot be empty
);

INSERT INTO connections_new (
    id, host, port, status, created_at, last_active, name, language,
    auto_reconnect, safe_mode, last_connected_at, workspace_root, hostname,
    control_plane_url, build_commit, build_tag, user_id
)
SELECT
    id, host, port, status, created_at, last_active, name, language,
    auto_reconnect, safe_mode, last_connected_at, workspace_root, hostname,
    control_plane_url, build_commit, build_tag, user_id
FROM connections;

DROP TABLE connections;
ALTER TABLE connections_new RENAME TO connections;

CREATE INDEX IF NOT EXISTS idx_connections_status ON connections(status);
CREATE INDEX IF NOT EXISTS idx_connections_status_lower ON connections(LOWER(status));
CREATE INDEX IF NOT EXISTS idx_connections_last_active ON connections(last_active);
CREATE INDEX IF NOT EXISTS idx_connections_host_port ON connections(host, port);
CREATE INDEX IF NOT EXISTS idx_connections_language ON connections(language);
CREATE INDEX IF NOT EXISTS idx_connections_auto_reconnect ON connections(auto_reconnect, status);
CREATE INDEX IF NOT EXISTS idx_connections_safe_mode ON connections(safe_mode);
CREATE UNIQUE INDEX IF NOT EXISTS idx_connections_identity
    ON connections(name, language, workspace_root, hostname)
    WHERE name IS NOT NULL;