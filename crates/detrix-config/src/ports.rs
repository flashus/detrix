//! Port Registry - Centralized port handling for Detrix
//!
//! This module provides a unified approach to port allocation across:
//! - Daemon services (HTTP, gRPC, GELF) - managed by PortRegistry
//! - CLI clients (reading ports from PID file)
//! - Tests (unique port allocation with atomic counters)
//!
//! # Port Types
//!
//! ## Daemon-level ports (managed by PortRegistry)
//! - HTTP/REST API server
//! - gRPC API server
//! - GELF output (Graylog)
//!
//! ## Connection-level ports (NOT managed by PortRegistry)
//! - Debugger connections (debugpy, delve, lldb-dap) are per-connection
//! - Each Connection entity has its own host:port stored in the database
//! - Managed by ConnectionService and AdapterLifecycleManager
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    PortRegistry                         │
//! │  ┌─────────────────────────────────────────────────┐   │
//! │  │ allocations: HashMap<ServiceType, PortAllocation>│   │
//! │  └─────────────────────────────────────────────────┘   │
//! │                                                         │
//! │  Daemon-level ports only:                               │
//! │  - Http (REST/WebSocket API)                            │
//! │  - Grpc (gRPC API)                                      │
//! │  - Gelf (Graylog output)                                │
//! └─────────────────────────────────────────────────────────┘
//!
//! ┌─────────────────────────────────────────────────────────┐
//! │           Connection-level ports (per-connection)       │
//! │  ┌─────────────────────────────────────────────────┐   │
//! │  │ Connection { host, port, language, ... }        │   │
//! │  └─────────────────────────────────────────────────┘   │
//! │                                                         │
//! │  Stored in database, one per debugger connection:       │
//! │  - 127.0.0.1:5678 (Python/debugpy)                     │
//! │  - 127.0.0.1:2345 (Go/delve)                           │
//! │  - 127.0.0.1:5679 (Rust/lldb-dap)                      │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ## Daemon (serve.rs)
//!
//! ```rust,ignore
//! let mut registry = PortRegistry::new();
//! registry.register(ServiceType::Http, 8080, true);  // fallback enabled
//! registry.register(ServiceType::Grpc, 50051, true);
//!
//! let http_port = registry.allocate(ServiceType::Http)?;
//! let grpc_port = registry.allocate(ServiceType::Grpc)?;
//!
//! // Store all ports in PID file
//! pid_file.set_ports(&registry)?;
//! ```
//!
//! ## CLI Client (reading from PID file)
//!
//! ```rust,ignore
//! let info = PidFile::read_info(&pid_path)?;
//! let grpc_port = info.get_port(ServiceType::Grpc).unwrap_or(DEFAULT_GRPC_PORT);
//! ```
//!
//! ## Tests (unique port allocation for parallel tests)
//!
//! ```rust,ignore
//! // For daemon services
//! let http_port = TestPortAllocator::get().allocate_http();
//! let grpc_port = TestPortAllocator::get().allocate_grpc();
//!
//! // For any other ports (e.g., spinning up debuggers in tests)
//! let debugger_port = TestPortAllocator::get().allocate_any();
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;

use crate::constants::{DEFAULT_GELF_PORT, DEFAULT_GRPC_PORT, DEFAULT_REST_PORT};

// ============================================================================
// ERRORS
// ============================================================================

/// Errors that can occur during port operations
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("Port {0} is unavailable (in use by another process)")]
    PortUnavailable(u16),

    #[error("No available port found in range {0}-{1}")]
    NoAvailablePort(u16, u16),

    #[error("Service {0:?} not registered")]
    ServiceNotRegistered(ServiceType),

    #[error("Service {0:?} already allocated")]
    AlreadyAllocated(ServiceType),
}

/// Result type for port operations
pub type PortResult<T> = Result<T, PortError>;

// ============================================================================
// SERVICE TYPE
// ============================================================================

/// Types of daemon-level services that use network ports.
///
/// These are services that the detrix daemon allocates and manages:
/// - `Http` - REST/WebSocket API server
/// - `Grpc` - gRPC API server
/// - `Gelf` - Graylog output
///
/// Debugger ports (debugpy, delve, lldb-dap) are NOT included here because
/// they are external - debuggers are started on their own ports and those
/// ports are passed TO detrix when creating a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    /// HTTP/REST API server (also handles WebSocket)
    Http,
    /// gRPC API server
    Grpc,
    /// GELF output (Graylog)
    Gelf,
}

impl ServiceType {
    /// Get the default port for this service type
    pub fn default_port(self) -> u16 {
        match self {
            ServiceType::Http => DEFAULT_REST_PORT,
            ServiceType::Grpc => DEFAULT_GRPC_PORT,
            ServiceType::Gelf => DEFAULT_GELF_PORT,
        }
    }

    /// Get all daemon service types
    pub fn all() -> &'static [ServiceType] {
        &[ServiceType::Http, ServiceType::Grpc, ServiceType::Gelf]
    }
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceType::Http => write!(f, "http"),
            ServiceType::Grpc => write!(f, "grpc"),
            ServiceType::Gelf => write!(f, "gelf"),
        }
    }
}

// ============================================================================
// PORT UTILITIES
// ============================================================================

/// Check if a port is available on localhost (127.0.0.1).
///
/// Only checks 127.0.0.1 since Detrix binds to localhost.
pub fn is_port_available(port: u16) -> bool {
    let addr_localhost = SocketAddr::from(([127, 0, 0, 1], port));
    TcpListener::bind(addr_localhost).is_ok()
}

/// Find the next available port in a range.
///
/// # Arguments
/// * `start_port` - The preferred/starting port
/// * `end_port` - The maximum port to try
///
/// # Returns
/// The first available port in the range, or None if all ports are occupied.
pub fn find_available_port(start_port: u16, end_port: u16) -> Option<u16> {
    (start_port..=end_port).find(|&port| is_port_available(port))
}

/// Find an available port with default range (preferred_port to preferred_port + 100).
pub fn find_available_port_default(preferred_port: u16) -> Option<u16> {
    find_available_port(preferred_port, preferred_port.saturating_add(100))
}

// ============================================================================
// PORT ALLOCATION
// ============================================================================

/// Configuration for a single port allocation
#[derive(Debug, Clone)]
pub struct PortAllocation {
    /// Preferred port number
    preferred: u16,
    /// Actually allocated port (after checking availability)
    actual: Option<u16>,
    /// Whether to auto-select an available port if preferred is occupied
    fallback_enabled: bool,
}

impl PortAllocation {
    /// Create a new port allocation configuration
    pub fn new(preferred: u16, fallback_enabled: bool) -> Self {
        Self {
            preferred,
            actual: None,
            fallback_enabled,
        }
    }

    /// Get the preferred port
    pub fn preferred(&self) -> u16 {
        self.preferred
    }

    /// Get the actual allocated port (if allocation was successful)
    pub fn actual(&self) -> Option<u16> {
        self.actual
    }

    /// Check if fallback is enabled
    pub fn fallback_enabled(&self) -> bool {
        self.fallback_enabled
    }
}

// ============================================================================
// PORT REGISTRY
// ============================================================================

/// Central registry for all port allocations in a Detrix instance.
///
/// Tracks which ports are registered, allocated, and provides fallback logic.
#[derive(Debug, Default)]
pub struct PortRegistry {
    allocations: HashMap<ServiceType, PortAllocation>,
}

impl PortRegistry {
    /// Create a new empty port registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a port registry from config values
    ///
    /// # Arguments
    /// * `http_port` - Preferred HTTP port
    /// * `grpc_port` - Preferred gRPC port
    /// * `gelf_port` - Preferred GELF output port
    /// * `fallback_enabled` - Whether to enable port fallback for all services
    pub fn from_config(
        http_port: u16,
        grpc_port: u16,
        gelf_port: u16,
        fallback_enabled: bool,
    ) -> Self {
        let mut registry = Self::new();
        registry.register(ServiceType::Http, http_port, fallback_enabled);
        registry.register(ServiceType::Grpc, grpc_port, fallback_enabled);
        registry.register(ServiceType::Gelf, gelf_port, fallback_enabled);
        registry
    }

    /// Register a service with a preferred port
    ///
    /// # Arguments
    /// * `service` - The service type
    /// * `preferred` - Preferred port number
    /// * `fallback_enabled` - Whether to auto-select an available port if preferred is occupied
    pub fn register(&mut self, service: ServiceType, preferred: u16, fallback_enabled: bool) {
        self.allocations
            .insert(service, PortAllocation::new(preferred, fallback_enabled));
    }

    /// Allocate a port for a registered service.
    ///
    /// If fallback is enabled and the preferred port is occupied, finds the next available port.
    /// If fallback is disabled and the preferred port is occupied, returns an error.
    ///
    /// # Returns
    /// The allocated port number, or an error if allocation failed.
    pub fn allocate(&mut self, service: ServiceType) -> PortResult<u16> {
        let allocation = self
            .allocations
            .get_mut(&service)
            .ok_or(PortError::ServiceNotRegistered(service))?;

        if allocation.actual.is_some() {
            return Err(PortError::AlreadyAllocated(service));
        }

        let preferred = allocation.preferred;

        if allocation.fallback_enabled {
            // Try to find an available port starting from preferred
            let port = find_available_port_default(preferred)
                .ok_or(PortError::NoAvailablePort(preferred, preferred + 100))?;

            allocation.actual = Some(port);
            Ok(port)
        } else {
            // Strict mode: fail if preferred port is unavailable
            if !is_port_available(preferred) {
                return Err(PortError::PortUnavailable(preferred));
            }

            allocation.actual = Some(preferred);
            Ok(preferred)
        }
    }

    /// Get the allocated port for a service (if allocated)
    pub fn get(&self, service: ServiceType) -> Option<u16> {
        self.allocations.get(&service).and_then(|a| a.actual)
    }

    /// Get all allocated ports as a map
    pub fn all_allocated(&self) -> HashMap<ServiceType, u16> {
        self.allocations
            .iter()
            .filter_map(|(service, alloc)| alloc.actual.map(|port| (*service, port)))
            .collect()
    }

    /// Check if a service is registered
    pub fn is_registered(&self, service: ServiceType) -> bool {
        self.allocations.contains_key(&service)
    }

    /// Check if a service has an allocated port
    pub fn is_allocated(&self, service: ServiceType) -> bool {
        self.get(service).is_some()
    }
}

// ============================================================================
// TEST PORT ALLOCATOR
// ============================================================================

/// Port allocator for tests with unique port allocation.
///
/// Uses atomic counters to ensure unique ports across parallel tests.
/// Ports are allocated with random offsets to avoid collisions between test runs.
///
/// For daemon services (HTTP, gRPC), use `allocate(ServiceType)`.
/// For any other port needs (e.g., spinning up debuggers in tests), use `allocate_any()`.
pub struct TestPortAllocator {
    http_counter: AtomicU16,
    grpc_counter: AtomicU16,
    /// General-purpose counter for any port needs (debuggers, etc.)
    general_counter: AtomicU16,
}

impl TestPortAllocator {
    /// Get the global test port allocator instance
    pub fn get() -> &'static Self {
        static INSTANCE: OnceLock<TestPortAllocator> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    /// Create a new test port allocator with randomized initial offsets
    fn new() -> Self {
        // Use random offset based on process ID and time to avoid collisions
        let seed = (std::process::id() as u64).wrapping_mul(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(12345),
        );
        let offset = ((seed % 1000) as u16) * 10; // 0-9990 in steps of 10

        Self {
            http_counter: AtomicU16::new(8000 + offset),
            grpc_counter: AtomicU16::new(50000 + offset),
            general_counter: AtomicU16::new(5000 + offset), // For debuggers, etc.
        }
    }

    /// Allocate a unique HTTP port for testing
    pub fn allocate_http(&self) -> u16 {
        self.allocate_with_counter(&self.http_counter)
    }

    /// Allocate a unique gRPC port for testing
    pub fn allocate_grpc(&self) -> u16 {
        self.allocate_with_counter(&self.grpc_counter)
    }

    /// Allocate any available port for testing (e.g., for debuggers)
    pub fn allocate_any(&self) -> u16 {
        self.allocate_with_counter(&self.general_counter)
    }

    /// Allocate a port by daemon service type
    pub fn allocate(&self, service: ServiceType) -> u16 {
        match service {
            ServiceType::Http => self.allocate_http(),
            ServiceType::Grpc => self.allocate_grpc(),
            ServiceType::Gelf => self.allocate_any(), // GELF uses general counter
        }
    }

    /// Find an available port starting from the given base, incrementing until one is found
    fn allocate_with_counter(&self, counter: &AtomicU16) -> u16 {
        let base = counter.fetch_add(10, Ordering::SeqCst);
        self.find_available_port_from(base, 10)
    }

    /// Find an available port starting from base
    fn find_available_port_from(&self, base: u16, max_attempts: u16) -> u16 {
        for offset in 0..max_attempts {
            let port = base.wrapping_add(offset);
            if is_port_available(port) {
                return port;
            }
        }
        // Fallback: return base port even if not available (will fail later with clearer error)
        base
    }
}

impl Default for TestPortAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_port_available() {
        // Port 0 should always work (OS assigns available port)
        assert!(is_port_available(0));

        // Port 1 should be unavailable (privileged)
        assert!(!is_port_available(1));
    }

    #[test]
    fn test_find_available_port() {
        // Should find an available port in a large range
        let port = find_available_port(50000, 50100);
        assert!(port.is_some());

        // Verify the returned port is in the expected range
        if let Some(p) = port {
            assert!(p >= 50000);
            assert!(p <= 50100);
        }
    }

    #[test]
    fn test_find_available_port_default() {
        let port = find_available_port_default(50000);
        assert!(port.is_some());

        if let Some(p) = port {
            assert!(p >= 50000);
            assert!(p <= 50100);
        }
    }

    #[test]
    fn test_service_type_default_ports() {
        assert_eq!(ServiceType::Http.default_port(), DEFAULT_REST_PORT);
        assert_eq!(ServiceType::Grpc.default_port(), DEFAULT_GRPC_PORT);
        assert_eq!(ServiceType::Gelf.default_port(), DEFAULT_GELF_PORT);
    }

    #[test]
    fn test_service_type_serialization() {
        // Test serialization
        let http = ServiceType::Http;
        let json = serde_json::to_string(&http).unwrap();
        assert_eq!(json, "\"http\"");

        // Test deserialization
        let parsed: ServiceType = serde_json::from_str("\"grpc\"").unwrap();
        assert_eq!(parsed, ServiceType::Grpc);

        // Test all service types roundtrip
        for service in ServiceType::all() {
            let json = serde_json::to_string(service).unwrap();
            let parsed: ServiceType = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, service);
        }
    }

    #[test]
    fn test_port_registry_basic() {
        let mut registry = PortRegistry::new();
        registry.register(ServiceType::Http, 8080, true);
        registry.register(ServiceType::Grpc, 50051, true);

        assert!(registry.is_registered(ServiceType::Http));
        assert!(registry.is_registered(ServiceType::Grpc));
        assert!(!registry.is_registered(ServiceType::Gelf)); // Not registered yet

        // Before allocation
        assert!(!registry.is_allocated(ServiceType::Http));
        assert_eq!(registry.get(ServiceType::Http), None);

        // After allocation
        let http_port = registry.allocate(ServiceType::Http).unwrap();
        assert!(registry.is_allocated(ServiceType::Http));
        assert_eq!(registry.get(ServiceType::Http), Some(http_port));
    }

    #[test]
    fn test_port_registry_from_config() {
        let registry = PortRegistry::from_config(8080, 50051, 12201, true);

        assert!(registry.is_registered(ServiceType::Http));
        assert!(registry.is_registered(ServiceType::Grpc));
        assert!(registry.is_registered(ServiceType::Gelf));
    }

    #[test]
    fn test_port_registry_no_fallback() {
        let mut registry = PortRegistry::new();

        // Use port 1 which is typically reserved and unavailable
        registry.register(ServiceType::Http, 1, false);

        // Should fail since port 1 is not available and fallback is disabled
        let result = registry.allocate(ServiceType::Http);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PortError::PortUnavailable(1)));
    }

    #[test]
    fn test_port_registry_already_allocated() {
        let mut registry = PortRegistry::new();
        registry.register(ServiceType::Http, 60000, true);

        // First allocation should succeed
        registry.allocate(ServiceType::Http).unwrap();

        // Second allocation should fail
        let result = registry.allocate(ServiceType::Http);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PortError::AlreadyAllocated(ServiceType::Http)
        ));
    }

    #[test]
    fn test_port_registry_service_not_registered() {
        let mut registry = PortRegistry::new();

        let result = registry.allocate(ServiceType::Http);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PortError::ServiceNotRegistered(ServiceType::Http)
        ));
    }

    #[test]
    fn test_port_registry_all_allocated() {
        let mut registry = PortRegistry::new();
        registry.register(ServiceType::Http, 60001, true);
        registry.register(ServiceType::Grpc, 60002, true);

        registry.allocate(ServiceType::Http).unwrap();
        registry.allocate(ServiceType::Grpc).unwrap();

        let all = registry.all_allocated();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key(&ServiceType::Http));
        assert!(all.contains_key(&ServiceType::Grpc));
    }

    #[test]
    fn test_test_allocator_uniqueness() {
        let allocator = TestPortAllocator::get();

        // Allocate multiple ports and verify uniqueness
        let mut ports = std::collections::HashSet::new();

        for _ in 0..10 {
            let http = allocator.allocate_http();
            let grpc = allocator.allocate_grpc();
            let any = allocator.allocate_any();

            // All ports should be unique
            assert!(ports.insert(http), "Duplicate HTTP port: {}", http);
            assert!(ports.insert(grpc), "Duplicate gRPC port: {}", grpc);
            assert!(ports.insert(any), "Duplicate general port: {}", any);
        }
    }

    #[test]
    fn test_test_allocator_by_service_type() {
        let allocator = TestPortAllocator::get();

        let http = allocator.allocate(ServiceType::Http);
        let grpc = allocator.allocate(ServiceType::Grpc);
        let gelf = allocator.allocate(ServiceType::Gelf);

        // All should be different
        assert_ne!(http, grpc);
        assert_ne!(http, gelf);
        assert_ne!(grpc, gelf);
    }

    #[test]
    fn test_is_port_available_detects_occupied_port() {
        // Find an available port first
        let port = find_available_port(50100, 50200).expect("Should find available port");

        // Verify it's available
        assert!(is_port_available(port), "Port {} should be available", port);

        // Bind to the port
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let _listener = TcpListener::bind(addr).expect("Should bind to port");

        // Now the port should NOT be available
        assert!(
            !is_port_available(port),
            "Port {} should NOT be available when bound",
            port
        );

        // Drop listener - port should be available again (after potential TIME_WAIT)
        // Note: On some systems, there may be a brief TIME_WAIT period
    }

    #[test]
    fn test_port_fallback_when_port_occupied() {
        // Find an available port
        let preferred_port = find_available_port(50300, 50400).expect("Should find available port");

        // Bind to the preferred port
        let addr = SocketAddr::from(([127, 0, 0, 1], preferred_port));
        let _listener = TcpListener::bind(addr).expect("Should bind to port");

        // Create a registry with fallback enabled
        let mut registry = PortRegistry::new();
        registry.register(ServiceType::Http, preferred_port, true);

        // Allocate should succeed but return a different port
        let allocated_port = registry.allocate(ServiceType::Http).unwrap();

        // The allocated port should be different from the preferred (blocked) port
        assert_ne!(
            allocated_port, preferred_port,
            "Port fallback should find a different port when preferred is occupied"
        );

        // The allocated port should be available and nearby
        assert!(
            allocated_port > preferred_port && allocated_port <= preferred_port + 100,
            "Fallback port {} should be in range [{}, {}]",
            allocated_port,
            preferred_port + 1,
            preferred_port + 100
        );
    }
}
