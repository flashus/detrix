//! Language-neutral capture intermediate representation.
//!
//! Profiles lower DWARF locations into this representation.  Probe backends
//! consume it without knowing whether the source language is Go, Rust, or a
//! future profile.

use crate::dwarf::types::{Register, TargetArchitecture};

pub const CAPTURE_PLAN_SCHEMA_VERSION: u16 = 1;
pub const MAX_CAPTURE_FIELDS: usize = 64;
pub const MAX_CAPTURE_PAYLOAD_BYTES: usize = 4096;
pub const MAX_CAPTURE_OP_BYTES: usize = 4096;
pub const MAX_CAPTURE_OP_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSemantics {
    Value,
    Address,
    DerefAddress,
}

/// Bounded header layouts whose pointer words are captured in the event and
/// whose pointed-to bytes are handled by the profile's user-space policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    String,
    RustString,
    BorrowedStr,
    Slice,
    RustVec,
    BorrowedSlice,
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
    /// A stack/frame read relative to the DWARF-selected frame register.
    /// This is distinct from `Stack` because Rust/LLVM commonly uses RBP or
    /// an AArch64 frame register while the uprobe context's SP is different.
    Frame {
        register: Register,
        offset: i64,
        size: usize,
        semantics: ValueSemantics,
    },
    /// Bounded inline bytes copied from a stack/frame location. This is the
    /// first composite operation supported by the Rust profile (structs and
    /// fixed arrays); recursive heap traversal remains out of scope.
    Blob {
        offset: i64,
        size: usize,
        semantics: ValueSemantics,
    },
    /// Capture the pointer stored at `base`; user space may then follow the
    /// bounded pointed-to value using the profile's memory policy.  Keeping
    /// this explicit prevents heap-indirect Go values from being mistaken
    /// for ordinary stack scalars.
    Indirect {
        base: Box<ReadOp>,
        size: usize,
    },
    /// Capture a Go map runtime pointer.  The backend only records the
    /// pointer; map iteration remains a profile-owned user-space operation.
    Map {
        base: Box<ReadOp>,
    },
    /// A bounded pointer/metadata header. `base` identifies the address of
    /// the first word in the target frame; the profile-specific header kind
    /// determines adjacent-word ordering without leaking language names into
    /// the backend renderer.
    Header {
        base: Box<ReadOp>,
        size: usize,
        kind: HeaderKind,
    },
    /// Header whose component words come from explicit DWARF locations.
    /// Rust/LLVM may describe `String`/`Vec` fields with non-adjacent stack
    /// slots, so deriving them from one synthetic base is not sound.
    HeaderExplicit {
        ptr: Box<ReadOp>,
        len: Box<ReadOp>,
        cap: Option<Box<ReadOp>>,
        size: usize,
        kind: HeaderKind,
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
    /// A value reconstructed from bounded pieces. Undefined pieces remain
    /// represented by an unavailable entry instead of being compacted.
    Piecewise {
        pieces: Vec<CapturePiece>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturePiece {
    pub offset: usize,
    pub size: usize,
    pub op: Option<ReadOp>,
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
    /// Compute a deterministic configuration fingerprint from the validated
    /// plan. The explicit plan_hash remains an externally visible identity;
    /// this fingerprint is used by compilers/loaders to detect a plan object
    /// that was mutated after admission.
    pub fn configuration_fingerprint(&self) -> Result<String, PlanError> {
        self.validate()?;
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.schema_version.to_le_bytes());
        hasher.update(format!("{:?}", self.architecture).as_bytes());
        hasher.update(self.profile_id.as_bytes());
        hasher.update(self.plan_hash.as_bytes());
        hasher.update(self.probe_pc.to_le_bytes());
        hasher.update(self.max_payload_bytes.to_le_bytes());
        for field in &self.fields {
            hasher.update(field.name.as_bytes());
            hasher.update(field.offset.to_le_bytes());
            hasher.update(field.size.to_le_bytes());
            hasher.update(format!("{:?}", field.op).as_bytes());
        }
        Ok(format!("sha256:{:x}", hasher.finalize()))
    }

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
        if self.fields.len() > MAX_CAPTURE_FIELDS {
            return Err(PlanError::TooManyFields {
                count: self.fields.len(),
                limit: MAX_CAPTURE_FIELDS,
            });
        }
        if self.max_payload_bytes == 0 || self.max_payload_bytes > MAX_CAPTURE_PAYLOAD_BYTES {
            return Err(PlanError::PayloadLimit {
                required: self.max_payload_bytes,
                limit: MAX_CAPTURE_PAYLOAD_BYTES,
            });
        }
        let mut end = 0usize;
        let mut ranges = Vec::with_capacity(self.fields.len());
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
            let field_end = field
                .offset
                .checked_add(field.size)
                .ok_or(PlanError::TooLarge)?;
            if ranges
                .iter()
                .any(|(start, previous_end)| field.offset < *previous_end && *start < field_end)
            {
                return Err(PlanError::InvalidField(format!(
                    "{} overlaps another field",
                    field.name
                )));
            }
            ranges.push((field.offset, field_end));
            validate_op(&field.op, self.fields.len(), 0, field.size)?;
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
    #[error("capture plan has {count} fields, limit is {limit}")]
    TooManyFields { count: usize, limit: usize },
    #[error("capture plan is too large")]
    TooLarge,
    #[error("payload requires {required} bytes, limit is {limit}")]
    PayloadLimit { required: usize, limit: usize },
    #[error("capture plan has no deterministic hash")]
    MissingPlanHash,
}

fn validate_op(
    op: &ReadOp,
    field_count: usize,
    depth: usize,
    field_size: usize,
) -> Result<(), PlanError> {
    if depth > MAX_CAPTURE_OP_DEPTH {
        return Err(PlanError::InvalidField(
            "read operation nesting exceeds limit".into(),
        ));
    }
    match op {
        ReadOp::Stack { size, .. }
        | ReadOp::Frame { size, .. }
        | ReadOp::Blob { size, .. }
        | ReadOp::Cfa { size, .. } => {
            if *size == 0 || *size > MAX_CAPTURE_OP_BYTES {
                return Err(PlanError::InvalidField(format!(
                    "read size {size} exceeds limit"
                )));
            }
        }
        ReadOp::Indirect { base, size } => {
            if *size == 0 || *size > MAX_CAPTURE_OP_BYTES {
                return Err(PlanError::InvalidField(format!(
                    "read size {size} exceeds limit"
                )));
            }
            // The base is executable capture IR too. Validate it recursively
            // so an attacker cannot hide an unbounded/invalid operation below
            // an otherwise bounded pointer read.
            validate_op(base, field_count, depth + 1, field_size)?;
        }
        ReadOp::Constant { bytes } => {
            if bytes.is_empty() || bytes.len() > MAX_CAPTURE_OP_BYTES {
                return Err(PlanError::InvalidField("invalid constant size".into()));
            }
        }
        ReadOp::UserBytes {
            address_field,
            size,
        } => {
            if *size == 0 || *size > MAX_CAPTURE_OP_BYTES {
                return Err(PlanError::InvalidField(format!(
                    "read size {size} exceeds limit"
                )));
            }
            if usize::from(*address_field) >= field_count {
                return Err(PlanError::InvalidField(
                    "user-byte address field out of range".into(),
                ));
            }
        }
        ReadOp::Piece { offset, size, op } => {
            if *size == 0 || offset.checked_add(*size).is_none() {
                return Err(PlanError::InvalidField("invalid piece bounds".into()));
            }
            if offset + size > field_size {
                return Err(PlanError::InvalidField(
                    "piece extends beyond field payload".into(),
                ));
            }
            validate_op(op, field_count, depth + 1, *size)?;
        }
        ReadOp::Piecewise { pieces } => {
            if pieces.is_empty() {
                return Err(PlanError::InvalidField(
                    "piecewise operation has no pieces".into(),
                ));
            }
            let mut ranges = Vec::with_capacity(pieces.len());
            for piece in pieces {
                if piece.size == 0 || piece.offset.checked_add(piece.size).is_none() {
                    return Err(PlanError::InvalidField("invalid piecewise bounds".into()));
                }
                let end = piece.offset + piece.size;
                if end > field_size {
                    return Err(PlanError::InvalidField(
                        "piecewise piece extends beyond field payload".into(),
                    ));
                }
                if ranges
                    .iter()
                    .any(|(start, previous_end)| piece.offset < *previous_end && *start < end)
                {
                    return Err(PlanError::InvalidField(
                        "piecewise pieces overlap another piece".into(),
                    ));
                }
                ranges.push((piece.offset, end));
                if let Some(op) = &piece.op {
                    validate_op(op, field_count, depth + 1, piece.size)?;
                }
            }
        }
        ReadOp::Header { base, size, .. } => {
            if *size == 0 || *size > MAX_CAPTURE_OP_BYTES {
                return Err(PlanError::InvalidField(format!(
                    "read size {size} exceeds limit"
                )));
            }
            validate_op(base, field_count, depth + 1, *size)?;
        }
        ReadOp::HeaderExplicit {
            ptr,
            len,
            cap,
            size,
            ..
        } => {
            if *size == 0 || *size > MAX_CAPTURE_OP_BYTES {
                return Err(PlanError::InvalidField(format!(
                    "read size {size} exceeds limit"
                )));
            }
            validate_op(ptr, field_count, depth + 1, 8)?;
            validate_op(len, field_count, depth + 1, 8)?;
            if let Some(cap) = cap {
                validate_op(cap, field_count, depth + 1, 8)?;
            }
        }
        ReadOp::Map { base } => validate_op(base, field_count, depth + 1, field_size)?,
        ReadOp::Register { .. } | ReadOp::Unavailable { .. } => {}
    }
    Ok(())
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
        assert_eq!(
            plan().configuration_fingerprint().unwrap(),
            plan().configuration_fingerprint().unwrap()
        );
    }

    #[test]
    fn rejects_overlapping_payload_beyond_limit() {
        let mut p = plan();
        p.fields[0].size = 9;
        assert!(matches!(p.validate(), Err(PlanError::PayloadLimit { .. })));
    }

    #[test]
    fn rejects_overlapping_fields() {
        let mut p = plan();
        p.fields.push(CaptureField {
            name: "other".into(),
            offset: 4,
            size: 4,
            op: ReadOp::Register {
                register: Register::Rbx,
                semantics: ValueSemantics::Value,
            },
        });
        p.max_payload_bytes = 8;
        assert!(
            matches!(p.validate(), Err(PlanError::InvalidField(message)) if message.contains("overlaps"))
        );
    }

    #[test]
    fn rejects_unbounded_nested_reads() {
        let mut p = plan();
        let mut op = ReadOp::Register {
            register: Register::Rax,
            semantics: ValueSemantics::Value,
        };
        for _ in 0..(MAX_CAPTURE_OP_DEPTH + 1) {
            op = ReadOp::Piece {
                offset: 0,
                size: 1,
                op: Box::new(op),
            };
        }
        p.fields[0].op = op;
        assert!(
            matches!(p.validate(), Err(PlanError::InvalidField(message)) if message.contains("nesting"))
        );
    }

    #[test]
    fn rejects_zero_payload_limit() {
        let mut p = plan();
        p.max_payload_bytes = 0;
        assert!(matches!(p.validate(), Err(PlanError::PayloadLimit { .. })));
    }

    #[test]
    fn rejects_piecewise_piece_outside_field() {
        let mut p = plan();
        p.fields[0].op = ReadOp::Piecewise {
            pieces: vec![CapturePiece {
                offset: 4,
                size: 8,
                op: Some(ReadOp::Register {
                    register: Register::Rax,
                    semantics: ValueSemantics::Value,
                }),
            }],
        };
        assert!(matches!(
            p.validate(),
            Err(PlanError::InvalidField(message)) if message.contains("beyond field")
        ));
    }

    #[test]
    fn rejects_invalid_nested_indirect_base() {
        let mut p = plan();
        let mut op = ReadOp::Register {
            register: Register::Rax,
            semantics: ValueSemantics::Value,
        };
        for _ in 0..(MAX_CAPTURE_OP_DEPTH + 1) {
            op = ReadOp::Indirect {
                base: Box::new(op),
                size: 1,
            };
        }
        p.fields[0].op = op;
        assert!(
            matches!(p.validate(), Err(PlanError::InvalidField(message)) if message.contains("nesting"))
        );
    }
}
