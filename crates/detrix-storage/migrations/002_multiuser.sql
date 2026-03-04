-- Multi-tenant support: per-user metric ownership + agent identity
--
-- Renames created_by → user_id and adds agent_id column.
-- Existing metrics retain their created_by value as user_id.
-- Metrics with NULL user_id are accessible only to Admin scope.

-- Rename created_by → user_id (SQLite >= 3.25.0)
ALTER TABLE metrics RENAME COLUMN created_by TO user_id;

-- Add agent_id for per-agent identity tracking
ALTER TABLE metrics ADD COLUMN agent_id TEXT;
