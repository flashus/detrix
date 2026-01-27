//! DAP Adapter Factory Implementation
//!
//! Implements the DapAdapterFactory trait from detrix-application.
//! This provides a concrete implementation for creating language-specific DAP adapters.

use async_trait::async_trait;
use detrix_application::{DapAdapterFactory, DapAdapterRef};
use detrix_core::Result;
use std::path::PathBuf;
use std::sync::Arc;

use crate::{GoAdapter, PythonAdapter, RustAdapter};

/// Default implementation of DapAdapterFactory
///
/// This factory creates language-specific DAP adapters.
/// Currently supports:
/// - Python (debugpy)
/// - Go (Delve)
/// - Rust (CodeLLDB / lldb-dap)
pub struct DapAdapterFactoryImpl {
    /// Base path for resolving relative file paths
    base_path: PathBuf,
}

impl DapAdapterFactoryImpl {
    /// Create a new DapAdapterFactoryImpl
    ///
    /// # Arguments
    /// * `base_path` - Base directory for resolving relative file paths in metrics
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
        }
    }
}

#[async_trait]
impl DapAdapterFactory for DapAdapterFactoryImpl {
    async fn create_python_adapter(&self, _host: &str, port: u16) -> Result<DapAdapterRef> {
        // Create default debugpy configuration for the given port
        let config = PythonAdapter::default_config(port);

        // Create PythonAdapter instance
        let adapter = PythonAdapter::new(config, self.base_path.clone());

        // Wrap in Arc and return
        Ok(Arc::new(adapter))
    }

    async fn create_go_adapter(&self, _host: &str, port: u16) -> Result<DapAdapterRef> {
        // Create default Delve configuration for the given port
        let config = GoAdapter::default_config(port);

        // Create GoAdapter instance
        let adapter = GoAdapter::new(config, self.base_path.clone());

        // Wrap in Arc and return
        Ok(Arc::new(adapter))
    }

    async fn create_rust_adapter(
        &self,
        _host: &str,
        port: u16,
        program: Option<&str>,
    ) -> Result<DapAdapterRef> {
        // Choose configuration based on whether a program path is provided
        let config = match program {
            // Direct TCP connection to lldb-dap: send program path in launch request
            // This is the preferred mode for direct lldb-dap connection
            Some(program_path) => RustAdapter::config_direct_launch(program_path, port),
            // Attach mode: connect to existing lldb-dap (e.g., via lldb-serve wrapper)
            None => RustAdapter::default_config(port),
        };

        // Create RustAdapter instance
        let adapter = RustAdapter::new(config, self.base_path.clone());

        // Wrap in Arc and return
        Ok(Arc::new(adapter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_factory_creates_python_adapter() {
        let factory = DapAdapterFactoryImpl::new(PathBuf::from("/tmp"));
        let result = factory.create_python_adapter("127.0.0.1", 5678).await;

        assert!(result.is_ok());
        let adapter = result.unwrap();

        // Verify it's wrapped correctly (type check via trait object)
        // Just checking that we got a valid Arc<dyn DapAdapter>
        assert!(Arc::strong_count(&adapter) == 1);
    }

    #[tokio::test]
    async fn test_factory_with_different_ports() {
        let factory = DapAdapterFactoryImpl::new(PathBuf::from("/tmp"));

        // Test with different ports
        for port in [5678, 5679, 5680] {
            let result = factory.create_python_adapter("127.0.0.1", port).await;
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_factory_with_different_base_paths() {
        let paths = vec!["/tmp", "/var/log", "/home/user/projects"];

        for path in paths {
            let factory = DapAdapterFactoryImpl::new(PathBuf::from(path));
            let result = factory.create_python_adapter("127.0.0.1", 5678).await;
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_factory_creates_go_adapter() {
        let factory = DapAdapterFactoryImpl::new(PathBuf::from("/tmp"));
        let result = factory.create_go_adapter("127.0.0.1", 38697).await;

        assert!(result.is_ok());
        let adapter = result.unwrap();

        // Verify it's wrapped correctly
        assert!(Arc::strong_count(&adapter) == 1);
    }

    #[tokio::test]
    async fn test_factory_go_with_different_ports() {
        let factory = DapAdapterFactoryImpl::new(PathBuf::from("/tmp"));

        // Test with different ports (Delve commonly uses 38697, but can be any port)
        for port in [38697, 38698, 2345] {
            let result = factory.create_go_adapter("127.0.0.1", port).await;
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_factory_creates_rust_adapter() {
        let factory = DapAdapterFactoryImpl::new(PathBuf::from("/tmp"));
        let result = factory.create_rust_adapter("127.0.0.1", 4711, None).await;

        assert!(result.is_ok());
        let adapter = result.unwrap();

        // Verify it's wrapped correctly
        assert!(Arc::strong_count(&adapter) == 1);
    }

    #[tokio::test]
    async fn test_factory_rust_with_different_ports() {
        let factory = DapAdapterFactoryImpl::new(PathBuf::from("/tmp"));

        // Test with different ports
        for port in [4711, 4712, 13000] {
            let result = factory.create_rust_adapter("127.0.0.1", port, None).await;
            assert!(result.is_ok());
        }
    }
}
