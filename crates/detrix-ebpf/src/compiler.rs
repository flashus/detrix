//! Backend-facing capture compiler contract.
//!
//! Profiles produce `CapturePlan`; architecture/backend implementations compile
//! it later.  Keeping this seam explicit prevents language layouts from being
//! embedded in the current C-source generator.

use crate::capture_plan::{
    CaptureField, CapturePiece, CapturePlan, HeaderKind, PlanError, ReadOp, ValueSemantics,
    CAPTURE_PLAN_SCHEMA_VERSION,
};
use crate::dwarf::types::{
    ProbePoint, Register, ResolvedVariable, TargetArchitecture, VariableLocation, VariableSize,
};
use crate::error::Result as EbpfResult;
use crate::probe::program::{
    generate_bpf_program, generate_bpf_program_from_plan, generate_bpf_program_with_envelope,
    BpfProgram, RawEnvelopeSpec,
};
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
/// language-neutral compiler boundary. Scalar, header, map-pointer, and
/// heap-indirect locations are represented explicitly in `CapturePlan`; the
/// generator remains the compatibility renderer for their established wire
/// semantics.
#[derive(Debug, Clone, Default)]
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
    /// Lower the scalar subset of a resolved probe into the language-neutral
    /// plan consumed by the compiler. Composite locations fail closed here.
    pub fn plan_from_probe(
        probe_point: &ProbePoint,
        plan_hash: impl Into<String>,
    ) -> std::result::Result<CapturePlan, CompileError> {
        let architecture = if probe_point.variables.iter().any(|variable| {
            matches!(
                &variable.location,
                VariableLocation::Register(Register::Arm64(_))
                    | VariableLocation::FrameOffset {
                        register: Register::Arm64(_),
                        ..
                    }
            )
        }) {
            TargetArchitecture::Aarch64
        } else {
            TargetArchitecture::X86_64
        };
        let mut offset = 0usize;
        let mut fields = Vec::with_capacity(probe_point.variables.len());
        for variable in &probe_point.variables {
            let is_blob = matches!(&variable.location, VariableLocation::StackBlob { .. });
            let header = matches!(
                &variable.location,
                VariableLocation::GoString { .. }
                    | VariableLocation::StringHeader { .. }
                    | VariableLocation::GoSlice { .. }
                    | VariableLocation::SliceHeader { .. }
            );
            if (variable.nested_type.is_some() && !is_blob && !header)
                || (!variable.location.is_scalar() && !is_blob && !header)
            {
                return Err(CompileError::UnsupportedLocation(format!(
                    "Rust variable '{}' is not a bounded scalar",
                    variable.name
                )));
            }
            let size = match &variable.location {
                VariableLocation::StackBlob { byte_size, .. } => *byte_size,
                VariableLocation::GoString { .. } | VariableLocation::StringHeader { .. } => 16,
                VariableLocation::GoSlice { .. } | VariableLocation::SliceHeader { .. } => 24,
                _ => variable.size.bytes(),
            };
            if let Some(kind) = header_kind(variable) {
                let base = match &variable.location {
                    VariableLocation::GoString { ptr, .. }
                    | VariableLocation::StringHeader { ptr, .. }
                    | VariableLocation::GoSlice { ptr, .. }
                    | VariableLocation::SliceHeader { ptr, .. } => {
                        let base = header_base_location(ptr, kind);
                        location_to_read_op(&base)?
                    }
                    _ => unreachable!("header_kind only returns true for header locations"),
                };
                fields.push(CaptureField {
                    name: variable.name.clone(),
                    offset,
                    size,
                    op: ReadOp::Header {
                        base: Box::new(base),
                        size,
                        kind,
                    },
                });
                offset = offset.saturating_add(size);
                continue;
            }
            let op = match &variable.location {
                VariableLocation::Register(register) => ReadOp::Register {
                    register: *register,
                    semantics: ValueSemantics::Value,
                },
                VariableLocation::StackOffset { offset } => ReadOp::Stack {
                    offset: *offset,
                    size,
                    semantics: ValueSemantics::Value,
                },
                VariableLocation::FrameOffset { register, offset } => ReadOp::Frame {
                    register: *register,
                    offset: *offset,
                    size,
                    semantics: ValueSemantics::Value,
                },
                VariableLocation::StackBlob { offset, byte_size } => ReadOp::Blob {
                    offset: *offset,
                    size: *byte_size,
                    semantics: ValueSemantics::Value,
                },
                VariableLocation::PiecewiseBlob { .. } => {
                    return Err(CompileError::UnsupportedLocation(format!(
                        "Rust variable '{}' has unsupported piecewise composite location",
                        variable.name
                    )))
                }
                _ => unreachable!("is_scalar checked above"),
            };
            fields.push(CaptureField {
                name: variable.name.clone(),
                offset,
                size,
                op,
            });
            offset = offset.saturating_add(size);
        }
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture,
            profile_id: "rust".into(),
            plan_hash: plan_hash.into(),
            probe_pc: probe_point.pc,
            fields,
            max_payload_bytes: offset,
        };
        plan.validate().map_err(CompileError::InvalidPlan)?;
        Ok(plan)
    }

    pub fn compile_variables(&self, variables: &[ResolvedVariable]) -> EbpfResult<BpfProgram> {
        self.compile_variables_with_envelope(variables, None)
    }

    pub fn compile_variables_with_envelope(
        &self,
        variables: &[ResolvedVariable],
        envelope: Option<RawEnvelopeSpec>,
    ) -> EbpfResult<BpfProgram> {
        let architecture = infer_architecture(variables);
        self.compile_variables_with_envelope_for_arch(variables, envelope, architecture)
    }

    pub fn compile_variables_with_envelope_for_arch(
        &self,
        variables: &[ResolvedVariable],
        envelope: Option<RawEnvelopeSpec>,
        _architecture: TargetArchitecture,
    ) -> EbpfResult<BpfProgram> {
        if variables
            .iter()
            .any(|variable| variable.nested_type.is_some())
        {
            return Err(crate::error::Error::Ebpf(
                "Rust eBPF compiler currently supports scalar variables only".into(),
            ));
        }
        let framed = variables
            .iter()
            .cloned()
            .map(|mut variable| {
                variable.location = frame_location(variable.location);
                variable
            })
            .collect::<Vec<_>>();
        generate_bpf_program_with_envelope(&framed, false, None, None, &self.config, envelope)
    }

    /// Compile a validated Rust capture plan into the existing BPF artifact.
    /// This is the migration seam that lets live Rust probes consume the IR
    /// while preserving the legacy variable API for Go.
    pub fn compile_plan_to_program(&self, plan: &CapturePlan) -> Result<BpfProgram, CompileError> {
        plan.validate().map_err(CompileError::InvalidPlan)?;
        if plan.profile_id != "rust" {
            return Err(CompileError::ProfileMismatch {
                expected: "rust".into(),
                actual: plan.profile_id.clone(),
            });
        }
        self.compile_scalar_variables(plan)
    }

    fn compile_scalar_variables(
        &self,
        plan: &CapturePlan,
    ) -> std::result::Result<BpfProgram, CompileError> {
        generate_bpf_program_from_plan(
            plan,
            false,
            None,
            None,
            &self.config,
            Some(RawEnvelopeSpec {
                profile_tag: profile_tag("rust"),
                plan_tag: plan_tag(&plan.plan_hash),
            }),
        )
        .map_err(|error| CompileError::Backend(error.to_string()))
    }
}

fn infer_architecture(variables: &[ResolvedVariable]) -> TargetArchitecture {
    if variables.iter().any(|variable| {
        matches!(
            variable.location,
            VariableLocation::Register(Register::Arm64(_))
                | VariableLocation::FrameOffset {
                    register: Register::Arm64(_),
                    ..
                }
        )
    }) {
        TargetArchitecture::Aarch64
    } else {
        TargetArchitecture::X86_64
    }
}

fn frame_location(location: VariableLocation) -> VariableLocation {
    match location {
        VariableLocation::StackOffset { offset } => VariableLocation::StackOffset { offset },
        VariableLocation::GoString { ptr, len } | VariableLocation::StringHeader { ptr, len } => {
            VariableLocation::StringHeader {
                ptr: Box::new(frame_location(*ptr)),
                len: Box::new(frame_location(*len)),
            }
        }
        VariableLocation::GoSlice { ptr, len, cap }
        | VariableLocation::SliceHeader { ptr, len, cap } => VariableLocation::SliceHeader {
            ptr: Box::new(frame_location(*ptr)),
            len: Box::new(frame_location(*len)),
            cap: Box::new(frame_location(*cap)),
        },
        other => other,
    }
}

fn header_kind(variable: &ResolvedVariable) -> Option<HeaderKind> {
    match &variable.location {
        VariableLocation::GoString { .. } | VariableLocation::StringHeader { .. }
            if variable.type_name == "&str" || variable.type_name == "&mut str" =>
        {
            Some(HeaderKind::BorrowedStr)
        }
        VariableLocation::GoString { .. } | VariableLocation::StringHeader { .. }
            if variable.type_name == "String"
                || variable.type_name.starts_with("alloc::string::String")
                || variable.type_name.starts_with("std::string::String") =>
        {
            Some(HeaderKind::RustString)
        }
        VariableLocation::GoString { .. } | VariableLocation::StringHeader { .. } => {
            Some(HeaderKind::String)
        }
        VariableLocation::GoSlice { .. } | VariableLocation::SliceHeader { .. }
            if variable.type_name.starts_with("Vec<")
                || variable.type_name.starts_with("alloc::vec::Vec<")
                || variable.type_name.starts_with("std::vec::Vec<") =>
        {
            Some(HeaderKind::RustVec)
        }
        VariableLocation::GoSlice { .. } | VariableLocation::SliceHeader { .. }
            if variable.type_name.starts_with("&[") || variable.type_name.starts_with("&mut [") =>
        {
            Some(HeaderKind::BorrowedSlice)
        }
        VariableLocation::GoSlice { .. } | VariableLocation::SliceHeader { .. } => {
            Some(HeaderKind::Slice)
        }
        _ => None,
    }
}

fn location_to_read_op(location: &VariableLocation) -> Result<ReadOp, CompileError> {
    match location {
        VariableLocation::Register(register) => Ok(ReadOp::Register {
            register: *register,
            semantics: ValueSemantics::Value,
        }),
        VariableLocation::StackOffset { offset } => Ok(ReadOp::Stack {
            offset: *offset,
            size: 8,
            semantics: ValueSemantics::Value,
        }),
        VariableLocation::FrameOffset { register, offset } => Ok(ReadOp::Frame {
            register: *register,
            offset: *offset,
            size: 8,
            semantics: ValueSemantics::Value,
        }),
        other => Err(CompileError::UnsupportedLocation(format!(
            "header base has unsupported location {other:?}"
        ))),
    }
}

pub fn profile_tag(profile: &str) -> u32 {
    profile
        .as_bytes()
        .iter()
        .fold(0x811c_9dc5u32, |hash, byte| {
            hash.wrapping_mul(0x0100_0193) ^ u32::from(*byte)
        })
}

pub fn plan_tag(plan_hash: &str) -> u64 {
    plan_hash
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            hash.wrapping_mul(0x0000_0100_0000_01b3) ^ u64::from(*byte)
        })
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
        let program = self.compile_plan_to_program(plan)?;
        Ok(CompiledCapture {
            plan_hash: plan.plan_hash.clone(),
            architecture: plan.architecture,
            artifact: program.source.into_bytes(),
        })
    }
}

impl GoBpfCompiler {
    /// Lower a Go probe into the shared CapturePlan IR. Map pointers and
    /// heap-indirect values use explicit operations so the policy boundary is
    /// retained even though their established user-space readers remain.
    pub fn plan_from_probe(
        probe_point: &ProbePoint,
        plan_hash: impl Into<String>,
    ) -> std::result::Result<CapturePlan, CompileError> {
        let architecture = infer_architecture(&probe_point.variables);
        let mut offset = 0usize;
        let mut fields = Vec::with_capacity(probe_point.variables.len());
        for variable in &probe_point.variables {
            let (size, op) = match &variable.location {
                VariableLocation::Register(register) => (
                    variable.size.bytes(),
                    ReadOp::Register {
                        register: *register,
                        semantics: ValueSemantics::Value,
                    },
                ),
                VariableLocation::StackOffset { offset: stack } => (
                    variable.size.bytes(),
                    ReadOp::Stack {
                        offset: *stack,
                        size: variable.size.bytes(),
                        semantics: ValueSemantics::Value,
                    },
                ),
                VariableLocation::FrameOffset {
                    register,
                    offset: frame,
                } => (
                    variable.size.bytes(),
                    ReadOp::Frame {
                        register: *register,
                        offset: *frame,
                        size: variable.size.bytes(),
                        semantics: ValueSemantics::Value,
                    },
                ),
                VariableLocation::StackBlob {
                    offset: stack,
                    byte_size,
                } => (
                    *byte_size,
                    ReadOp::Blob {
                        offset: *stack,
                        size: *byte_size,
                        semantics: ValueSemantics::Value,
                    },
                ),
                VariableLocation::StackIndirect {
                    offset: stack,
                    byte_size,
                } => (
                    8,
                    ReadOp::Indirect {
                        base: Box::new(ReadOp::Stack {
                            offset: *stack,
                            size: 8,
                            semantics: ValueSemantics::Address,
                        }),
                        size: *byte_size,
                    },
                ),
                VariableLocation::GoMap { ptr } => (
                    8,
                    ReadOp::Map {
                        base: Box::new(location_to_read_op(ptr)?),
                    },
                ),
                VariableLocation::PiecewiseBlob { pieces, byte_size } => {
                    let mut cursor = 0usize;
                    let mut lowered = Vec::with_capacity(pieces.len());
                    for piece in pieces {
                        let op = piece
                            .location
                            .as_ref()
                            .map(location_to_read_op)
                            .transpose()?;
                        lowered.push(CapturePiece {
                            offset: cursor,
                            size: piece.byte_size,
                            op,
                        });
                        cursor = cursor.saturating_add(piece.byte_size);
                    }
                    (*byte_size, ReadOp::Piecewise { pieces: lowered })
                }
                VariableLocation::GoString { ptr, .. }
                | VariableLocation::StringHeader { ptr, .. } => (
                    16,
                    ReadOp::Header {
                        base: Box::new(location_to_read_op(ptr)?),
                        size: 16,
                        kind: HeaderKind::String,
                    },
                ),
                VariableLocation::GoSlice { ptr, .. }
                | VariableLocation::SliceHeader { ptr, .. } => (
                    24,
                    ReadOp::Header {
                        base: Box::new(location_to_read_op(ptr)?),
                        size: 24,
                        kind: HeaderKind::Slice,
                    },
                ),
            };
            fields.push(CaptureField {
                name: variable.name.clone(),
                offset,
                size,
                op,
            });
            offset = offset.saturating_add(size);
        }
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture,
            profile_id: "go".into(),
            plan_hash: plan_hash.into(),
            probe_pc: probe_point.pc,
            fields,
            max_payload_bytes: offset,
        };
        plan.validate().map_err(CompileError::InvalidPlan)?;
        Ok(plan)
    }

    pub fn compile_variables(&self, variables: &[ResolvedVariable]) -> EbpfResult<BpfProgram> {
        generate_bpf_program(
            variables,
            self.capture_goid,
            self.g_addr_offset,
            self.goid_offset,
            &self.config,
        )
    }

    /// Compile a Go plan with an optional negotiated raw envelope. The
    /// compatibility trait entry point below still emits legacy records when
    /// no envelope has been negotiated.
    pub fn compile_with_envelope(
        &self,
        plan: &CapturePlan,
        envelope: Option<RawEnvelopeSpec>,
    ) -> std::result::Result<CompiledCapture, CompileError> {
        plan.validate().map_err(CompileError::InvalidPlan)?;
        if plan.profile_id != "go" {
            return Err(CompileError::ProfileMismatch {
                expected: "go".into(),
                actual: plan.profile_id.clone(),
            });
        }
        let program = generate_bpf_program_from_plan(
            plan,
            self.capture_goid,
            self.g_addr_offset,
            self.goid_offset,
            &self.config,
            envelope,
        )
        .map_err(|error| CompileError::Backend(error.to_string()))?;
        Ok(CompiledCapture {
            plan_hash: plan.plan_hash.clone(),
            architecture: plan.architecture,
            artifact: program.source.into_bytes(),
        })
    }
}

impl CaptureCompiler for GoBpfCompiler {
    fn compile(&self, plan: &CapturePlan) -> std::result::Result<CompiledCapture, CompileError> {
        self.compile_with_envelope(plan, None)
    }
}

#[allow(dead_code)]
fn plan_field_to_go_variable(
    field: &crate::capture_plan::CaptureField,
) -> std::result::Result<ResolvedVariable, CompileError> {
    let location = match &field.op {
        ReadOp::Header { base, kind, .. } => {
            let base = read_op_to_location(base)?;
            return Ok(ResolvedVariable {
                name: field.name.clone(),
                location: header_location(base, *kind),
                size: VariableSize::QWord,
                type_name: header_type_name(*kind).into(),
                nested_type: None,
            });
        }
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
        ReadOp::Frame {
            register,
            offset,
            size,
            semantics: ValueSemantics::Value,
        } => {
            let size = VariableSize::from_byte_size(*size as u64).ok_or_else(|| {
                CompileError::UnsupportedLocation(format!("{}-byte frame scalar", size))
            })?;
            return Ok(ResolvedVariable {
                name: field.name.clone(),
                location: VariableLocation::FrameOffset {
                    register: *register,
                    offset: *offset,
                },
                size,
                type_name: "scalar".into(),
                nested_type: None,
            });
        }
        ReadOp::Blob {
            offset,
            size,
            semantics: ValueSemantics::Value,
        } => {
            return Ok(ResolvedVariable {
                name: field.name.clone(),
                location: VariableLocation::StackBlob {
                    offset: *offset,
                    byte_size: *size,
                },
                size: VariableSize::QWord,
                type_name: "blob".into(),
                nested_type: None,
            });
        }
        ReadOp::Piecewise { pieces } => {
            let mut legacy = Vec::with_capacity(pieces.len());
            for piece in pieces {
                legacy.push(crate::dwarf::types::VariablePiece {
                    location: piece.op.as_ref().map(read_op_to_location).transpose()?,
                    byte_size: piece.size,
                });
            }
            return Ok(ResolvedVariable {
                name: field.name.clone(),
                location: VariableLocation::PiecewiseBlob {
                    pieces: legacy,
                    byte_size: field.size,
                },
                size: VariableSize::QWord,
                type_name: "piecewise".into(),
                nested_type: None,
            });
        }
        ReadOp::Indirect { base, size } => {
            let base = read_op_to_location(base)?;
            let offset = match base {
                VariableLocation::StackOffset { offset }
                | VariableLocation::FrameOffset { offset, .. } => offset,
                other => {
                    return Err(CompileError::UnsupportedLocation(format!(
                        "indirect base operation {other:?} is not stack-addressable"
                    )))
                }
            };
            return Ok(ResolvedVariable {
                name: field.name.clone(),
                location: VariableLocation::StackIndirect {
                    offset,
                    byte_size: *size,
                },
                size: VariableSize::QWord,
                type_name: "indirect".into(),
                nested_type: None,
            });
        }
        ReadOp::Map { base } => {
            return Ok(ResolvedVariable {
                name: field.name.clone(),
                location: VariableLocation::GoMap {
                    ptr: Box::new(read_op_to_location(base)?),
                },
                size: VariableSize::QWord,
                type_name: "map".into(),
                nested_type: None,
            });
        }
        ReadOp::Register { semantics, .. }
        | ReadOp::Stack { semantics, .. }
        | ReadOp::Frame { semantics, .. }
        | ReadOp::Blob { semantics, .. }
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

#[allow(dead_code)]
fn read_op_to_location(op: &ReadOp) -> Result<VariableLocation, CompileError> {
    match op {
        ReadOp::Register { register, .. } => Ok(VariableLocation::Register(*register)),
        ReadOp::Stack { offset, .. } => Ok(VariableLocation::StackOffset { offset: *offset }),
        ReadOp::Frame {
            register, offset, ..
        } => Ok(VariableLocation::FrameOffset {
            register: *register,
            offset: *offset,
        }),
        other => Err(CompileError::UnsupportedLocation(format!(
            "header base operation {other:?} is not a scalar address"
        ))),
    }
}

fn shift_location(location: VariableLocation, delta: i64) -> VariableLocation {
    match location {
        VariableLocation::StackOffset { offset } => VariableLocation::StackOffset {
            offset: offset.saturating_add(delta),
        },
        VariableLocation::FrameOffset { register, offset } => VariableLocation::FrameOffset {
            register,
            offset: offset.saturating_add(delta),
        },
        other => other,
    }
}

fn header_base_location(location: &VariableLocation, kind: HeaderKind) -> VariableLocation {
    let delta = match kind {
        // The parser has already normalized the live Rust Vec spill to its
        // first pointer word.  Owned String still starts from the inner Vec
        // pointer word and therefore needs the historical -8 adjustment.
        HeaderKind::RustVec => 0,
        HeaderKind::RustString => -8,
        HeaderKind::String | HeaderKind::BorrowedStr => 0,
        HeaderKind::Slice | HeaderKind::BorrowedSlice => 0,
    };
    shift_location(location.clone(), delta)
}

#[allow(dead_code)]
fn header_location(base: VariableLocation, kind: HeaderKind) -> VariableLocation {
    match kind {
        HeaderKind::String => VariableLocation::GoString {
            ptr: Box::new(base.clone()),
            len: Box::new(shift_location(base, 8)),
        },
        HeaderKind::RustString => VariableLocation::StringHeader {
            ptr: Box::new(shift_location(base.clone(), 8)),
            len: Box::new(shift_location(base, 16)),
        },
        HeaderKind::BorrowedStr => VariableLocation::StringHeader {
            ptr: Box::new(base.clone()),
            len: Box::new(shift_location(base, 8)),
        },
        HeaderKind::Slice => VariableLocation::GoSlice {
            ptr: Box::new(base.clone()),
            len: Box::new(shift_location(base.clone(), 8)),
            cap: Box::new(shift_location(base, 16)),
        },
        HeaderKind::RustVec => VariableLocation::SliceHeader {
            ptr: Box::new(base.clone()),
            len: Box::new(shift_location(base.clone(), 16)),
            // rustc may spill the nominal RawVec capacity slot as an
            // unrelated pointer at the selected PC. Keep the capture
            // bounded and semantically safe by aliasing capacity to length.
            cap: Box::new(shift_location(base, 16)),
        },
        HeaderKind::BorrowedSlice => VariableLocation::SliceHeader {
            ptr: Box::new(base.clone()),
            len: Box::new(shift_location(base.clone(), 8)),
            cap: Box::new(shift_location(base, 8)),
        },
    }
}

#[allow(dead_code)]
fn header_type_name(kind: HeaderKind) -> &'static str {
    match kind {
        HeaderKind::String => "String",
        HeaderKind::RustString => "alloc::string::String",
        HeaderKind::BorrowedStr => "&str",
        HeaderKind::Slice => "[]byte",
        HeaderKind::RustVec => "Vec<u8>",
        HeaderKind::BorrowedSlice => "&[u8]",
    }
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
    use crate::capture_plan::{
        CaptureField, HeaderKind, ReadOp, ValueSemantics, CAPTURE_PLAN_SCHEMA_VERSION,
    };
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
    fn go_probe_scalar_uses_capture_plan_boundary() {
        let probe = ProbePoint {
            binary_path: std::path::PathBuf::from("/tmp/go"),
            pc: 0x44,
            symbol_offset: 0x44,
            function_name: "main.main".into(),
            variables: vec![ResolvedVariable {
                name: "counter".into(),
                location: VariableLocation::Register(Register::Rax),
                size: VariableSize::QWord,
                type_name: "int".into(),
                nested_type: None,
            }],
        };
        let plan = GoBpfCompiler::plan_from_probe(&probe, "probe:44").unwrap();
        assert_eq!(plan.profile_id, "go");
        assert_eq!(plan.fields.len(), 1);
        let compiled = GoBpfCompiler::default().compile(&plan).unwrap();
        assert!(String::from_utf8(compiled.artifact)
            .unwrap()
            .contains("ctx->rax"));
    }

    #[test]
    fn go_compiler_can_render_negotiated_drx1_envelope() {
        let probe = ProbePoint {
            binary_path: std::path::PathBuf::from("/tmp/go"),
            pc: 0x47,
            symbol_offset: 0x47,
            function_name: "main.main".into(),
            variables: vec![ResolvedVariable {
                name: "counter".into(),
                location: VariableLocation::Register(Register::Rax),
                size: VariableSize::QWord,
                type_name: "int".into(),
                nested_type: None,
            }],
        };
        let plan = GoBpfCompiler::plan_from_probe(&probe, "probe:47").unwrap();
        let source = String::from_utf8(
            GoBpfCompiler::default()
                .compile_with_envelope(
                    &plan,
                    Some(RawEnvelopeSpec {
                        profile_tag: profile_tag("go"),
                        plan_tag: plan_tag("probe:47"),
                    }),
                )
                .unwrap()
                .artifact,
        )
        .unwrap();
        assert!(source.contains("drx_magic"));
        assert!(source.contains("drx_profile_tag"));
        assert!(source.contains("DRX1") || source.contains("0x31585244"));
    }

    #[test]
    fn go_plan_preserves_indirect_and_map_semantics() {
        let probe = ProbePoint {
            binary_path: std::path::PathBuf::from("/tmp/go"),
            pc: 0x45,
            symbol_offset: 0x45,
            function_name: "main.main".into(),
            variables: vec![
                ResolvedVariable {
                    name: "escaped".into(),
                    location: VariableLocation::StackIndirect {
                        offset: -32,
                        byte_size: 24,
                    },
                    size: VariableSize::QWord,
                    type_name: "struct".into(),
                    nested_type: None,
                },
                ResolvedVariable {
                    name: "lookup".into(),
                    location: VariableLocation::GoMap {
                        ptr: Box::new(VariableLocation::StackOffset { offset: -40 }),
                    },
                    size: VariableSize::QWord,
                    type_name: "map[string]int".into(),
                    nested_type: None,
                },
            ],
        };
        let plan = GoBpfCompiler::plan_from_probe(&probe, "probe:45").unwrap();
        assert!(matches!(
            plan.fields[0].op,
            ReadOp::Indirect { size: 24, .. }
        ));
        assert!(matches!(plan.fields[1].op, ReadOp::Map { .. }));
        let source =
            String::from_utf8(GoBpfCompiler::default().compile(&plan).unwrap().artifact).unwrap();
        assert!(source.contains("DETRIX_STACK_PTR - 32"));
        assert!(source.contains("DETRIX_STACK_PTR - 40"));
    }

    #[test]
    fn go_piecewise_probe_uses_capture_plan_boundary() {
        let probe = ProbePoint {
            binary_path: std::path::PathBuf::from("/tmp/go"),
            pc: 0x46,
            symbol_offset: 0x46,
            function_name: "main.main".into(),
            variables: vec![ResolvedVariable {
                name: "pair".into(),
                location: VariableLocation::PiecewiseBlob {
                    pieces: vec![
                        crate::dwarf::types::VariablePiece {
                            location: Some(VariableLocation::Register(Register::Rax)),
                            byte_size: 8,
                        },
                        crate::dwarf::types::VariablePiece {
                            location: Some(VariableLocation::StackOffset { offset: -16 }),
                            byte_size: 8,
                        },
                    ],
                    byte_size: 16,
                },
                size: VariableSize::QWord,
                type_name: "pair".into(),
                nested_type: None,
            }],
        };
        let plan = GoBpfCompiler::plan_from_probe(&probe, "probe:46").unwrap();
        assert!(matches!(plan.fields[0].op, ReadOp::Piecewise { .. }));
        let source =
            String::from_utf8(GoBpfCompiler::default().compile(&plan).unwrap().artifact).unwrap();
        assert!(source.contains("ctx->rax"));
        assert!(source.contains("DETRIX_STACK_PTR - 16"));
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

    #[test]
    fn rust_probe_lowering_preserves_frame_register_and_identity() {
        let probe = ProbePoint {
            binary_path: std::path::PathBuf::from("/tmp/rust"),
            pc: 0x1234,
            symbol_offset: 0x34,
            function_name: "main".into(),
            variables: vec![ResolvedVariable {
                name: "value".into(),
                location: VariableLocation::FrameOffset {
                    register: Register::Rbp,
                    offset: -16,
                },
                size: VariableSize::QWord,
                type_name: "u64".into(),
                nested_type: None,
            }],
        };
        let plan = RustBpfCompiler::plan_from_probe(&probe, "probe:1234").unwrap();
        assert_eq!(plan.architecture, TargetArchitecture::X86_64);
        assert_eq!(plan.plan_hash, "probe:1234");
        assert!(matches!(
            plan.fields[0].op,
            ReadOp::Frame {
                register: Register::Rbp,
                offset: -16,
                ..
            }
        ));
        let source = RustBpfCompiler::default()
            .compile_plan_to_program(&plan)
            .unwrap()
            .source;
        assert!(source.contains("ctx->rbp"));
    }

    #[test]
    fn rust_plan_uses_declared_aarch64_frame_register_on_non_arm_hosts() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::Aarch64,
            profile_id: "rust".into(),
            plan_hash: "probe:arm64".into(),
            probe_pc: 0x1234,
            fields: vec![CaptureField {
                name: "value".into(),
                offset: 0,
                size: 8,
                op: ReadOp::Frame {
                    register: Register::Arm64(29),
                    offset: -16,
                    size: 8,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 8,
        };
        let source = String::from_utf8(
            RustBpfCompiler::default()
                .compile(&plan)
                .expect("AArch64 plan should compile")
                .artifact,
        )
        .unwrap();
        assert!(source.contains("ctx->regs[29] - 16"));
        assert!(!source.contains("ctx->rbp - 16"));
    }

    #[test]
    fn rust_compiler_renders_bounded_inline_blob_plan() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "rust".into(),
            plan_hash: "probe:blob".into(),
            probe_pc: 0x1234,
            fields: vec![CaptureField {
                name: "request".into(),
                offset: 0,
                size: 16,
                op: ReadOp::Blob {
                    offset: -32,
                    size: 16,
                    semantics: ValueSemantics::Value,
                },
            }],
            max_payload_bytes: 16,
        };
        let source = RustBpfCompiler::default()
            .compile_plan_to_program(&plan)
            .unwrap()
            .source;
        assert!(source.contains("var0_blob[16]"));
        assert!(source.contains("DETRIX_STACK_PTR - 32"));
    }

    #[test]
    fn rust_plan_header_lowers_through_same_compiler_boundary() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "rust".into(),
            plan_hash: "header:rust-vec".into(),
            probe_pc: 7,
            fields: vec![CaptureField {
                name: "values".into(),
                offset: 0,
                size: 24,
                op: ReadOp::Header {
                    base: Box::new(ReadOp::Frame {
                        register: Register::Rbp,
                        offset: -32,
                        size: 8,
                        semantics: ValueSemantics::Value,
                    }),
                    size: 24,
                    kind: HeaderKind::RustVec,
                },
            }],
            max_payload_bytes: 24,
        };
        let source = RustBpfCompiler::default()
            .compile(&plan)
            .expect("header plan should compile")
            .artifact;
        let source = String::from_utf8(source).unwrap();
        assert!(source.contains("var0_len"));
        assert!(source.contains("var0_cap"));
        assert!(source.contains("ctx->rbp - 32"));
        assert_eq!(source.matches("ctx->rbp - 16").count(), 2);
    }
}
