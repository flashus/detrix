//! Reconnecting Adapter Factory
//!
//! A factory decorator that wraps adapters with reconnection middleware.
//!
//! # Design
//!
//! This follows the Decorator pattern at the factory level:
//! - Wraps any existing `DapAdapterFactory` implementation
//! - Adapters created by the inner factory are automatically wrapped with `ReconnectingAdapter`
//! - Configuration for reconnection is passed at factory creation time
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_application::middleware::ReconnectingAdapterFactory;
//! use detrix_config::ReconnectConfig;
//!
//! let base_factory = DapAdapterFactoryImpl::new("/path/to/project");
//! let reconnecting_factory = ReconnectingAdapterFactory::new(
//!     Arc::new(base_factory),
//!     ReconnectConfig::default(),
//! );
//! // Use reconnecting_factory - all adapters will have automatic reconnection
//! ```

use crate::{DapAdapterFactory, DapAdapterFactoryRef, DapAdapterRef};
use async_trait::async_trait;
use detrix_config::ReconnectConfig;
use detrix_core::Result;
use std::sync::Arc;

use super::ReconnectingAdapter;

/// Factory decorator that wraps adapters with reconnection middleware
///
/// This factory delegates to an inner factory and wraps all created adapters
/// with `ReconnectingAdapter` to add automatic reconnection with exponential backoff.
pub struct ReconnectingAdapterFactory {
    /// The wrapped factory
    inner: DapAdapterFactoryRef,
    /// Reconnection configuration to apply to all adapters
    config: ReconnectConfig,
}

impl ReconnectingAdapterFactory {
    /// Create a new reconnecting adapter factory
    ///
    /// # Arguments
    /// * `inner` - The underlying factory to delegate adapter creation to
    /// * `config` - Reconnection configuration to apply to all adapters
    pub fn new(inner: DapAdapterFactoryRef, config: ReconnectConfig) -> Self {
        Self { inner, config }
    }

    /// Create with default reconnection configuration
    pub fn with_defaults(inner: DapAdapterFactoryRef) -> Self {
        Self::new(inner, ReconnectConfig::default())
    }

    /// Get a reference to the reconnection configuration
    pub fn config(&self) -> &ReconnectConfig {
        &self.config
    }
}

#[async_trait]
impl DapAdapterFactory for ReconnectingAdapterFactory {
    async fn create_python_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        // Create the base adapter using the inner factory
        let base_adapter = self.inner.create_python_adapter(host, port).await?;

        // Wrap with reconnection middleware if enabled
        if self.config.enabled {
            let reconnecting = ReconnectingAdapter::new(base_adapter, self.config.clone());
            Ok(Arc::new(reconnecting))
        } else {
            // Return unwrapped if reconnection is disabled
            Ok(base_adapter)
        }
    }

    async fn create_go_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        // Create the base adapter using the inner factory
        let base_adapter = self.inner.create_go_adapter(host, port).await?;

        // Wrap with reconnection middleware if enabled
        if self.config.enabled {
            let reconnecting = ReconnectingAdapter::new(base_adapter, self.config.clone());
            Ok(Arc::new(reconnecting))
        } else {
            // Return unwrapped if reconnection is disabled
            Ok(base_adapter)
        }
    }

    async fn create_rust_adapter(
        &self,
        host: &str,
        port: u16,
        program: Option<&str>,
        pid: Option<u32>,
    ) -> Result<DapAdapterRef> {
        // Create the base adapter using the inner factory
        let base_adapter = self
            .inner
            .create_rust_adapter(host, port, program, pid)
            .await?;

        // Wrap with reconnection middleware if enabled
        if self.config.enabled {
            let reconnecting = ReconnectingAdapter::new(base_adapter, self.config.clone());
            Ok(Arc::new(reconnecting))
        } else {
            // Return unwrapped if reconnection is disabled
            Ok(base_adapter)
        }
    }
}
