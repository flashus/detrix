//! Pluggable language profiles.  Profiles describe types and lowering policy;
//! they do not attach probes or own event forwarding.

use crate::capture_plan::{
    CaptureField, CapturePlan, PlanError, ReadOp, ValueSemantics, CAPTURE_PLAN_SCHEMA_VERSION,
};
use crate::dwarf::types::{Register, TargetArchitecture, VariableLocation};
use crate::dwarf::{DwarfInfo, TypeInfo};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileCapabilities {
    pub scalar: bool,
    pub pointer: bool,
    pub inline_struct: bool,
    pub fixed_array: bool,
    /// Owned `String` header and bounded user-space string read.
    pub string: bool,
    /// Borrowed `&str` fat-pointer header.
    pub borrowed_str: bool,
    /// Owned `Vec<T>` header and bounded metadata capture.
    pub vector: bool,
    /// Borrowed `&[T]`/`&mut [T]` fat-pointer header.
    pub borrowed_slice: bool,
    pub slice: bool,
    /// Explicit, non-niche enum representation with verified variant metadata.
    pub enumeration: bool,
    /// Compiler-specific niche encodings such as `Option<NonNull<T>>`.
    pub niche_enumeration: bool,
    pub trait_object: bool,
    pub async_state: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeMetadata {
    pub g_addr_offset: Option<i64>,
    pub goid_offset: Option<u64>,
}

pub trait LanguageProfile: Send + Sync {
    /// Stable registry key for this profile. Keeping this as a string makes
    /// third-party profiles additive: a new language does not require
    /// extending the built-in `ProfileId` enum or changing the manager's
    /// construction switches.
    fn id(&self) -> &'static str;
    fn architectures(&self) -> &'static [TargetArchitecture];
    fn capabilities(&self) -> ProfileCapabilities;
    fn classify_type(&self, name: &str, byte_size: usize) -> TypeDescriptor;
    /// Classify a fully resolved DWARF type. Profiles may override this when
    /// compiler metadata (for example explicit enum variants) is required;
    /// the name/size method remains the compatibility fallback for callers
    /// that do not yet have a complete `TypeInfo`.
    fn classify_type_info(&self, type_info: &TypeInfo) -> TypeDescriptor {
        self.classify_type(&type_info.name, type_info.byte_size as usize)
    }
    /// Profile-owned runtime layout metadata. Non-Go profiles return the
    /// default, keeping runtime-specific offsets out of generic attachment.
    fn runtime_metadata(&self, _dwarf: &DwarfInfo, _capture_goid: bool) -> RuntimeMetadata {
        RuntimeMetadata::default()
    }
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
            profile_id: self.id().into(),
            plan_hash: format!("profile:{}:pc:{pc:x}:{name}:{byte_size}", self.id()),
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
            profile_id: self.id().into(),
            plan_hash: format!("profile:{}:pc:{pc:x}:{name}", self.id()),
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

const X86_64_AARCH64: &[TargetArchitecture] =
    &[TargetArchitecture::X86_64, TargetArchitecture::Aarch64];

impl LanguageProfile for GoProfile {
    fn id(&self) -> &'static str {
        "go"
    }
    fn architectures(&self) -> &'static [TargetArchitecture] {
        // The shared uprobe renderer already has x86-64 and AArch64 register
        // mappings; keep Go's profile capability aligned with the backend
        // instead of rejecting arm64 during policy preflight.
        X86_64_AARCH64
    }
    fn capabilities(&self) -> ProfileCapabilities {
        ProfileCapabilities {
            scalar: true,
            pointer: true,
            inline_struct: true,
            fixed_array: true,
            string: true,
            borrowed_str: false,
            vector: false,
            borrowed_slice: false,
            slice: true,
            enumeration: false,
            niche_enumeration: false,
            trait_object: false,
            async_state: false,
        }
    }
    fn classify_type(&self, name: &str, byte_size: usize) -> TypeDescriptor {
        if name == "string" {
            TypeDescriptor::OpaqueBytes { size: byte_size }
        } else {
            TypeDescriptor::Scalar { size: byte_size }
        }
    }

    fn runtime_metadata(&self, dwarf: &DwarfInfo, capture_goid: bool) -> RuntimeMetadata {
        if !capture_goid {
            return RuntimeMetadata::default();
        }
        RuntimeMetadata {
            g_addr_offset: dwarf.g_addr_offset().unwrap_or(None),
            goid_offset: dwarf.goid_field_offset(),
        }
    }
}

impl LanguageProfile for RustProfile {
    fn id(&self) -> &'static str {
        "rust"
    }
    fn architectures(&self) -> &'static [TargetArchitecture] {
        X86_64_AARCH64
    }
    fn capabilities(&self) -> ProfileCapabilities {
        ProfileCapabilities {
            scalar: true,
            pointer: true,
            inline_struct: true,
            fixed_array: true,
            // Rust `String`/`&str` headers have bounded pointer/length
            // lowering and user-space string reads covered by the fixture.
            string: true,
            borrowed_str: true,
            vector: true,
            borrowed_slice: true,
            slice: true,
            // Explicit discriminant enums are covered by the Rust header
            // fixture and bounded decoder; niche layouts remain disabled.
            enumeration: true,
            niche_enumeration: false,
            trait_object: false,
            async_state: false,
        }
    }
    fn classify_type(&self, name: &str, byte_size: usize) -> TypeDescriptor {
        let normalized = name.trim();
        if normalized == "String"
            || normalized == "alloc::string::String"
            || normalized == "std::string::String"
        {
            TypeDescriptor::StringHeader { size: byte_size }
        } else if normalized.starts_with("&[")
            || normalized.starts_with("&mut [")
            || normalized.starts_with("&str")
            || normalized.starts_with("&mut str")
            || normalized.starts_with("slice::")
            || normalized.starts_with("Vec<")
            || normalized.starts_with("alloc::vec::Vec")
            || normalized.starts_with("std::vec::Vec")
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

    fn classify_type_info(&self, type_info: &TypeInfo) -> TypeDescriptor {
        if type_info.is_enum {
            return TypeDescriptor::Enum {
                size: type_info.byte_size as usize,
            };
        }
        if type_info.is_string {
            return TypeDescriptor::StringHeader {
                size: type_info.byte_size as usize,
            };
        }
        if type_info.is_slice {
            return TypeDescriptor::SliceHeader {
                size: type_info.byte_size as usize,
            };
        }
        self.classify_type(&type_info.name, type_info.byte_size as usize)
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
            VariableLocation::FrameOffset { register, offset } => ReadOp::Frame {
                register: *register,
                offset: *offset,
                size: byte_size,
                semantics,
            },
            VariableLocation::StackIndirect { offset, .. } => ReadOp::Stack {
                offset: *offset,
                size: byte_size,
                semantics: ValueSemantics::DerefAddress,
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
            profile_id: self.id().into(),
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
        assert_eq!(GoProfile.id(), "go");
        assert_eq!(RustProfile.id(), "rust");
    }

    #[test]
    fn go_profile_matches_supported_probe_architectures() {
        assert!(GoProfile
            .architectures()
            .contains(&TargetArchitecture::X86_64));
        assert!(GoProfile
            .architectures()
            .contains(&TargetArchitecture::Aarch64));
    }

    #[test]
    fn rust_capabilities_only_advertise_live_categories() {
        let capabilities = RustProfile.capabilities();
        assert!(capabilities.scalar);
        assert!(capabilities.pointer);
        assert!(capabilities.inline_struct);
        assert!(capabilities.fixed_array);
        assert!(capabilities.string);
        assert!(capabilities.borrowed_str);
        assert!(capabilities.vector);
        assert!(capabilities.borrowed_slice);
        assert!(capabilities.slice);
        assert!(capabilities.enumeration);
        assert!(!capabilities.niche_enumeration);
        assert!(!capabilities.trait_object);
        assert!(!capabilities.async_state);
    }
    #[test]
    fn rust_str_is_a_bounded_slice_header() {
        assert_eq!(
            RustProfile.classify_type("&str", 8),
            TypeDescriptor::SliceHeader { size: 8 }
        );
        assert_eq!(
            RustProfile.classify_type("&mut str", 16),
            TypeDescriptor::SliceHeader { size: 16 }
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
        assert!(matches!(
            RustProfile.classify_type("Vec<u64, alloc::alloc::Global>", 24),
            TypeDescriptor::SliceHeader { size: 24 }
        ));
    }

    #[test]
    fn rust_explicit_enum_descriptor_is_advertised_live() {
        assert_eq!(
            RustProfile.classify_type("enum TradeState", 1),
            TypeDescriptor::Enum { size: 1 }
        );
        assert!(RustProfile.capabilities().enumeration);
    }

    #[test]
    fn rust_type_info_classification_prefers_dwarf_shape_over_name() {
        let mut info = TypeInfo::unknown();
        info.name = "TradeState".into();
        info.byte_size = 2;
        info.is_enum = true;
        assert_eq!(
            RustProfile.classify_type_info(&info),
            TypeDescriptor::Enum { size: 2 }
        );
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
    fn rust_scalar_lowering_preserves_frame_register_semantics() {
        let plan = RustProfile
            .lower_scalar(
                "counter",
                0x1234,
                TargetArchitecture::Aarch64,
                &VariableLocation::FrameOffset {
                    register: Register::Arm64(29),
                    offset: -16,
                },
                8,
            )
            .unwrap();
        assert!(matches!(
            plan.fields[0].op,
            ReadOp::Frame {
                register: Register::Arm64(29),
                offset: -16,
                ..
            }
        ));
    }

    #[test]
    fn rust_profile_advertises_only_live_probe_architectures() {
        assert!(RustProfile
            .architectures()
            .contains(&TargetArchitecture::X86_64));
        assert!(RustProfile
            .architectures()
            .contains(&TargetArchitecture::Aarch64));
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
