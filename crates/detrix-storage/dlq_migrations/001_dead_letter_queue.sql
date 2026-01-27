-- Dead-Letter Queue for failed event flushes (PERF-01 audit finding)
-- Events that fail to persist are saved here for retry
-- All timestamps use INTEGER (microseconds since epoch)
--
-- This is a SEPARATE database from main storage to provide true isolation.
-- If the main database has issues, DLQ can still capture failed events.

-- ============================================================
-- DEAD LETTER EVENTS TABLE
-- ============================================================

CREATE TABLE IF NOT EXISTS dead_letter_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Event data as JSON (original metric event structure)
    event_json TEXT NOT NULL,
    -- Error that caused the flush failure
    error_message TEXT NOT NULL,
    -- Retry tracking
    retry_count INTEGER NOT NULL DEFAULT 0,
    -- Status: 'pending', 'retrying', 'failed' (permanently)
    status TEXT NOT NULL DEFAULT 'pending',
    -- Timestamps (microseconds since epoch)
    created_at INTEGER NOT NULL,
    last_retry_at INTEGER,

    CHECK(status IN ('pending', 'retrying', 'failed'))
);

-- Index for finding pending events to retry
CREATE INDEX IF NOT EXISTS idx_dle_status ON dead_letter_events(status);
CREATE INDEX IF NOT EXISTS idx_dle_status_created ON dead_letter_events(status, created_at);
-- Index for cleanup of old failed events
CREATE INDEX IF NOT EXISTS idx_dle_created ON dead_letter_events(created_at);
-- Index for retry scheduling
CREATE INDEX IF NOT EXISTS idx_dle_pending_retry ON dead_letter_events(status, last_retry_at)
    WHERE status = 'pending';
