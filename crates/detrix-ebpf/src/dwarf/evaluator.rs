//! Language-neutral DWARF location-expression lowering.
//!
//! `gimli` remains the only DWARF parser/evaluator.  This module translates
//! the operations it yields into a small semantic IR that distinguishes a
//! value from an address and retains undefined piece widths.  Language
//! profiles may lower this IR into a bounded `CapturePlan` without having to
//! parse DWARF opcodes themselves.

use super::types::{Register, TargetArchitecture};
use crate::error::{Error, Result};
use gimli::{Encoding, Expression, Reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationSemantics {
    Value,
    Address,
    /// The expression computes an address whose pointee must be read. This
    /// is intentionally distinct from `Address`: profiles must not mistake a
    /// DW_OP_deref location for a pointer-valued scalar.
    DerefAddress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationAtom {
    Register(Register),
    RegisterOffset { register: Register, offset: i64 },
    FrameOffset { offset: i64 },
    CfaOffset { offset: i64 },
    Absolute(u64),
    Constant(u64),
    Dereference(Box<LocationAtom>),
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationPiece {
    pub atom: Option<LocationAtom>,
    pub semantics: LocationSemantics,
    pub byte_size: usize,
    /// Byte offset of this piece within the logical value. DWARF piecewise
    /// locations may place pieces out of order; retaining it prevents the
    /// generic layer from silently concatenating them by encounter order.
    pub value_offset: usize,
}

/// Lower one gimli expression into semantic pieces. Unsupported operations are
/// retained as an explicit unavailable piece instead of being silently dropped.
pub fn evaluate_expression<R: Reader>(
    expression: Expression<R>,
    encoding: Encoding,
    architecture: TargetArchitecture,
) -> Result<Vec<LocationPiece>> {
    let mut operations = expression.operations(encoding);
    let mut pieces = Vec::new();
    let mut pending: Option<(LocationAtom, LocationSemantics)> = None;

    while let Some(operation) = operations
        .next()
        .map_err(|error| Error::DwarfParse(format!("DWARF operation: {error}")))?
    {
        match operation {
            gimli::Operation::Register { register } => {
                pending = Some((
                    Register::from_dwarf_for_arch(register.0, architecture)
                        .map(LocationAtom::Register)
                        .unwrap_or_else(|| {
                            LocationAtom::Unavailable(format!(
                                "unsupported DWARF register {}",
                                register.0
                            ))
                        }),
                    LocationSemantics::Value,
                ));
            }
            gimli::Operation::RegisterOffset {
                register, offset, ..
            } => {
                let atom = Register::from_dwarf_for_arch(register.0, architecture)
                    .map(|register| LocationAtom::RegisterOffset { register, offset })
                    .unwrap_or_else(|| {
                        LocationAtom::Unavailable(format!(
                            "unsupported DWARF base register {}",
                            register.0
                        ))
                    });
                pending = Some((atom, LocationSemantics::Address));
            }
            gimli::Operation::FrameOffset { offset } => {
                pending = Some((
                    LocationAtom::FrameOffset { offset },
                    LocationSemantics::Address,
                ));
            }
            gimli::Operation::CallFrameCFA => {
                pending = Some((
                    LocationAtom::CfaOffset { offset: 0 },
                    LocationSemantics::Address,
                ));
            }
            gimli::Operation::Address { address } => {
                pending = Some((LocationAtom::Absolute(address), LocationSemantics::Address));
            }
            gimli::Operation::PlusConstant { value } => {
                pending = Some(match pending.take() {
                    Some((LocationAtom::Register(register), semantics)) => (
                        LocationAtom::RegisterOffset {
                            register,
                            offset: value as i64,
                        },
                        semantics,
                    ),
                    Some((LocationAtom::RegisterOffset { register, offset }, semantics)) => (
                        LocationAtom::RegisterOffset {
                            register,
                            offset: offset.saturating_add(value as i64),
                        },
                        semantics,
                    ),
                    Some((LocationAtom::FrameOffset { offset }, semantics)) => (
                        LocationAtom::FrameOffset {
                            offset: offset.saturating_add(value as i64),
                        },
                        semantics,
                    ),
                    Some((LocationAtom::CfaOffset { offset }, semantics)) => (
                        LocationAtom::CfaOffset {
                            offset: offset.saturating_add(value as i64),
                        },
                        semantics,
                    ),
                    Some((LocationAtom::Absolute(address), semantics)) => (
                        LocationAtom::Absolute(address.saturating_add(value)),
                        semantics,
                    ),
                    Some((atom, semantics)) => (
                        LocationAtom::Unavailable(format!(
                            "DW_OP_plus_uconst cannot apply to {atom:?}"
                        )),
                        semantics,
                    ),
                    None => (
                        LocationAtom::Unavailable(
                            "DW_OP_plus_uconst without a preceding location".into(),
                        ),
                        LocationSemantics::Value,
                    ),
                });
            }
            gimli::Operation::UnsignedConstant { value } => {
                pending = Some((LocationAtom::Constant(value), LocationSemantics::Value));
            }
            gimli::Operation::SignedConstant { value } => {
                pending = Some((
                    LocationAtom::Constant(value as u64),
                    LocationSemantics::Value,
                ));
            }
            gimli::Operation::Deref { .. } => {
                let (atom, _semantics) = pending.take().unwrap_or_else(|| {
                    (
                        LocationAtom::Unavailable("DW_OP_deref without an address".into()),
                        LocationSemantics::Address,
                    )
                });
                pending = Some((
                    LocationAtom::Dereference(Box::new(atom)),
                    LocationSemantics::DerefAddress,
                ));
            }
            gimli::Operation::StackValue => {
                if let Some((atom, semantics)) = pending.as_mut() {
                    *semantics = LocationSemantics::Value;
                    if matches!(atom, LocationAtom::FrameOffset { .. }) {
                        *atom = LocationAtom::Unavailable(
                            "DW_OP_stack_value on a frame address is not a memory location".into(),
                        );
                    }
                }
            }
            gimli::Operation::Piece {
                size_in_bits,
                bit_offset,
            } => {
                let (atom, semantics) = pending.take().unwrap_or_else(|| {
                    (
                        LocationAtom::Unavailable("undefined DWARF piece".into()),
                        LocationSemantics::Value,
                    )
                });
                pieces.push(LocationPiece {
                    atom: Some(atom),
                    semantics,
                    byte_size: (size_in_bits / 8) as usize,
                    value_offset: (bit_offset.unwrap_or(0) / 8) as usize,
                });
            }
            gimli::Operation::ImplicitValue { .. } => {
                pending = Some((
                    LocationAtom::Unavailable("implicit value requires profile bytes".into()),
                    LocationSemantics::Value,
                ));
            }
            gimli::Operation::Nop => {}
            other => {
                pending = Some((
                    LocationAtom::Unavailable(format!("unsupported DWARF operation: {other:?}")),
                    LocationSemantics::Value,
                ));
            }
        }
    }

    if let Some((atom, semantics)) = pending {
        pieces.push(LocationPiece {
            atom: Some(atom),
            semantics,
            byte_size: 0,
            value_offset: 0,
        });
    }
    if pieces.is_empty() {
        return Err(Error::DwarfParse("empty DWARF location expression".into()));
    }
    Ok(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gimli::{EndianSlice, LittleEndian};

    fn encoding() -> Encoding {
        Encoding {
            format: gimli::Format::Dwarf32,
            version: 5,
            address_size: 8,
        }
    }

    fn eval(bytes: &[u8]) -> Vec<LocationPiece> {
        evaluate_expression(
            Expression(EndianSlice::new(bytes, LittleEndian)),
            encoding(),
            TargetArchitecture::X86_64,
        )
        .unwrap()
    }

    #[test]
    fn preserves_register_value_semantics() {
        let pieces = eval(&[0x50]); // DW_OP_reg0
        assert_eq!(pieces[0].semantics, LocationSemantics::Value);
        assert_eq!(pieces[0].atom, Some(LocationAtom::Register(Register::Rax)));
    }

    #[test]
    fn preserves_register_address_and_piece_size() {
        let pieces = eval(&[0x77, 0x70, 0x93, 0x08]); // breg7 -16; piece 8 bytes
        assert_eq!(pieces[0].semantics, LocationSemantics::Address);
        assert_eq!(pieces[0].byte_size, 8);
        assert_eq!(
            pieces[0].atom,
            Some(LocationAtom::RegisterOffset {
                register: Register::Rsp,
                offset: -16
            })
        );
    }

    #[test]
    fn represents_undefined_piece_and_deref() {
        let pieces = eval(&[0x06, 0x93, 0x08]); // deref with no address
        assert!(matches!(
            &pieces[0].atom,
            Some(LocationAtom::Dereference(atom))
                if matches!(atom.as_ref(), LocationAtom::Unavailable(_))
        ));
        assert_eq!(pieces[0].byte_size, 8);
        assert_eq!(pieces[0].semantics, LocationSemantics::DerefAddress);
    }

    #[test]
    fn preserves_cfa_plus_offset() {
        // DW_OP_call_frame_cfa; DW_OP_plus_uconst 16; DW_OP_piece 8.
        let pieces = eval(&[0x9c, 0x23, 0x10, 0x93, 0x08]);
        assert_eq!(pieces[0].semantics, LocationSemantics::Address);
        assert_eq!(pieces[0].byte_size, 8);
        assert_eq!(pieces[0].atom, Some(LocationAtom::CfaOffset { offset: 16 }));
    }

    #[test]
    fn preserves_absolute_address() {
        let mut bytes = vec![0x03]; // DW_OP_addr
        bytes.extend_from_slice(&0x1234u64.to_le_bytes());
        bytes.extend_from_slice(&[0x93, 0x08]);
        let pieces = eval(&bytes);
        assert_eq!(pieces[0].semantics, LocationSemantics::Address);
        assert_eq!(pieces[0].byte_size, 8);
        assert_eq!(pieces[0].atom, Some(LocationAtom::Absolute(0x1234)));
    }

    #[test]
    fn preserves_multiple_and_undefined_pieces() {
        // Register piece followed by an explicitly undefined piece.
        let pieces = eval(&[0x50, 0x93, 0x04, 0x93, 0x04]);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].byte_size, 4);
        assert_eq!(pieces[0].value_offset, 0);
        assert!(matches!(pieces[1].atom, Some(LocationAtom::Unavailable(_))));
        assert_eq!(pieces[1].byte_size, 4);
    }

    #[test]
    fn stack_value_converts_address_to_value() {
        // DW_OP_breg7 -8; DW_OP_stack_value; DW_OP_piece 8.
        let pieces = eval(&[0x77, 0x78, 0x9f, 0x93, 0x08]);
        assert_eq!(pieces[0].semantics, LocationSemantics::Value);
        assert_eq!(pieces[0].value_offset, 0);
    }

    #[test]
    fn rejects_empty_expression() {
        let result = evaluate_expression(
            Expression(EndianSlice::new(&[], LittleEndian)),
            encoding(),
            TargetArchitecture::X86_64,
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_truncated_register_offset_expression() {
        // DW_OP_breg7 requires a signed LEB128 offset.  A truncated operand
        // must be reported as malformed DWARF rather than being converted to
        // an arbitrary stack location.
        let result = evaluate_expression(
            Expression(EndianSlice::new(&[0x77, 0x80], LittleEndian)),
            encoding(),
            TargetArchitecture::X86_64,
        );
        assert!(result.is_err());
    }
}
