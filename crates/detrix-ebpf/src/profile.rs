//! Pluggable language profiles.  Profiles describe types and lowering policy;
//! they do not attach probes or own event forwarding.

use crate::capture_plan::{
    CaptureField, CapturePlan, PlanError, ReadOp, ValueSemantics, CAPTURE_PLAN_SCHEMA_VERSION,
};
use crate::dwarf::types::{Register, TargetArchitecture};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileId {
    Go,
    Rust,
}

impl ProfileId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::Rust => "rust",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeDescriptor {
    Scalar { size: usize },
    Pointer { size: usize },
    Struct { size: usize },
    Array { element_size: usize, count: usize },
    OpaqueBytes { size: usize },
}

pub trait LanguageProfile: Send + Sync {
    fn id(&self) -> ProfileId;
    fn architectures(&self) -> &'static [TargetArchitecture];
    fn classify_type(&self, name: &str, byte_size: usize) -> TypeDescriptor;
    fn scalar_plan(
        &self,
        name: &str,
        pc: u64,
        register: Register,
    ) -> Result<CapturePlan, PlanError> {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: self.id().as_str().into(),
            plan_hash: format!("profile:{}:pc:{pc:x}:{name}", self.id().as_str()),
            probe_pc: pc,
            fields: vec![CaptureField {
                name: name.into(),
                offset: 0,
                size: 8,
                op: ReadOp::Register {
                    register,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 8,
        };
        plan.validate()?;
        Ok(plan)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GoProfile;
#[derive(Debug, Default, Clone, Copy)]
pub struct RustProfile;

const X86_64_ONLY: &[TargetArchitecture] = &[TargetArchitecture::X86_64];

impl LanguageProfile for GoProfile {
    fn id(&self) -> ProfileId {
        ProfileId::Go
    }
    fn architectures(&self) -> &'static [TargetArchitecture] {
        X86_64_ONLY
    }
    fn classify_type(&self, name: &str, byte_size: usize) -> TypeDescriptor {
        if name == "string" {
            TypeDescriptor::OpaqueBytes { size: byte_size }
        } else {
            TypeDescriptor::Scalar { size: byte_size }
        }
    }
}

impl LanguageProfile for RustProfile {
    fn id(&self) -> ProfileId {
        ProfileId::Rust
    }
    fn architectures(&self) -> &'static [TargetArchitecture] {
        X86_64_ONLY
    }
    fn classify_type(&self, name: &str, byte_size: usize) -> TypeDescriptor {
        if name.starts_with('*') || name.starts_with('&') {
            TypeDescriptor::Pointer { size: byte_size }
        } else if name.starts_with('[') {
            TypeDescriptor::Array {
                element_size: 1,
                count: byte_size,
            }
        } else {
            TypeDescriptor::Scalar { size: byte_size }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_have_stable_ids() {
        assert_eq!(GoProfile.id().as_str(), "go");
        assert_eq!(RustProfile.id().as_str(), "rust");
    }
    #[test]
    fn rust_pointer_is_address_capable_descriptor() {
        assert_eq!(
            RustProfile.classify_type("&str", 8),
            TypeDescriptor::Pointer { size: 8 }
        );
    }
}
