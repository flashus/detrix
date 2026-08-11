//! Language-neutral capture intermediate representation.
//!
//! Profiles lower DWARF locations into this representation.  Probe backends
//! consume it without knowing whether the source language is Go, Rust, or a
//! future profile.

use crate::dwarf::types::{Register, TargetArchitecture};

pub const CAPTURE_PLAN_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSemantics {
    Value,
    Address,
    DerefAddress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOp {
    Register {
        register: Register,
        semantics: ValueSemantics,
    },
    Stack {
        offset: i64,
        size: usize,
        semantics: ValueSemantics,
    },
    Cfa {
        offset: i64,
        size: usize,
        semantics: ValueSemantics,
    },
    UserBytes {
        address_field: u16,
        size: usize,
    },
    Constant {
        bytes: Vec<u8>,
    },
    Piece {
        offset: usize,
        size: usize,
        op: Box<ReadOp>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureField {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub op: ReadOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturePlan {
    pub schema_version: u16,
    pub architecture: TargetArchitecture,
    pub profile_id: String,
    pub plan_hash: String,
    pub probe_pc: u64,
    pub fields: Vec<CaptureField>,
    pub max_payload_bytes: usize,
}

impl CapturePlan {
    pub fn validate(&self) -> Result<(), PlanError> {
        if self.schema_version != CAPTURE_PLAN_SCHEMA_VERSION {
            return Err(PlanError::UnsupportedSchema(self.schema_version));
        }
        if self.profile_id.trim().is_empty() {
            return Err(PlanError::MissingProfile);
        }
        if self.fields.is_empty() {
            return Err(PlanError::EmptyFields);
        }
        let mut end = 0usize;
        for field in &self.fields {
            if field.name.trim().is_empty() {
                return Err(PlanError::InvalidField("empty field name".into()));
            }
            if field.size == 0 {
                return Err(PlanError::InvalidField(format!(
                    "{} has zero size",
                    field.name
                )));
            }
            end = end.max(
                field
                    .offset
                    .checked_add(field.size)
                    .ok_or(PlanError::TooLarge)?,
            );
        }
        if end > self.max_payload_bytes {
            return Err(PlanError::PayloadLimit {
                required: end,
                limit: self.max_payload_bytes,
            });
        }
        if self.plan_hash.trim().is_empty() {
            return Err(PlanError::MissingPlanHash);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("unsupported capture plan schema {0}")]
    UnsupportedSchema(u16),
    #[error("capture plan has no profile")]
    MissingProfile,
    #[error("capture plan has no fields")]
    EmptyFields,
    #[error("invalid capture field: {0}")]
    InvalidField(String),
    #[error("capture plan is too large")]
    TooLarge,
    #[error("payload requires {required} bytes, limit is {limit}")]
    PayloadLimit { required: usize, limit: usize },
    #[error("capture plan has no deterministic hash")]
    MissingPlanHash,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> CapturePlan {
        CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "rust".into(),
            plan_hash: "sha256:test".into(),
            probe_pc: 0x1000,
            fields: vec![CaptureField {
                name: "value".into(),
                offset: 0,
                size: 8,
                op: ReadOp::Register {
                    register: Register::Rax,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 8,
        }
    }

    #[test]
    fn validates_language_neutral_scalar_plan() {
        assert!(plan().validate().is_ok());
    }

    #[test]
    fn rejects_overlapping_payload_beyond_limit() {
        let mut p = plan();
        p.fields[0].size = 9;
        assert!(matches!(p.validate(), Err(PlanError::PayloadLimit { .. })));
    }
}
