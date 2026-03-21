-- Rename connections.created_by → connections.user_id for consistency with metrics.user_id
--
-- The Connection.created_by field was originally named after the client identity
-- (X-Detrix-Client-Id header), but the field semantics match user_id better
-- (it stores the authenticated user ID in multi-tenant deployments).
-- SQLite >= 3.25.0 (available in all supported environments) supports RENAME COLUMN.

ALTER TABLE connections RENAME COLUMN created_by TO user_id;
