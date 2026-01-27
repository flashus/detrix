//! # Detrix Application Layer
//!
//! This crate contains the **Use Cases** (business logic) of the Detrix application,
//! following Clean Architecture principles.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │  Presentation Layer                 │  <- detrix-api, detrix-cli
//! │  (Controllers: gRPC, REST, WS, CLI) │
//! ├─────────────────────────────────────┤
//! │  Application Layer                  │  <- THIS CRATE
//! │  (Use Cases / Services)             │
//! ├─────────────────────────────────────┤
//! │  Infrastructure Layer               │  <- detrix-dap, detrix-storage
//! │  (DAP adapters, Storage)            │
//! ├─────────────────────────────────────┤
//! │  Domain Layer                       │  <- detrix-core
//! │  (Entities, Value Objects, Traits)  │
//! └─────────────────────────────────────┘
//! ```
//!
//! ## Services
//!
//! - [`MetricService`] - All metric management operations (add, update, delete, list, toggle)
//! - [`StreamingService`] - All event streaming operations (query, subscribe, latest values)
//!
//! ## Principles
//!
//! 1. **Protocol-Agnostic**: Services have NO knowledge of gRPC, REST, WebSocket, or CLI
//! 2. **Domain Types Only**: Services accept and return types from `detrix-core`
//! 3. **Reusable**: Same business logic for all presentation layers
//! 4. **Testable**: Services can be tested without any transport protocol
//!
//! ## Usage
//!
//! Controllers (gRPC, REST, WebSocket, CLI) should:
//! 1. Parse/validate incoming DTOs
//! 2. Convert DTOs to domain types
//! 3. Call service methods
//! 4. Convert domain results back to DTOs
//! 5. Return formatted responses
//!
//! Controllers should NEVER contain business logic - only DTO mapping!

// Application layer errors
pub mod error;

// Middleware (decorators for adapters)
pub mod middleware;

// Output ports are in detrix-ports crate
// Re-export for convenience
pub use detrix_ports as ports;

// Safety validation (expression validators)
pub mod safety;

// Protocol-agnostic business logic (Use Cases)
pub mod services;

// Shared application context
pub mod context;

// Re-export for convenience
pub use context::AppContext;
pub use error::{
    // Legacy error wrappers (prefer extension traits for new code)
    CommandErrorWithContext,
    // Typed error sub-enums
    ConfigError,
    // Extension traits for contextual errors
    ConfigIoResultExt,
    // Main types
    Error,
    FileInspectionError,
    FileIoResultExt,
    GroupOperationResult,
    IoErrorWithContext,
    JsonParseErrorWithContext,
    OperationOutcome,
    OperationWarning,
    Result,
    SafetyError,
    SafetyJsonParseError,
};

// Re-export port types from detrix-ports for convenience
pub use detrix_ports::{
    AnchorService, AnchorServiceRef, ConnectionRepository, ConnectionRepositoryRef, DapAdapter,
    DapAdapterFactory, DapAdapterFactoryRef, DapAdapterRef, DlqEntry, DlqEntryStatus,
    DlqRepository, DlqRepositoryRef, ErrorCountRow, EventOutput, EventOutputRef, EventRepository,
    EventRepositoryRef, FileEvent, FileWatcher, FileWatcherConfig, FileWatcherRef, GroupSummary,
    McpUsageRepository, McpUsageRepositoryRef, MetricFilter, MetricRepository, MetricRepositoryRef,
    NullFileWatcher, NullOutput, PurityAnalyzer, PurityAnalyzerRef, PurityCache, PurityCacheEntry,
    PurityCacheKey, PurityCacheRef, PurityCacheStats, RemoveMetricResult, SetMetricResult,
    SourceContext, SymbolInfo, SystemEventRepository, SystemEventRepositoryRef, ToggleMetricResult,
    ToolCountRow, UsageStats,
};

// Thread-safe reference to expression validator (defined here since ExpressionValidator is in this crate)
pub type ExpressionValidatorRef = std::sync::Arc<dyn safety::ExpressionValidator + Send + Sync>;
pub use safety::{ExpressionValidator, PythonValidator, ValidationResult, ValidatorRegistry};
pub use services::{
    AdapterLifecycleManager, AnchorServiceConfig, Audience, CallTimer, CodeContext, CodeLine,
    ConfigService, ConfigUpdateResult, ConnectionService, DefaultAnchorService,
    EnvironmentCheckResult, EnvironmentService, EventCaptureService, FileChangeResult,
    FileInspectionRequest, FileInspectionResult, FileInspectionService, FileOverview,
    JwksValidator, JwtClaims, JwtError, LanguageCapabilities, LineInspectionResult,
    LspSymbolLookup, LspSymbolLookupRef, ManagedAdapterInfo, ManagedAdapterStatus, McpErrorCode,
    McpUsageCounters, McpUsageEvent, McpUsageService, MetricService, NotifyFileWatcher,
    NullLspLookup, SourceLanguage, StreamingService, SystemEventService, TextSearchMatch,
    UsageSnapshot, VariableDefinition, VariableSearchResult, WorkflowStats,
    RESTART_REQUIRED_FIELDS,
};
