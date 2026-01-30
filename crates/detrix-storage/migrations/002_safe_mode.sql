-- Add safe_mode column to connections table
-- SafeMode: Only allow logpoints (non-blocking), disable breakpoint-based operations.
-- When enabled, blocks: function call expressions, stack trace capture, memory snapshots.

ALTER TABLE connections ADD COLUMN safe_mode INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_connections_safe_mode ON connections(safe_mode);
