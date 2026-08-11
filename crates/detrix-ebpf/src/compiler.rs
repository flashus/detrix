//! Backend-facing capture compiler contract.
//!
//! Profiles produce `CapturePlan`; architecture/backend implementations compile
//! it later.  Keeping this seam explicit prevents language layouts from being
//! embedded in the current C-source generator.

use crate::capture_plan::{CapturePlan, PlanError, ReadOp, ValueSemantics};
use crate::dwarf::types::{ResolvedVariable, TargetArchitecture, VariableLocation, VariableSize};
use crate::error::Result as EbpfResult;
use crate::probe::program::{generate_bpf_program, BpfProgram};
use crate::probe::types::CaptureConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCapture {
    pub plan_hash: String,
    pub architecture: TargetArchitecture,
    pub artifact: Vec<u8>,
}

pub trait CaptureCompiler: Send + Sync {
    fn compile(&self, plan: &CapturePlan) -> std::result::Result<CompiledCapture, CompileError>;
}

/// Compatibility compiler that places the existing Go generator behind the
/// language-neutral compiler boundary. Composite Go locations remain handled
/// by the legacy path until their IR lowering is added; scalar register/stack
/// locations are fully represented by `CapturePlan`.
#[derive(Debug, Clone)]
pub struct GoBpfCompiler {
    pub config: CaptureConfig,
    pub capture_goid: bool,
    pub g_addr_offset: Option<i64>,
    pub goid_offset: Option<u64>,
}

/// Scalar Rust compiler using the same architecture-neutral scalar read
/// renderer as Go. It intentionally rejects composites until the Rust decoder
/// and profile-specific bounded memory policy are enabled.
#[derive(Debug, Clone)]
pub struct RustBpfCompiler {
    pub config: CaptureConfig,
}

impl Default for RustBpfCompiler {
    fn default() -> Self {
        Self {
            config: CaptureConfig {
                capture_goid: false,
                ..CaptureConfig::default()
            },
        }
    }
}

impl RustBpfCompiler {
    pub fn compile_variables(&self, variables: &[ResolvedVariable]) -> EbpfResult<BpfProgram> {
        if variables
            .iter()
            .any(|variable| variable.nested_type.is_some())
        {
            return Err(crate::error::Error::Ebpf(
                "Rust eBPF compiler currently supports scalar variables only".into(),
            ));
        }
        generate_bpf_program(variables, false, None, None, &self.config)
    }

    fn compile_scalar_variables(
        &self,
        plan: &CapturePlan,
    ) -> std::result::Result<BpfProgram, CompileError> {
        let variables = plan
            .fields
            .iter()
            .map(|field| {
                let scalar_field = match &field.op {
                    ReadOp::Register {
                        register,
                        semantics,
                    } if *semantics == ValueSemantics::Value
                        || *semantics == ValueSemantics::Address =>
                    {
                        crate::capture_plan::CaptureField {
                            op: ReadOp::Register {
                                register: *register,
                                semantics: ValueSemantics::Value,
                            },
                            ..field.clone()
                        }
                    }
                    ReadOp::Stack {
                        offset,
                        size,
                        semantics,
                    } if *semantics == ValueSemantics::Value
                        || *semantics == ValueSemantics::Address =>
                    {
                        crate::capture_plan::CaptureField {
                            op: ReadOp::Stack {
                                offset: *offset,
                                size: *size,
                                semantics: ValueSemantics::Value,
                            },
                            ..field.clone()
                        }
                    }
                    _ => {
                        return Err(CompileError::UnsupportedLocation(format!(
                            "Rust operation for '{}' is not a bounded scalar",
                            field.name
                        )))
                    }
                };
                plan_field_to_go_variable(&scalar_field)
            })
            .collect::<std::result::Result<Vec<_>, CompileError>>()?;
        self.compile_variables(&variables)
            .map_err(|error| CompileError::Backend(error.to_string()))
    }
}

impl CaptureCompiler for RustBpfCompiler {
    fn compile(&self, plan: &CapturePlan) -> std::result::Result<CompiledCapture, CompileError> {
        plan.validate().map_err(CompileError::InvalidPlan)?;
        if plan.profile_id != "rust" {
            return Err(CompileError::ProfileMismatch {
                expected: "rust".into(),
                actual: plan.profile_id.clone(),
            });
        }
        let program = self.compile_scalar_variables(plan)?;
        Ok(CompiledCapture {
            plan_hash: plan.plan_hash.clone(),
            architecture: plan.architecture,
            artifact: program.source.into_bytes(),
        })
    }
}

impl Default for GoBpfCompiler {
    fn default() -> Self {
        Self {
            config: CaptureConfig::default(),
            capture_goid: false,
            g_addr_offset: None,
            goid_offset: None,
        }
    }
}

impl GoBpfCompiler {
    pub fn compile_variables(&self, variables: &[ResolvedVariable]) -> EbpfResult<BpfProgram> {
        generate_bpf_program(
            variables,
            self.capture_goid,
            self.g_addr_offset,
            self.goid_offset,
            &self.config,
        )
    }
}

impl CaptureCompiler for GoBpfCompiler {
    fn compile(&self, plan: &CapturePlan) -> std::result::Result<CompiledCapture, CompileError> {
        plan.validate().map_err(CompileError::InvalidPlan)?;
        if plan.profile_id != "go" {
            return Err(CompileError::ProfileMismatch {
                expected: "go".into(),
                actual: plan.profile_id.clone(),
            });
        }
        let variables = plan
            .fields
            .iter()
            .map(plan_field_to_go_variable)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let program = self
            .compile_variables(&variables)
            .map_err(|error| CompileError::Backend(error.to_string()))?;
        Ok(CompiledCapture {
            plan_hash: plan.plan_hash.clone(),
            architecture: plan.architecture,
            artifact: program.source.into_bytes(),
        })
    }
}

fn plan_field_to_go_variable(
    field: &crate::capture_plan::CaptureField,
) -> std::result::Result<ResolvedVariable, CompileError> {
    let location = match &field.op {
        ReadOp::Register {
            register,
            semantics: ValueSemantics::Value,
        } => VariableLocation::Register(*register),
        ReadOp::Stack {
            offset,
            size,
            semantics: ValueSemantics::Value,
        } => {
            let size = VariableSize::from_byte_size(*size as u64).ok_or_else(|| {
                CompileError::UnsupportedLocation(format!("{}-byte stack scalar", size))
            })?;
            return Ok(ResolvedVariable {
                name: field.name.clone(),
                location: VariableLocation::StackOffset { offset: *offset },
                size,
                type_name: "scalar".into(),
                nested_type: None,
            });
        }
        ReadOp::Register { semantics, .. } | ReadOp::Stack { semantics, .. }
            if *semantics != ValueSemantics::Value =>
        {
            return Err(CompileError::UnsupportedLocation(
                "address/deref semantics require a profile-specific lowering".into(),
            ));
        }
        other => {
            return Err(CompileError::UnsupportedLocation(format!(
                "IR operation {other:?} is not supported by the Go compatibility compiler"
            )));
        }
    };
    let size = VariableSize::from_byte_size(field.size as u64).ok_or_else(|| {
        CompileError::UnsupportedLocation(format!("{}-byte register scalar", field.size))
    })?;
    Ok(ResolvedVariable {
        name: field.name.clone(),
        location,
        size,
        type_name: "scalar".into(),
        nested_type: None,
    })
}

/// Validation-only compiler used by offline tests and preflight. A real
/// renderer can replace the artifact generation without changing this API.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlanValidatorCompiler;

impl CaptureCompiler for PlanValidatorCompiler {
    fn compile(&self, plan: &CapturePlan) -> std::result::Result<CompiledCapture, CompileError> {
        plan.validate().map_err(CompileError::InvalidPlan)?;
        let artifact = format!(
            "detrix-capture-plan-v{}:{}:{}:{}",
            plan.schema_version, plan.profile_id, plan.architecture as u8, plan.plan_hash
        )
        .into_bytes();
        Ok(CompiledCapture {
            plan_hash: plan.plan_hash.clone(),
            architecture: plan.architecture,
            artifact,
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("invalid capture plan: {0}")]
    InvalidPlan(PlanError),
    #[error("capture compiler does not support architecture {0:?}")]
    UnsupportedArchitecture(TargetArchitecture),
    #[error("capture program rejected: {0}")]
    Rejected(String),
    #[error("compiler profile mismatch: expected {expected}, got {actual}")]
    ProfileMismatch { expected: String, actual: String },
    #[error("unsupported capture location: {0}")]
    UnsupportedLocation(String),
    #[error("backend compiler error: {0}")]
    Backend(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_plan::{CaptureField, ReadOp, ValueSemantics, CAPTURE_PLAN_SCHEMA_VERSION};
    use crate::dwarf::types::Register;

    #[test]
    fn compiler_preserves_plan_identity() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "rust".into(),
            plan_hash: "sha256:abc".into(),
            probe_pc: 42,
            fields: vec![CaptureField {
                name: "n".into(),
                offset: 0,
                size: 8,
                op: ReadOp::Register {
                    register: Register::Rax,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 8,
        };
        let compiled = PlanValidatorCompiler.compile(&plan).unwrap();
        assert_eq!(compiled.plan_hash, plan.plan_hash);
        assert!(!compiled.artifact.is_empty());
    }

    #[test]
    fn go_compiler_renders_existing_bpf_generator() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "go".into(),
            plan_hash: "sha256:go".into(),
            probe_pc: 7,
            fields: vec![CaptureField {
                name: "n".into(),
                offset: 0,
                size: 8,
                op: ReadOp::Register {
                    register: Register::Rax,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 8,
        };
        let compiled = GoBpfCompiler::default().compile(&plan).unwrap();
        let source = String::from_utf8(compiled.artifact).unwrap();
        assert!(source.contains("event->var0"));
        assert!(source.contains("ctx->rax"));
    }

    #[test]
    fn go_compiler_rejects_rust_plan() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "rust".into(),
            plan_hash: "sha256:rust".into(),
            probe_pc: 7,
            fields: vec![CaptureField {
                name: "n".into(),
                offset: 0,
                size: 8,
                op: ReadOp::Register {
                    register: Register::Rax,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 8,
        };
        assert!(matches!(
            GoBpfCompiler::default().compile(&plan),
            Err(CompileError::ProfileMismatch { .. })
        ));
    }

    #[test]
    fn rust_compiler_renders_bounded_scalar_plan() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "rust".into(),
            plan_hash: "sha256:rust".into(),
            probe_pc: 7,
            fields: vec![CaptureField {
                name: "counter".into(),
                offset: 0,
                size: 8,
                op: ReadOp::Register {
                    register: Register::Rdi,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 8,
        };
        let compiled = RustBpfCompiler::default().compile(&plan).unwrap();
        let source = String::from_utf8(compiled.artifact).unwrap();
        assert!(source.contains("event->var0"));
        assert!(!source.contains("goid"));
    }
}
