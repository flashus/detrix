//! Test helpers for detrix-api
//!
//! This module exists to de-duplicate integration-style test setup that would
//! otherwise be copy/pasted across multiple `#[cfg(test)]` modules.

#![cfg(test)]

use crate::ApiState;
use detrix_application::{
    AppContext, ConnectionRepositoryRef, DapAdapterFactoryRef, EventRepositoryRef,
    MetricRepositoryRef,
};
use detrix_config::ApiConfig;
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::MockDapAdapterFactory;
use std::sync::Arc;
use tempfile::TempDir;

/// Create a test ApiState backed by a temporary SQLite database.
///
/// Returns the state and the TempDir guard that keeps the DB alive.
pub async fn create_test_state() -> (Arc<ApiState>, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test.db");
    let config = SqliteConfig {
        path: db_path,
        ..Default::default()
    };
    let storage = Arc::new(SqliteStorage::new(&config).await.unwrap());
    let mock_factory = Arc::new(MockDapAdapterFactory::new());

    let context = AppContext::new(
        Arc::clone(&storage) as MetricRepositoryRef,
        Arc::clone(&storage) as EventRepositoryRef,
        Arc::clone(&storage) as ConnectionRepositoryRef,
        mock_factory as DapAdapterFactoryRef,
        &ApiConfig::default(),
        &detrix_config::SafetyConfig::default(),
        &detrix_config::StorageConfig::default(),
        &detrix_config::DaemonConfig::default(),
        &detrix_config::AdapterConnectionConfig::default(),
        &detrix_config::AnchorConfig::default(),
        &detrix_config::LimitsConfig::default(),
        None,
        None, // No separate DLQ storage in tests
    );

    let state = Arc::new(ApiState::builder(context, storage).build());
    (state, temp_dir)
}
