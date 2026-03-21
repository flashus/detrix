-- Add index on user_id for efficient multi-tenant metric lookups
CREATE INDEX IF NOT EXISTS idx_metrics_user_id ON metrics(user_id);
