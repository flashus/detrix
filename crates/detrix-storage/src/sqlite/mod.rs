//! SQLite storage implementation
//!
//! This module provides SQLite-based implementations of all repository traits.
//!
//! # Module Structure
//! - `mod.rs` - SqliteStorage, config, and shared helpers
//! - `metric_repo.rs` - MetricRepository implementation
//! - `event_repo.rs` - EventRepository implementation
//! - `connection_repo.rs` - ConnectionRepository implementation
//! - `system_event_repo.rs` - SystemEventRepository implementation

mod connection_repo;
mod event_repo;
mod mcp_usage_repo;
mod metric_repo;
mod system_event_repo;

#[cfg(test)]
mod mcp_usage_repo_tests;
#[cfg(test)]
mod metric_repo_tests;

// Note: DLQ is in a dedicated DlqStorage (src/dlq_storage.rs)
// for database isolation from main storage failures.

use detrix_config::constants::{DEFAULT_BUSY_TIMEOUT_MS, DEFAULT_POOL_SIZE};
use detrix_core::entities::{
    MetricMode, MODE_FIRST, MODE_SAMPLE, MODE_SAMPLE_INTERVAL, MODE_STREAM, MODE_THROTTLE,
};
use detrix_core::error::{Error, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing::{debug, info};

// Note: Sub-modules only contain trait implementations on SqliteStorage.
// No re-exports needed - just importing the modules causes the impls to be registered.

/// SQLite configuration
#[derive(Debug, Clone)]
pub struct SqliteConfig {
    pub path: PathBuf,
    pub pool_size: u32,
    pub busy_timeout_ms: u64,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        SqliteConfig {
            path: detrix_config::paths::default_database_path(),
            pool_size: DEFAULT_POOL_SIZE,
            busy_timeout_ms: DEFAULT_BUSY_TIMEOUT_MS,
        }
    }
}

impl From<&detrix_config::StorageConfig> for SqliteConfig {
    fn from(config: &detrix_config::StorageConfig) -> Self {
        SqliteConfig {
            path: config.path.clone(),
            pool_size: config.pool_size,
            busy_timeout_ms: config.busy_timeout_ms,
        }
    }
}

/// SQLite storage with connection pool
#[derive(Debug, Clone)]
pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    /// Create new SQLite storage from config
    pub async fn new(config: &SqliteConfig) -> Result<Self> {
        let pool = create_pool(config).await?;
        run_migrations(&pool).await?;

        info!(path = %config.path.display(), "SQLite storage initialized");

        Ok(SqliteStorage { pool })
    }

    /// Create in-memory storage (for testing)
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str(":memory:")?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        run_migrations(&pool).await?;

        debug!("In-memory SQLite storage created");

        Ok(SqliteStorage { pool })
    }

    /// Get reference to connection pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Create connection pool
async fn create_pool(config: &SqliteConfig) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(&config.path.to_string_lossy())?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_millis(config.busy_timeout_ms));

    let pool = SqlitePoolOptions::new()
        .max_connections(config.pool_size)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Run database migrations using sqlx's built-in migration system
async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;

    debug!("Database migrations completed");
    Ok(())
}

// =============================================================================
// Shared Helper Functions
// =============================================================================

/// Helper to get current timestamp in microseconds
pub(crate) fn now_micros() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0) // Fallback to 0 if system time is before UNIX_EPOCH (shouldn't happen)
}

/// Serialize MetricMode to string
pub(crate) fn mode_to_string(mode: &MetricMode) -> String {
    mode.as_str().to_string()
}

/// Serialize MetricMode config to JSON if needed
pub(crate) fn mode_config_to_json(mode: &MetricMode) -> Option<String> {
    match mode {
        MetricMode::Sample { rate } => Some(serde_json::json!({ "rate": rate }).to_string()),
        MetricMode::SampleInterval { seconds } => {
            Some(serde_json::json!({ "seconds": seconds }).to_string())
        }
        MetricMode::Throttle { max_per_second } => {
            Some(serde_json::json!({ "max_per_second": max_per_second }).to_string())
        }
        _ => None,
    }
}

/// Deserialize MetricMode from string and config
pub(crate) fn string_to_mode(mode_type: &str, config: Option<&str>) -> Result<MetricMode> {
    match mode_type {
        MODE_STREAM => Ok(MetricMode::Stream),
        MODE_FIRST => Ok(MetricMode::First),
        MODE_SAMPLE => {
            let config = config
                .ok_or_else(|| Error::InvalidConfig("Sample mode requires config".to_string()))?;
            let json: serde_json::Value = serde_json::from_str(config)?;
            let rate = json["rate"]
                .as_u64()
                .ok_or_else(|| Error::InvalidConfig("Missing rate".to_string()))?
                as u32;
            Ok(MetricMode::Sample { rate })
        }
        MODE_SAMPLE_INTERVAL => {
            let config = config.ok_or_else(|| {
                Error::InvalidConfig("SampleInterval mode requires config".to_string())
            })?;
            let json: serde_json::Value = serde_json::from_str(config)?;
            let seconds = json["seconds"]
                .as_u64()
                .ok_or_else(|| Error::InvalidConfig("Missing seconds".to_string()))?
                as u32;
            Ok(MetricMode::SampleInterval { seconds })
        }
        MODE_THROTTLE => {
            let config = config
                .ok_or_else(|| Error::InvalidConfig("Throttle mode requires config".to_string()))?;
            let json: serde_json::Value = serde_json::from_str(config)?;
            let max_per_second = json["max_per_second"]
                .as_u64()
                .ok_or_else(|| Error::InvalidConfig("Missing max_per_second".to_string()))?
                as u32;
            Ok(MetricMode::Throttle { max_per_second })
        }
        _ => Err(Error::InvalidConfig(format!(
            "Unknown mode type: {}",
            mode_type
        ))),
    }
}

// =============================================================================
// Proptest tests for shared helpers
// =============================================================================

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// Generate arbitrary MetricMode values
    fn arb_metric_mode() -> impl Strategy<Value = MetricMode> {
        prop_oneof![
            Just(MetricMode::Stream),
            Just(MetricMode::First),
            (1..=10000u32).prop_map(|rate| MetricMode::Sample { rate }),
            (1..=86400u32).prop_map(|seconds| MetricMode::SampleInterval { seconds }),
            (1..=10000u32).prop_map(|max| MetricMode::Throttle {
                max_per_second: max
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Roundtrip: string_to_mode(mode_to_string(m), config) == m
        #[test]
        fn proptest_metric_mode_roundtrip(mode in arb_metric_mode()) {
            let mode_type = mode_to_string(&mode);
            let config = mode_config_to_json(&mode);

            let result = string_to_mode(&mode_type, config.as_deref());

            prop_assert!(result.is_ok(), "Failed for mode: {:?}, type: {}, config: {:?}", mode, mode_type, config);
            prop_assert_eq!(result.unwrap(), mode);
        }

        /// Mode type strings are consistent
        #[test]
        fn proptest_mode_type_strings_valid(mode in arb_metric_mode()) {
            let mode_type = mode_to_string(&mode);
            let expected = match mode {
                MetricMode::Stream => MODE_STREAM,
                MetricMode::First => MODE_FIRST,
                MetricMode::Sample { .. } => MODE_SAMPLE,
                MetricMode::SampleInterval { .. } => MODE_SAMPLE_INTERVAL,
                MetricMode::Throttle { .. } => MODE_THROTTLE,
            };
            prop_assert_eq!(mode_type, expected);
        }

        /// Config is None for simple modes, Some for parameterized modes
        #[test]
        fn proptest_mode_config_presence(mode in arb_metric_mode()) {
            let config = mode_config_to_json(&mode);
            match mode {
                MetricMode::Stream | MetricMode::First => {
                    prop_assert!(config.is_none(), "Simple modes should have no config");
                }
                MetricMode::Sample { .. } | MetricMode::SampleInterval { .. } | MetricMode::Throttle { .. } => {
                    prop_assert!(config.is_some(), "Parameterized modes should have config");
                }
            }
        }

        /// Invalid mode types return error, not panic
        #[test]
        fn proptest_invalid_mode_type_returns_error(s in "[a-z]{1,20}") {
            prop_assume!(![MODE_STREAM, MODE_FIRST, MODE_SAMPLE, MODE_SAMPLE_INTERVAL, MODE_THROTTLE].contains(&s.as_str()));

            let result = string_to_mode(&s, None);
            prop_assert!(result.is_err(), "Should reject unknown mode type: {}", s);
        }

        /// Missing config for modes that require it returns error
        #[test]
        fn proptest_missing_config_returns_error(mode_type in prop_oneof![
            Just(MODE_SAMPLE),
            Just(MODE_SAMPLE_INTERVAL),
            Just(MODE_THROTTLE),
        ]) {
            let result = string_to_mode(mode_type, None);
            prop_assert!(result.is_err(), "Should reject {} without config", mode_type);
        }

        /// Malformed JSON config returns error, not panic
        #[test]
        fn proptest_malformed_json_returns_error(
            mode_type in prop_oneof![Just(MODE_SAMPLE), Just(MODE_SAMPLE_INTERVAL), Just(MODE_THROTTLE)],
            bad_json in "[{\\[\"a-z0-9:,]{1,50}"
        ) {
            // Skip if it accidentally parses as valid JSON
            prop_assume!(serde_json::from_str::<serde_json::Value>(&bad_json).is_err());

            let result = string_to_mode(mode_type, Some(&bad_json));
            prop_assert!(result.is_err(), "Should reject malformed JSON: {}", bad_json);
        }

        /// Config with wrong keys returns error
        #[test]
        fn proptest_wrong_config_keys_returns_error(
            value in 1..1000u32,
        ) {
            // sample expects "rate", not "seconds"
            let bad_config = format!(r#"{{"seconds": {}}}"#, value);
            let result = string_to_mode(MODE_SAMPLE, Some(&bad_config));
            prop_assert!(result.is_err(), "Should reject sample with wrong key");

            // sample_interval expects "seconds", not "rate"
            let bad_config = format!(r#"{{"rate": {}}}"#, value);
            let result = string_to_mode(MODE_SAMPLE_INTERVAL, Some(&bad_config));
            prop_assert!(result.is_err(), "Should reject sample_interval with wrong key");
        }
    }
}
