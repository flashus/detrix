//! Pluggable language profiles.  Profiles describe types and lowering policy;
//! they do not attach probes or own event forwarding.

use crate::capture_plan::{
    CaptureField, CapturePlan, PlanError, ReadOp, ValueSemantics, CAPTURE_PLAN_SCHEMA_VERSION,
};
use crate::dwarf::types::{Register, TargetArchitecture, VariableLocation};

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
    StringHeader { size: usize },
    SliceHeader { size: usize },
    Struct { size: usize },
    Array { element_size: usize, count: usize },
    Enum { size: usize },
    OpaqueBytes { size: usize },
}

pub trait LanguageProfile: Send + Sync {
    fn id(&self) -> ProfileId;
    fn architectures(&self) -> &'static [TargetArchitecture];
    fn classify_type(&self, name: &str, byte_size: usize) -> TypeDescriptor;
    fn lower_scalar(
        &self,
        name: &str,
        pc: u64,
        architecture: TargetArchitecture,
        location: &VariableLocation,
        byte_size: usize,
    ) -> Result<CapturePlan, ProfileError> {
        if !self.architectures().contains(&architecture) {
            return Err(ProfileError::UnsupportedArchitecture(architecture));
        }
        let op = match location {
            VariableLocation::Register(register) => ReadOp::Register {
                register: *register,
                semantics: ValueSemantics::Value,
            },
            VariableLocation::StackOffset { offset } => ReadOp::Stack {
                offset: *offset,
                size: byte_size,
                semantics: ValueSemantics::Value,
            },
            other => {
                return Err(ProfileError::UnsupportedLocation(format!(
                    "{other:?} is not a scalar location"
                )))
            }
        };
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture,
            profile_id: self.id().as_str().into(),
            plan_hash: format!(
                "profile:{}:pc:{pc:x}:{name}:{byte_size}",
                self.id().as_str()
            ),
            probe_pc: pc,
            fields: vec![CaptureField {
                name: name.into(),
                offset: 0,
                size: byte_size,
                op,
            }],
            max_payload_bytes: byte_size,
        };
        plan.validate().map_err(ProfileError::InvalidPlan)?;
        Ok(plan)
    }
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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("profile does not support architecture {0:?}")]
    UnsupportedArchitecture(TargetArchitecture),
    #[error("unsupported profile location: {0}")]
    UnsupportedLocation(String),
    #[error("invalid lowered capture plan: {0}")]
    InvalidPlan(PlanError),
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
        let normalized = name.trim();
        if normalized == "String" || normalized == "alloc::string::String" {
            TypeDescriptor::StringHeader { size: byte_size }
        } else if normalized.starts_with("&[")
            || normalized.starts_with("&str")
            || normalized.starts_with("slice::")
            || normalized.starts_with("alloc::vec::Vec")
        {
            TypeDescriptor::SliceHeader { size: byte_size }
        } else if normalized.starts_with('*') || normalized.starts_with('&') {
            TypeDescriptor::Pointer { size: byte_size }
        } else if normalized.starts_with('[') {
            TypeDescriptor::Array {
                element_size: 1,
                count: byte_size,
            }
        } else if normalized.starts_with("enum ") {
            TypeDescriptor::Enum { size: byte_size }
        } else {
            TypeDescriptor::Scalar { size: byte_size }
        }
    }

    fn lower_scalar(
        &self,
        name: &str,
        pc: u64,
        architecture: TargetArchitecture,
        location: &VariableLocation,
        byte_size: usize,
    ) -> Result<CapturePlan, ProfileError> {
        let semantics = if name.trim_start().starts_with('*') || name.trim_start().starts_with('&')
        {
            ValueSemantics::Address
        } else {
            ValueSemantics::Value
        };
        if !self.architectures().contains(&architecture) {
            return Err(ProfileError::UnsupportedArchitecture(architecture));
        }
        let op = match location {
            VariableLocation::Register(register) => ReadOp::Register {
                register: *register,
                semantics,
            },
            VariableLocation::StackOffset { offset } => ReadOp::Stack {
                offset: *offset,
                size: byte_size,
                semantics,
            },
            other => {
                return Err(ProfileError::UnsupportedLocation(format!(
                    "{other:?} is not a Rust scalar/pointer location"
                )))
            }
        };
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture,
            profile_id: self.id().as_str().into(),
            plan_hash: format!("profile:rust:pc:{pc:x}:{name}:{byte_size}"),
            probe_pc: pc,
            fields: vec![CaptureField {
                name: name.into(),
                offset: 0,
                size: byte_size,
                op,
            }],
            max_payload_bytes: byte_size,
        };
        plan.validate().map_err(ProfileError::InvalidPlan)?;
        Ok(plan)
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
    fn rust_str_is_a_bounded_slice_header() {
        assert_eq!(
            RustProfile.classify_type("&str", 8),
            TypeDescriptor::SliceHeader { size: 8 }
        );
    }

    #[test]
    fn rust_pointer_lowering_preserves_address_semantics() {
        let plan = RustProfile
            .lower_scalar(
                "&str",
                0x1234,
                TargetArchitecture::X86_64,
                &VariableLocation::Register(Register::Rdi),
                8,
            )
            .unwrap();
        assert!(matches!(
            plan.fields[0].op,
            ReadOp::Register {
                semantics: ValueSemantics::Address,
                ..
            }
        ));
    }

    #[test]
    fn rust_common_headers_have_bounded_descriptors() {
        assert!(matches!(
            RustProfile.classify_type("String", 24),
            TypeDescriptor::StringHeader { size: 24 }
        ));
        assert!(matches!(
            RustProfile.classify_type("&[u8]", 16),
            TypeDescriptor::SliceHeader { size: 16 }
        ));
    }

    #[test]
    fn rust_scalar_lowering_preserves_stack_semantics() {
        let plan = RustProfile
            .lower_scalar(
                "counter",
                0x1234,
                TargetArchitecture::X86_64,
                &VariableLocation::StackOffset { offset: -16 },
                8,
            )
            .unwrap();
        assert_eq!(plan.profile_id, "rust");
        assert!(matches!(
            plan.fields[0].op,
            ReadOp::Stack { offset: -16, .. }
        ));
    }

    #[test]
    fn rust_scalar_lowering_rejects_composites() {
        let error = RustProfile
            .lower_scalar(
                "value",
                1,
                TargetArchitecture::X86_64,
                &VariableLocation::GoString {
                    ptr: Box::new(VariableLocation::Register(Register::Rax)),
                    len: Box::new(VariableLocation::Register(Register::Rdx)),
                },
                16,
            )
            .unwrap_err();
        assert!(matches!(error, ProfileError::UnsupportedLocation(_)));
    }
}
