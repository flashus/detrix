//! Domain types for Agent Connection Manager.
//!
//! All types are pure domain types with no proto dependencies.
//! Proto conversion happens at the gRPC boundary in `detrix-api`.

use std::collections::HashSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::services::AdapterLifecycleManager;
use dashmap::{DashMap, DashSet};
use detrix_core::{ConnectionId, MetricEvent, SystemEvent};
use detrix_ports::{ConnectionRepositoryRef, MetricRepositoryRef};
use tokio::sync::{mpsc, oneshot};

// ============================================================================
// Agent Capabilities & Binary Info
// ============================================================================

/// Domain representation of agent capabilities.
/// Converted from proto at the gRPC boundary.
#[derive(Debug, Clone, Default)]
pub struct AgentCapabilities {
    pub ebpf: bool,
    pub dap_python: bool,
    pub dap_go: bool,
    pub dap_rust: bool,
    pub supported_envelope_schemas: Vec<u32>,
    pub supported_capture_profiles: Vec<String>,
    pub max_capture_payload_bytes: u32,
}

impl AgentCapabilities {
    /// Validate the negotiated wire contract before a connection is marked
    /// usable. Legacy agents advertise no envelope/profile fields; that is
    /// still valid for the legacy Go path, but an explicitly selected Rust
    /// DRX1 path must prove both sides of the negotiation.
    pub fn validate_capture_admission(
        &self,
        selected_backend: &str,
        capture_profile: &str,
        reported_envelope_schemas: &[u32],
        reported_capture_profiles: &[String],
        reported_max_payload_bytes: u32,
    ) -> Result<(), String> {
        if !selected_backend.eq_ignore_ascii_case("ebpf") {
            return Ok(());
        }

        const DRX1_SCHEMA: u32 = 1;
        const DRX1_MIN_PAYLOAD_BYTES: u32 = 4096;

        if !matches!(capture_profile.to_ascii_lowercase().as_str(), "go" | "rust") {
            // Unknown/third-party profiles are admitted by their own backend
            // policy; this built-in wire contract only covers Go and Rust.
            return Ok(());
        }

        let registered_ok =
            self.supports_profile_drx1(capture_profile, DRX1_SCHEMA, DRX1_MIN_PAYLOAD_BYTES);
        // Older agents do not advertise a wire profile at all. Preserve the
        // legacy Go eBPF admission path while upgraded agents negotiate DRX1;
        // Rust remains opt-in and fail-closed until it proves the handshake.
        if capture_profile.eq_ignore_ascii_case("go") && !registered_ok {
            return Ok(());
        }
        let reported_ok = reported_envelope_schemas.contains(&DRX1_SCHEMA)
            && reported_capture_profiles
                .iter()
                .any(|profile| profile.eq_ignore_ascii_case(capture_profile))
            && reported_max_payload_bytes >= DRX1_MIN_PAYLOAD_BYTES;

        if registered_ok && reported_ok {
            Ok(())
        } else if !registered_ok {
            Err(format!(
                "agent registration does not advertise {capture_profile} DRX1/schema 1 with a 4096-byte payload"
            ))
        } else {
            Err(format!(
                "connection update does not confirm {capture_profile} DRX1/schema 1 with a 4096-byte payload"
            ))
        }
    }

    fn supports_profile_drx1(&self, profile: &str, schema: u32, min_payload_bytes: u32) -> bool {
        self.ebpf
            && self.supported_envelope_schemas.contains(&schema)
            && self
                .supported_capture_profiles
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(profile))
            && self.max_capture_payload_bytes >= min_payload_bytes
    }
}

#[cfg(test)]
mod capability_tests {
    use super::AgentCapabilities;

    fn rust_capabilities() -> AgentCapabilities {
        AgentCapabilities {
            ebpf: true,
            supported_envelope_schemas: vec![1],
            supported_capture_profiles: vec!["rust".into()],
            max_capture_payload_bytes: 4096,
            ..AgentCapabilities::default()
        }
    }

    #[test]
    fn accepts_matching_rust_drx1_advertisement() {
        let capabilities = rust_capabilities();
        assert!(capabilities
            .validate_capture_admission("ebpf", "rust", &[1], &["rust".into()], 4096)
            .is_ok());
    }

    #[test]
    fn accepts_matching_go_drx1_advertisement() {
        let capabilities = AgentCapabilities {
            ebpf: true,
            supported_envelope_schemas: vec![1],
            supported_capture_profiles: vec!["go".into()],
            max_capture_payload_bytes: 4096,
            ..AgentCapabilities::default()
        };
        assert!(capabilities
            .validate_capture_admission("ebpf", "go", &[1], &["go".into()], 4096)
            .is_ok());
    }

    #[test]
    fn rejects_rust_when_registration_is_legacy() {
        let capabilities = AgentCapabilities {
            ebpf: true,
            ..AgentCapabilities::default()
        };
        let error = capabilities
            .validate_capture_admission("ebpf", "rust", &[1], &["rust".into()], 4096)
            .unwrap_err();
        assert!(error.contains("registration"));
    }

    #[test]
    fn rejects_rust_when_connection_update_is_incomplete() {
        let capabilities = rust_capabilities();
        let error = capabilities
            .validate_capture_admission("ebpf", "rust", &[1], &[], 4096)
            .unwrap_err();
        assert!(error.contains("connection update"));
    }

    #[test]
    fn preserves_legacy_go_and_dap_paths() {
        let capabilities = AgentCapabilities::default();
        assert!(capabilities
            .validate_capture_admission("ebpf", "go", &[], &[], 0)
            .is_ok());
        assert!(capabilities
            .validate_capture_admission("dap", "rust", &[], &[], 0)
            .is_ok());
    }
}

/// Domain representation of a binary reported by the agent scanner.
#[derive(Debug, Clone)]
pub struct AgentBinaryInfo {
    pub binary_path: String,
    pub pid: u32,
    /// Inode of `/proc/<pid>/exe` — used with pid to detect PID reuse.
    pub inode: u64,
    pub build_info: String,
    pub has_dwarf: bool,
    pub exported_functions: Vec<String>,
    pub language: detrix_core::SourceLanguage,
}

/// Variable information from file inspection.
#[derive(Debug, Clone)]
pub struct VariableInfo {
    pub name: String,
    pub type_name: String,
    pub line: u32,
}

// ============================================================================
// IncomingAgentMessage (Agent → Server)
// ============================================================================

/// All messages an agent can send, expressed in domain types.
#[derive(Debug)]
pub enum IncomingAgentMessage {
    ConnectionUpdate {
        connection_id: ConnectionId,
        status: detrix_core::ConnectionStatus,
        error: Option<String>,
        selected_backend: String,
        capture_profile: String,
        backend_reason: String,
        debug_image_source: String,
        failure_class: String,
        supported_envelope_schemas: Vec<u32>,
        supported_capture_profiles: Vec<String>,
        max_capture_payload_bytes: u32,
        target_architecture: String,
    },
    EventBatch {
        connection_id: ConnectionId,
        events: Vec<MetricEvent>,
    },
    SetMetricAck {
        request_id: String,
        verified: bool,
        actual_line: Option<u32>,
        error: Option<String>,
    },
    RemoveMetricAck {
        request_id: String,
        confirmed: bool,
        error: Option<String>,
    },
    FileResponse {
        request_id: String,
        content: Vec<u8>,
        error: Option<String>,
    },
    InspectResponse {
        request_id: String,
        variables: Vec<VariableInfo>,
        error: Option<String>,
    },
    DropCount {
        connection_id: ConnectionId,
        total_events_dropped: u64,
        kernel_events_dropped: u64,
        decode_events_dropped: u64,
        unavailable_fields: u64,
        events_decoded: u64,
    },
    Heartbeat {
        cpu: f32,
        memory_bytes: u64,
        active_probes: u32,
        uptime_secs: u64,
        events_forwarded: u32,
        events_dropped: u32,
    },
    RegisterUpdate {
        binaries: Vec<AgentBinaryInfo>,
    },
    Pong,
    Error {
        code: String,
        message: String,
    },
}

// ============================================================================
// OutgoingAgentMessage (Server → Agent)
// ============================================================================

/// Server → Agent messages (domain types, converted to proto at gRPC boundary).
#[derive(Debug, Clone)]
pub enum OutgoingAgentMessage {
    RegisterAck {
        accepted: bool,
        rejection_reason: String,
        min_compatible_version: String,
    },
    CreateConnection {
        connection_id: String,
        language: String,
        binary_path: String,
        host: String,
        port: u32,
        safe_mode: bool,
        capture_backend: String,
        capture_profile: String,
        debug_info_path: String,
    },
    CloseConnection {
        connection_id: String,
    },
    SetMetric {
        request_id: String,
        connection_id: String,
        metric_name: String,
        file: String,
        line: u32,
        expressions: Vec<String>,
        enabled: bool,
        metric_id: u64,
    },
    RemoveMetric {
        request_id: String,
        connection_id: String,
        metric_name: String,
    },
    ReadFile {
        request_id: String,
        connection_id: String,
        path: String,
    },
    InspectFile {
        request_id: String,
        connection_id: String,
        file: String,
        line: u32,
        find_variable: String,
    },
    Ping,
}

/// Alias for the agent's outgoing message channel type.
pub type AgentOutgoingTx = mpsc::UnboundedSender<OutgoingAgentMessage>;

// ============================================================================
// RegisterResult
// ============================================================================

/// Return value of `register_atomic` — distinguishes expected rejection
/// from unexpected failure.
#[derive(Debug)]
pub enum RegisterResult {
    Accepted,
    Rejected { reason: String },
}

// ============================================================================
// AgentInfo (internal)
// ============================================================================

#[allow(dead_code)]
pub(super) struct AgentInfo {
    pub agent_id: String,
    pub hostname: String,
    pub capabilities: AgentCapabilities,
    pub binaries: Vec<AgentBinaryInfo>,
    pub outgoing_tx: AgentOutgoingTx,
    pub connected_at: std::time::Instant,
}

// ============================================================================
// AgentConnectionManager (public struct, methods in mod.rs)
// ============================================================================

pub type AgentConnectionManagerRef = Arc<AgentConnectionManager>;

/// Manages agent connections on the server side.
///
/// Coordinates between the gRPC agent stream and the application layer.
/// All public methods use domain types — proto conversion happens at the
/// gRPC boundary.
pub struct AgentConnectionManager {
    /// agent_id → AgentInfo
    pub(super) agents: Arc<DashMap<String, AgentInfo>>,
    /// connection_id → agent_id — routing table
    pub(super) connection_to_agent: Arc<DashMap<ConnectionId, String>>,
    /// request_id → oneshot sender (request-response correlation)
    pub(super) pending_requests: Arc<DashMap<String, oneshot::Sender<IncomingAgentMessage>>>,
    /// connection_id → sender for event forwarding.
    /// The sender is used by dispatch() to forward events.
    /// The receiver is taken by RemoteAdapter::subscribe_events().
    pub(super) event_channels: Arc<DashMap<ConnectionId, Option<mpsc::Sender<MetricEvent>>>>,
    /// connection_id → receiver side (taken by subscribe_events).
    pub(super) event_receivers: Arc<DashMap<ConnectionId, Option<mpsc::Receiver<MetricEvent>>>>,
    /// connection_id → set of active request_ids (for cancel_pending_for_connection).
    /// Independent DashMap — avoids holding AgentInfo write guard across await points.
    pub(super) connection_requests: Arc<DashMap<ConnectionId, HashSet<String>>>,
    /// request_id → connection_id reverse index — O(1) lookup in resolve_pending,
    /// replacing the former O(N) scan over connection_requests.
    pub(super) request_to_connection: Arc<DashMap<String, ConnectionId>>,
    /// agent_id → {ping_id → waiter} — indexed by unique ping_id to allow safe
    /// per-waiter removal without fragile Vec::pop() under concurrent callers.
    pub(super) pending_pings: Arc<DashMap<String, DashMap<u64, tokio::sync::oneshot::Sender<()>>>>,
    /// Monotonic counter for generating unique ping IDs.
    pub(super) ping_counter: Arc<AtomicU64>,
    pub(super) connection_repo: ConnectionRepositoryRef,
    pub(super) metric_repo: MetricRepositoryRef,
    pub(super) agent_config: Option<detrix_config::AgentConfig>,
    pub(super) system_event_tx: Option<tokio::sync::broadcast::Sender<SystemEvent>>,
    /// Lifecycle manager wired after construction.
    /// Uses tokio::sync::RwLock so .write().await is safe from async callers
    /// and poison errors are not silently swallowed.
    pub(super) adapter_lifecycle_manager:
        Arc<tokio::sync::RwLock<Option<Arc<AdapterLifecycleManager>>>>,
    /// connection_id → last liveness proof timestamp.
    ///
    /// Updated by `record_liveness()` which is called from:
    /// - The heartbeat dispatch handler (for every connection the agent manages).
    /// - `RemoteAdapter::confirm_alive()` (after successful request/response round-trips).
    ///
    /// Read by `liveness_age()` which `RemoteAdapter::ensure_connected()` uses to detect
    /// stale connections without resetting the timer on each call.
    pub(super) liveness_timestamps: Arc<DashMap<ConnectionId, std::time::Instant>>,
    /// Connections currently being started by dispatch — prevents duplicate start_adapter
    /// calls when concurrent ConnectionUpdate(Connected) events arrive.
    pub(super) starting_adapters: Arc<DashSet<ConnectionId>>,
    /// Serializes durable registration and stale-connection cleanup. Without this,
    /// two hosts exposing the same binary can each observe the other's freshly
    /// persisted Disconnected row as stale before routing ownership is installed.
    pub(super) registration_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serializes scanner snapshot reconciliation. The gRPC reader dispatches
    /// messages concurrently; without this lock a removal snapshot and the
    /// forced full snapshot after target close can observe the same old state
    /// and overwrite each other's result.
    pub(super) register_update_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AgentConnectionManager {
    /// Create a new AgentConnectionManager.
    pub fn new(
        connection_repo: ConnectionRepositoryRef,
        metric_repo: MetricRepositoryRef,
        agent_config: Option<detrix_config::AgentConfig>,
        system_event_tx: Option<tokio::sync::broadcast::Sender<SystemEvent>>,
    ) -> Self {
        Self {
            agents: Arc::new(DashMap::new()),
            connection_to_agent: Arc::new(DashMap::new()),
            pending_requests: Arc::new(DashMap::new()),
            event_channels: Arc::new(DashMap::new()),
            event_receivers: Arc::new(DashMap::new()),
            connection_requests: Arc::new(DashMap::new()),
            request_to_connection: Arc::new(DashMap::new()),
            pending_pings: Arc::new(DashMap::new()),
            ping_counter: Arc::new(AtomicU64::new(0)),
            connection_repo,
            metric_repo,
            agent_config,
            system_event_tx,
            adapter_lifecycle_manager: Arc::new(tokio::sync::RwLock::new(None)),
            liveness_timestamps: Arc::new(DashMap::new()),
            starting_adapters: Arc::new(DashSet::new()),
            registration_lock: Arc::new(tokio::sync::Mutex::new(())),
            register_update_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}
