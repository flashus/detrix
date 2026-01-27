//! Configuration types and loading for Detrix
//!
//! This crate provides:
//! - Configuration structures for all Detrix components
//! - Config file loading (TOML format)
//! - Environment variable overrides
//!
//! # Architecture
//!
//! Configuration is an **infrastructure concern** and lives outside the domain layer.
//! This crate depends on `detrix-core` only for domain types used in config
//! (e.g., `Location`, `MetricMode`, `SafetyLevel`).
//!
//! # Module Organization
//!
//! Configuration is split into logical modules:
//! - `adapter` - DAP adapter connection settings
//! - `anchor` - Metric location tracking and auto-relocation settings
//! - `api` - gRPC and REST endpoint settings
//! - `daemon` - Background service settings
//! - `output` - GELF/Graylog output settings
//! - `safety` - Expression safety validation settings
//! - `storage` - SQLite and event batching settings
//! - `tui` - Terminal UI settings
//! - `types` - Core config types and re-exports
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_config::{Config, load_config};
//! use std::path::Path;
//!
//! let config = load_config(Path::new("detrix.toml"))?;
//! println!("Storage path: {}", config.storage.path);
//! ```

mod loader;

// Default constants for all configuration values
pub mod constants;

// Path utilities
pub mod paths;

// Port registry and utilities
pub mod ports;

// PID file data structures (PidInfo)
pub mod pid;

// Config modules - organized by domain
mod adapter;
mod anchor;
mod api;
mod daemon;
mod output;
mod safety;
mod storage;
mod tui;

// Core types module that re-exports from submodules
mod types;

pub use loader::{
    create_config, ensure_default_config, load_config, load_config_from_str, resolve_config_path,
    ConfigError, ConfigTemplate, DEFAULT_CONFIG, FULL_CONFIG, MINIMAL_CONFIG,
};
pub use ports::{
    find_available_port, find_available_port_default, is_port_available, PortAllocation, PortError,
    PortRegistry, PortResult, ServiceType, TestPortAllocator,
};
pub use types::*;

// Re-export commonly used constants for convenience
pub use constants::{
    DEFAULT_DAEMON_POLL_INTERVAL_MS, DEFAULT_DAP_SOCKET_READ_TIMEOUT_SECS,
    DEFAULT_LLDB_CONFIG_DONE_DELAY_MS, DEFAULT_LOG_MESSAGE_PREVIEW_LEN,
    DEFAULT_LOG_PAYLOAD_PREVIEW_LEN, DEFAULT_MAX_PER_SECOND, DEFAULT_MAX_QUERY_LIMIT,
    DEFAULT_QUERY_LIMIT, DEFAULT_SAMPLE_INTERVAL_SECONDS, DEFAULT_SAMPLE_RATE,
    DEFAULT_TCP_KEEPALIVE_INTERVAL_SECS, DEFAULT_TCP_KEEPALIVE_RETRIES,
    DEFAULT_TCP_KEEPALIVE_TIME_SECS, LOCALHOST_IPV4,
};
