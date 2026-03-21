-- Add index on agent_id for efficient filtering by MCP bridge session cleanup
CREATE INDEX IF NOT EXISTS idx_metrics_agent_id ON metrics(agent_id);
