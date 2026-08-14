//! # detrix-ebpf — eBPF logpoint adapter for Go and Rust scalar capture on Linux
//!
//! Provides zero-pause variable capture at arbitrary source lines using
//! eBPF uprobes instead of the Debug Adapter Protocol (DAP).
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     EbpfAdapter                                │
//! │                  (impl DapAdapter)                             │
//! │                                                                │
//! │  set_metric("order_amount", "main.go#127", ["amount"])         │
//! │       │                                                        │
//! │       ▼                                                        │
//! │  ┌─────────────┐     ┌──────────────┐     ┌────────────────┐  │
//! │  │ dwarf/       │────►│ probe/       │────►│ Ring Buffer    │  │
//! │  │  parser      │     │  program.rs  │     │  → MetricEvent │  │
//! │  │  types       │     │  uprobe.rs   │     │                │  │
//! │  │              │     │  ringbuf.rs  │     │                │  │
//! │  └─────────────┘     └──────────────┘     └────────────────┘  │
//! │  file:line → PC       BPF C codegen       parse raw events    │
//! │  + var locations      attach uprobe        → ExpressionValue  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Platform Support
//!
//! - **DWARF parsing** (gimli/object): Cross-platform, testable on macOS.
//! - **BPF program generation**: Cross-platform (generates C source).
//! - **Uprobe attachment** (aya): Linux-only, gated by `cfg(target_os = "linux")`.
//!
//! ## Usage
//!
//! On Linux, use `EbpfAdapterFactory` to create adapters for Go binaries.
//! On macOS, continue using DAP/Delve via `DapAdapterFactory`.

pub mod adapter;
pub mod capture_plan;
pub mod compiler;
pub mod debug_image;
pub mod decode;
pub mod dwarf;
pub mod error;
pub mod factory;
pub mod mem_reader;
pub mod pc_selection;
pub mod policy;
pub mod probe;
pub mod profile;
pub mod registry;
pub mod replay;
pub mod runtime;
pub mod wire;

pub use adapter::EbpfAdapter;
pub use compiler::{
    plan_tag, profile_tag, CaptureCompiler, CompileError, CompiledCapture, GoBpfCompiler,
    PlanValidatorCompiler, RustBpfCompiler,
};
pub use debug_image::{
    DebugImageError, DebugImageMetadata, DebugImageProvider, DebugImageSource,
    EmbeddedDebugImageProvider, ExternalDebugImageProvider, TargetAbi,
};
pub use decode::{
    decode_blob_record, decode_composite, decode_enum_variant, decode_scalar_record, BlobFieldSpec,
    CompositeKind, DecodedBlob, DecodedComposite, DecodedEnum, DecodedScalar, EnumVariantSpec,
    ScalarDecodeError, ScalarFieldSpec, ScalarKind,
};
pub use dwarf::{EnumLayout, EnumVariantLayout, ProbePcCandidate, ProbeResolutionDiagnostics};
pub use factory::{EbpfAdapterFactory, EbpfGoFactory};
pub use policy::{resolve_backend, BackendDecision, CaptureBackend, PreflightError};
pub use probe::types::CaptureConfig;
pub use profile::{
    GoProfile, LanguageProfile, ProfileCapabilities, ProfileError, ProfileId, RuntimeMetadata,
    RustProfile,
};
pub use registry::{
    BackendRegistry, CaptureBackendFactory, GoEbpfBackend, ProfileRegistry, RustEbpfBackend,
};
pub use replay::{ReplayError, ReplayRecord};
pub use runtime::{ProfiledCaptureRuntime, RuntimeCounters, RuntimeError, RuntimeState};
pub use wire::{EventEnvelope, WireCapabilities};
