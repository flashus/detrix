//! Backend-facing capture compiler contract.
//!
//! Profiles produce `CapturePlan`; architecture/backend implementations compile
//! it later.  Keeping this seam explicit prevents language layouts from being
//! embedded in the current C-source generator.

use crate::capture_plan::{CapturePlan, PlanError};
use crate::dwarf::types::TargetArchitecture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCapture {
    pub plan_hash: String,
    pub architecture: TargetArchitecture,
    pub artifact: Vec<u8>,
}

pub trait CaptureCompiler: Send + Sync {
    fn compile(&self, plan: &CapturePlan) -> Result<CompiledCapture, CompileError>;
}

/// Validation-only compiler used by offline tests and preflight. A real
/// renderer can replace the artifact generation without changing this API.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlanValidatorCompiler;

impl CaptureCompiler for PlanValidatorCompiler {
    fn compile(&self, plan: &CapturePlan) -> Result<CompiledCapture, CompileError> {
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
}
