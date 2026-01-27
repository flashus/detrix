//! Shared formatting utilities
//!
//! This module contains pure formatting functions that can be used across all crates.

use chrono::{DateTime, Local, TimeZone, Utc};
use std::sync::atomic::{AtomicBool, Ordering};

// =============================================================================
// Global UTC Setting
// =============================================================================

/// Global setting for UTC timestamps (set from config at startup)
static USE_UTC: AtomicBool = AtomicBool::new(false);

/// Set whether to use UTC timestamps (called from config at startup)
///
/// # Example
/// ```
/// use detrix_core::formatting::set_use_utc;
/// set_use_utc(true); // Use UTC timestamps
/// ```
pub fn set_use_utc(use_utc: bool) {
    USE_UTC.store(use_utc, Ordering::SeqCst);
}

/// Get current UTC setting
pub fn is_use_utc() -> bool {
    USE_UTC.load(Ordering::SeqCst)
}

// =============================================================================
// Timestamp Formatting
// =============================================================================

/// Format timestamp from microseconds with custom format string
///
/// Uses local time by default, UTC if configured via `set_use_utc(true)`.
///
/// # Arguments
/// * `micros` - Microseconds since Unix epoch
/// * `format` - chrono format string (e.g., "%Y-%m-%d %H:%M:%S")
/// * `fallback` - Value to return if timestamp is invalid
///
/// # Example
/// ```
/// use detrix_core::formatting::format_timestamp_micros;
/// let ts = format_timestamp_micros(1704067200_000_000, "%Y-%m-%d %H:%M:%S", "N/A");
/// ```
pub fn format_timestamp_micros(micros: i64, format: &str, fallback: &str) -> String {
    let secs = micros / 1_000_000;
    let subsec_micros = (micros % 1_000_000).unsigned_abs() as u32;

    if let Some(dt) = Utc.timestamp_opt(secs, subsec_micros * 1000).single() {
        if USE_UTC.load(Ordering::SeqCst) {
            dt.format(format).to_string()
        } else {
            let local: DateTime<Local> = dt.into();
            local.format(format).to_string()
        }
    } else {
        fallback.to_string()
    }
}

/// Format timestamp with full date and time ("%Y-%m-%d %H:%M:%S")
///
/// Used by CLI table output.
///
/// # Example
/// ```
/// use detrix_core::formatting::format_timestamp_full;
/// let ts = format_timestamp_full(1704067200_000_000);
/// // Returns "2024-01-01 00:00:00" (in local or UTC based on setting)
/// ```
pub fn format_timestamp_full(micros: i64) -> String {
    format_timestamp_micros(micros, "%Y-%m-%d %H:%M:%S", "N/A")
}

/// Format timestamp with time and milliseconds ("%H:%M:%S%.3f")
///
/// Used by TUI for detailed time display.
///
/// # Example
/// ```
/// use detrix_core::formatting::format_timestamp_time;
/// let ts = format_timestamp_time(1704067200_123_000);
/// // Returns "00:00:00.123" (in local or UTC based on setting)
/// ```
pub fn format_timestamp_time(micros: i64) -> String {
    format_timestamp_micros(micros, "%H:%M:%S%.3f", "??:??:??.???")
}

/// Format timestamp with time only ("%H:%M:%S")
///
/// Used by TUI for compact time display.
///
/// # Example
/// ```
/// use detrix_core::formatting::format_timestamp_short;
/// let ts = format_timestamp_short(1704067200_000_000);
/// // Returns "00:00:00" (in local or UTC based on setting)
/// ```
pub fn format_timestamp_short(micros: i64) -> String {
    format_timestamp_micros(micros, "%H:%M:%S", "??:??:??")
}

// =============================================================================
// Time Constants
// =============================================================================

/// Number of seconds in a day (24 * 60 * 60)
pub const SECS_PER_DAY: u64 = 86400;
/// Number of seconds in an hour (60 * 60)
pub const SECS_PER_HOUR: u64 = 3600;
/// Number of seconds in a minute
pub const SECS_PER_MINUTE: u64 = 60;

/// Format duration/uptime as "Xd HH:MM:SS" or "HH:MM:SS"
///
/// # Arguments
/// * `seconds` - Duration in seconds
///
/// # Returns
/// Formatted string like "2d 05:30:15" or "05:30:15" if less than a day
///
/// # Example
/// ```
/// use detrix_core::format_uptime;
///
/// assert_eq!(format_uptime(3661), "01:01:01");
/// assert_eq!(format_uptime(90061), "1d 01:01:01");
/// ```
pub fn format_uptime(seconds: u64) -> String {
    let days = seconds / SECS_PER_DAY;
    let hours = (seconds % SECS_PER_DAY) / SECS_PER_HOUR;
    let minutes = (seconds % SECS_PER_HOUR) / SECS_PER_MINUTE;
    let secs = seconds % SECS_PER_MINUTE;

    if days > 0 {
        format!("{}d {:02}:{:02}:{:02}", days, hours, minutes, secs)
    } else {
        format!("{:02}:{:02}:{:02}", hours, minutes, secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // Timestamp formatting tests
    // ==========================================================================

    #[test]
    fn test_set_and_get_use_utc() {
        // Test default value
        set_use_utc(false);
        assert!(!is_use_utc());

        // Test setting to true
        set_use_utc(true);
        assert!(is_use_utc());

        // Reset for other tests
        set_use_utc(false);
    }

    #[test]
    fn test_format_timestamp_full_valid() {
        // Use UTC to get consistent results
        set_use_utc(true);

        // Jan 1, 2024 00:00:00 UTC = 1704067200 seconds
        let micros = 1704067200_000_000i64;
        let result = format_timestamp_full(micros);
        assert_eq!(result, "2024-01-01 00:00:00");

        set_use_utc(false);
    }

    #[test]
    fn test_format_timestamp_time_valid() {
        // Use UTC to get consistent results
        set_use_utc(true);

        // 12:30:45.123 UTC
        let micros = 1704110445_123_000i64;
        let result = format_timestamp_time(micros);
        assert_eq!(result, "12:00:45.123");

        set_use_utc(false);
    }

    #[test]
    fn test_format_timestamp_short_valid() {
        // Use UTC to get consistent results
        set_use_utc(true);

        // 12:30:45 UTC
        let micros = 1704110445_000_000i64;
        let result = format_timestamp_short(micros);
        assert_eq!(result, "12:00:45");

        set_use_utc(false);
    }

    #[test]
    fn test_format_timestamp_micros_fallback() {
        // Invalid timestamp (very large negative)
        let result = format_timestamp_micros(i64::MIN, "%Y-%m-%d", "INVALID");
        assert_eq!(result, "INVALID");
    }

    #[test]
    fn test_format_timestamp_custom_format() {
        set_use_utc(true);

        let micros = 1704067200_000_000i64;
        let result = format_timestamp_micros(micros, "%Y/%m/%d", "N/A");
        assert_eq!(result, "2024/01/01");

        set_use_utc(false);
    }

    // ==========================================================================
    // Uptime formatting tests
    // ==========================================================================

    #[test]
    fn test_format_uptime_seconds_only() {
        assert_eq!(format_uptime(45), "00:00:45");
    }

    #[test]
    fn test_format_uptime_minutes() {
        assert_eq!(format_uptime(125), "00:02:05");
    }

    #[test]
    fn test_format_uptime_hours() {
        assert_eq!(format_uptime(3661), "01:01:01");
    }

    #[test]
    fn test_format_uptime_days() {
        assert_eq!(format_uptime(90061), "1d 01:01:01");
    }

    #[test]
    fn test_format_uptime_multiple_days() {
        assert_eq!(format_uptime(259200), "3d 00:00:00");
    }
}
