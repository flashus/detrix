-- Multi-tenant support: per-user metric ownership + agent identity
--
-- Renames created_by → user_id and adds agent_id column.
-- Existing metrics retain their created_by value as user_id.
-- Metrics with NULL user_id are accessible only to Admin scope.

-- Rename created_by → user_id (SQLite >= 3.25.0)
ALTER TABLE metrics RENAME COLUMN created_by TO user_id;

-- Add agent_id for per-agent identity tracking
ALTER TABLE metrics ADD COLUMN agent_id TEXT;

-- Update unique index to include user_id so that different users can have
-- metrics at the same (location, connection_id) — each user owns their own metric.
-- SQLite treats each NULL as distinct in UNIQUE indexes, so metrics with NULL
-- user_id (pre-migration rows) each remain uniquely addressable.
DROP INDEX IF EXISTS idx_metrics_location_connection;
CREATE UNIQUE INDEX IF NOT EXISTS idx_metrics_location_connection
    ON metrics(location, connection_id, user_id);
