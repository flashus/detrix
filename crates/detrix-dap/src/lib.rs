//! Detrix DAP - Debug Adapter Protocol implementation
//!
//! This crate provides DAP protocol types and broker implementation
//! for managing debug adapters (debugpy, delve, CodeLLDB, etc.)
//!
//! # Architecture
//!
//! The crate uses a generic base adapter pattern:
//! - `BaseAdapter<P>` handles common DAP operations (connect, set_metric, etc.)
//! - Each language implements `OutputParser` trait for specific parsing
//! - This reduces code duplication while allowing language-specific customization

pub mod adapter;
pub mod base;
pub mod broker;
pub mod constants;
pub mod error;
pub mod error_detection;
pub mod ext;
pub mod factory;
pub mod go;
pub mod null_adapter;
pub mod protocol;
pub mod python;
pub mod rust;

pub use adapter::{
    AdapterConfig, AdapterProcess, AdapterState, AdapterSubprocessConfig, ConnectionMode,
};
pub use base::{find_metric_for_error, BaseAdapter, OutputParser, DETRICS_PREFIX};
pub use broker::DapBroker;
pub use error::{Error, Result};
pub use ext::DebugResult;
pub use factory::DapAdapterFactoryImpl;
pub use go::GoAdapter;
pub use null_adapter::NullAdapter;
pub use protocol::*;
pub use python::PythonAdapter;
pub use rust::RustAdapter;

// Re-export metric result types from detrix_application for convenience
pub use detrix_application::{RemoveMetricResult, SetMetricResult};
