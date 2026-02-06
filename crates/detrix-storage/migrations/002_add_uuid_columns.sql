-- Migration: Add identity columns for UUID-based connection identity
--
-- This migration adds workspace and hostname fields to enable deterministic UUID generation.
-- Connection ID (primary key) will be the UUID generated from: SHA256(name|language|workspace_root|hostname)[0..16]
--
-- Key changes:
-- 1. Add `workspace_root` column for workspace isolation
-- 2. Add `hostname` column for multi-host support
-- 3. Create unique constraint on identity components

-- =============================================================================
-- Step 1: Add new columns (nullable for migration compatibility)
-- =============================================================================

ALTER TABLE connections ADD COLUMN workspace_root TEXT;
ALTER TABLE connections ADD COLUMN hostname TEXT;

-- =============================================================================
-- Step 2: Populate workspace_root and hostname with defaults
-- =============================================================================

-- Default workspace_root for existing connections
UPDATE connections
SET workspace_root = '/unknown'
WHERE workspace_root IS NULL;

-- Default hostname for existing connections
UPDATE connections
SET hostname = 'unknown'
WHERE hostname IS NULL;

-- =============================================================================
-- Step 3: Create indexes for performance
-- =============================================================================

-- Unique constraint on identity components (prevents duplicate identities)
-- This ensures that (name, language, workspace_root, hostname) is unique
CREATE UNIQUE INDEX IF NOT EXISTS idx_connections_identity
    ON connections(name, language, workspace_root, hostname)
    WHERE name IS NOT NULL;

-- =============================================================================
-- Step 5: Future migration - Make columns NOT NULL (run after 6 months)
-- =============================================================================

-- NOTE: After all clients are updated to send identity fields (workspace_root/hostname),
-- run this migration in a separate file to enforce NOT NULL constraints:
--
-- -- migration 003_enforce_identity_not_null.sql
-- -- Step 1: Verify all rows have values
-- -- SELECT COUNT(*) FROM connections WHERE workspace_root IS NULL; -- Should be 0
-- --
-- -- Step 2: Create new table with NOT NULL constraints
-- -- CREATE TABLE connections_new (
-- --     id TEXT PRIMARY KEY,  -- connection_id (deterministic UUID from identity)
-- --     name TEXT DEFAULT NULL,
-- --     workspace_root TEXT NOT NULL,
-- --     hostname TEXT NOT NULL,
-- --     ... (rest of columns)
-- -- );
-- --
-- -- Step 3: Copy data
-- -- INSERT INTO connections_new SELECT * FROM connections;
-- --
-- -- Step 4: Drop old table and rename
-- -- DROP TABLE connections;
-- -- ALTER TABLE connections_new RENAME TO connections;
-- --
-- -- Step 5: Recreate indexes
-- -- (recreate all indexes from above)

-- =============================================================================
-- Verification queries (for testing)
-- =============================================================================

-- Check all connections have workspace_root:
-- SELECT COUNT(*) FROM connections WHERE workspace_root IS NULL; -- Should be 0

-- Check all connections have hostname:
-- SELECT COUNT(*) FROM connections WHERE hostname IS NULL; -- Should be 0

-- View sample connections with new fields:
-- SELECT id, name, workspace_root, hostname, language FROM connections LIMIT 5;
