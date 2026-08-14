//! Language-neutral decoding for versioned scalar capture records.
//!
//! The legacy Go ring-buffer parser remains unchanged. This decoder is the
//! first Rust-compatible path: it consumes the negotiated envelope and a
//! bounded field layout, so malformed or stale records fail before values are
//! converted into public metric events.

use crate::probe::types::CapturedValue;
use crate::wire::{EnvelopeError, EventEnvelope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Unsigned,
    Signed,
    Bool,
    Float32,
    Float64,
    Address,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarFieldSpec {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub kind: ScalarKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedScalar {
    pub name: String,
    pub value: CapturedValue,
}

/// A bounded inline composite field. The bytes are intentionally opaque at
/// this layer; language profiles may interpret them using DWARF layout, but
/// the wire decoder must enforce bounds before any interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobFieldSpec {
    pub name: String,
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBlob {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Compiler-provided variant metadata for an explicit (non-niche) enum.
/// Unknown discriminants are rejected rather than mapped to a guessed variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariantSpec {
    pub name: String,
    pub discriminant: u64,
    pub payload_offset: Option<usize>,
    pub payload_size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedEnum {
    pub discriminant: u64,
    pub variant: String,
    pub payload: Option<Vec<u8>>,
}

/// Bounded interpretation of an inline composite payload.  The wire layer
/// remains language-neutral; profiles choose one of these layouts only after
/// DWARF has supplied a bounded, compiler-specific representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeKind {
    /// Pointer/length string header. The pointer is returned as an address;
    /// following it is a separate bounded memory-reader step.
    String {
        pointer_offset: usize,
        length_offset: usize,
        word_size: usize,
    },
    /// Pointer/length/capacity header.  The pointer is deliberately returned
    /// as an address; following it is a separate bounded memory-reader step.
    Slice {
        pointer_offset: usize,
        length_offset: usize,
        capacity_offset: usize,
        word_size: usize,
    },
    /// A discriminant at a fixed byte offset.  Variant fields are not guessed
    /// here; a profile may select them only when its DWARF layout is known.
    EnumDiscriminant { offset: usize, size: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecodedComposite {
    String {
        pointer: u64,
        length: u64,
    },
    Slice {
        pointer: u64,
        length: u64,
        capacity: u64,
    },
    EnumDiscriminant(u64),
}

/// Decode an explicit enum discriminant and its selected bounded payload.
/// Niche layouts are intentionally not representable by this API; profiles
/// must supply a real discriminant offset and complete variant table first.
pub fn decode_enum_variant(
    blob: &DecodedBlob,
    discriminant_offset: usize,
    discriminant_size: usize,
    variants: &[EnumVariantSpec],
    max_bytes: usize,
) -> Result<DecodedEnum, ScalarDecodeError> {
    if blob.bytes.len() > max_bytes {
        return Err(ScalarDecodeError::Truncated {
            field: blob.name.clone(),
            end: blob.bytes.len(),
            payload_len: max_bytes,
        });
    }
    if variants.len() < 2 {
        return Err(ScalarDecodeError::IncompleteEnum(blob.name.clone()));
    }
    if !matches!(discriminant_size, 1 | 2 | 4 | 8) {
        return Err(ScalarDecodeError::UnsupportedWidth {
            field: blob.name.clone(),
            size: discriminant_size,
        });
    }
    let end = discriminant_offset
        .checked_add(discriminant_size)
        .ok_or_else(|| ScalarDecodeError::InvalidField(blob.name.clone()))?;
    if end > blob.bytes.len() {
        return Err(ScalarDecodeError::Truncated {
            field: blob.name.clone(),
            end,
            payload_len: blob.bytes.len(),
        });
    }
    let mut word = [0u8; 8];
    word[..discriminant_size].copy_from_slice(&blob.bytes[discriminant_offset..end]);
    let discriminant = u64::from_le_bytes(word);
    let variant = variants
        .iter()
        .find(|variant| variant.discriminant == discriminant)
        .ok_or_else(|| ScalarDecodeError::UnknownEnumDiscriminant {
            field: blob.name.clone(),
            discriminant,
        })?;
    let payload = match (variant.payload_offset, variant.payload_size) {
        (Some(offset), Some(size)) if size > 0 => {
            let payload_end = offset
                .checked_add(size)
                .ok_or_else(|| ScalarDecodeError::InvalidField(blob.name.clone()))?;
            if payload_end > blob.bytes.len() || payload_end > max_bytes {
                return Err(ScalarDecodeError::Truncated {
                    field: blob.name.clone(),
                    end: payload_end,
                    payload_len: blob.bytes.len(),
                });
            }
            Some(blob.bytes[offset..payload_end].to_vec())
        }
        (None, None) => None,
        _ => return Err(ScalarDecodeError::IncompleteEnum(blob.name.clone())),
    };
    Ok(DecodedEnum {
        discriminant,
        variant: variant.name.clone(),
        payload,
    })
}

/// Interpret one bounded blob according to a profile-provided layout.
pub fn decode_composite(
    blob: &DecodedBlob,
    kind: CompositeKind,
    max_bytes: usize,
) -> Result<DecodedComposite, ScalarDecodeError> {
    if blob.bytes.len() > max_bytes {
        return Err(ScalarDecodeError::Truncated {
            field: blob.name.clone(),
            end: blob.bytes.len(),
            payload_len: max_bytes,
        });
    }
    let read_word = |offset: usize, size: usize| -> Result<u64, ScalarDecodeError> {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return Err(ScalarDecodeError::UnsupportedWidth {
                field: blob.name.clone(),
                size,
            });
        }
        let end = offset
            .checked_add(size)
            .ok_or_else(|| ScalarDecodeError::InvalidField(blob.name.clone()))?;
        if end > blob.bytes.len() {
            return Err(ScalarDecodeError::Truncated {
                field: blob.name.clone(),
                end,
                payload_len: blob.bytes.len(),
            });
        }
        let mut bytes = [0u8; 8];
        bytes[..size].copy_from_slice(&blob.bytes[offset..end]);
        Ok(u64::from_le_bytes(bytes))
    };

    match kind {
        CompositeKind::String {
            pointer_offset,
            length_offset,
            word_size,
        } => Ok(DecodedComposite::String {
            pointer: read_word(pointer_offset, word_size)?,
            length: read_word(length_offset, word_size)?.min(max_bytes as u64),
        }),
        CompositeKind::Slice {
            pointer_offset,
            length_offset,
            capacity_offset,
            word_size,
        } => Ok(DecodedComposite::Slice {
            pointer: read_word(pointer_offset, word_size)?,
            length: read_word(length_offset, word_size)?,
            capacity: read_word(capacity_offset, word_size)?,
        }),
        CompositeKind::EnumDiscriminant { offset, size } => {
            Ok(DecodedComposite::EnumDiscriminant(read_word(offset, size)?))
        }
    }
}

pub fn decode_blob_record(
    envelope: &EventEnvelope,
    payload: &[u8],
    fields: &[BlobFieldSpec],
    max_payload: usize,
) -> Result<Vec<DecodedBlob>, ScalarDecodeError> {
    envelope.validate(max_payload)?;
    if envelope.field_count as usize != fields.len() {
        return Err(ScalarDecodeError::FieldCount {
            declared: envelope.field_count as usize,
            actual: fields.len(),
        });
    }
    if envelope.payload_len as usize != payload.len() {
        return Err(ScalarDecodeError::Length {
            declared: envelope.payload_len as usize,
            actual: payload.len(),
        });
    }
    fields
        .iter()
        .map(|field| {
            if field.name.trim().is_empty() || field.size == 0 {
                return Err(ScalarDecodeError::InvalidField(field.name.clone()));
            }
            let end = field
                .offset
                .checked_add(field.size)
                .ok_or_else(|| ScalarDecodeError::InvalidField(field.name.clone()))?;
            if end > payload.len() {
                return Err(ScalarDecodeError::Truncated {
                    field: field.name.clone(),
                    end,
                    payload_len: payload.len(),
                });
            }
            Ok(DecodedBlob {
                name: field.name.clone(),
                bytes: payload[field.offset..end].to_vec(),
            })
        })
        .collect()
}

pub fn decode_scalar_record(
    envelope: &EventEnvelope,
    payload: &[u8],
    fields: &[ScalarFieldSpec],
    max_payload: usize,
) -> Result<Vec<DecodedScalar>, ScalarDecodeError> {
    envelope.validate(max_payload)?;
    if envelope.field_count as usize != fields.len() {
        return Err(ScalarDecodeError::FieldCount {
            declared: envelope.field_count as usize,
            actual: fields.len(),
        });
    }
    if envelope.payload_len as usize != payload.len() {
        return Err(ScalarDecodeError::Length {
            declared: envelope.payload_len as usize,
            actual: payload.len(),
        });
    }

    fields
        .iter()
        .map(|field| {
            if field.name.trim().is_empty() || field.size == 0 {
                return Err(ScalarDecodeError::InvalidField(field.name.clone()));
            }
            let end = field
                .offset
                .checked_add(field.size)
                .ok_or_else(|| ScalarDecodeError::InvalidField(field.name.clone()))?;
            if end > payload.len() {
                return Err(ScalarDecodeError::Truncated {
                    field: field.name.clone(),
                    end,
                    payload_len: payload.len(),
                });
            }
            let bytes = &payload[field.offset..end];
            let value = decode_scalar(field.kind, bytes).ok_or_else(|| {
                ScalarDecodeError::UnsupportedWidth {
                    field: field.name.clone(),
                    size: field.size,
                }
            })?;
            Ok(DecodedScalar {
                name: field.name.clone(),
                value,
            })
        })
        .collect()
}

fn decode_scalar(kind: ScalarKind, bytes: &[u8]) -> Option<CapturedValue> {
    match (kind, bytes.len()) {
        (ScalarKind::Unsigned | ScalarKind::Address, 1) => {
            Some(CapturedValue::Scalar(bytes[0] as u64))
        }
        (ScalarKind::Unsigned | ScalarKind::Address, 2) => Some(CapturedValue::Scalar(
            u16::from_le_bytes(bytes.try_into().ok()?) as u64,
        )),
        (ScalarKind::Unsigned | ScalarKind::Address, 4) => Some(CapturedValue::Scalar(
            u32::from_le_bytes(bytes.try_into().ok()?) as u64,
        )),
        (ScalarKind::Unsigned | ScalarKind::Address, 8) => Some(CapturedValue::Scalar(
            u64::from_le_bytes(bytes.try_into().ok()?),
        )),
        (ScalarKind::Signed, 1) => Some(CapturedValue::Scalar(bytes[0] as i8 as i64 as u64)),
        (ScalarKind::Signed, 2) => Some(CapturedValue::Scalar(i16::from_le_bytes(
            bytes.try_into().ok()?,
        ) as i64 as u64)),
        (ScalarKind::Signed, 4) => Some(CapturedValue::Scalar(i32::from_le_bytes(
            bytes.try_into().ok()?,
        ) as i64 as u64)),
        (ScalarKind::Signed, 8) => Some(CapturedValue::Scalar(i64::from_le_bytes(
            bytes.try_into().ok()?,
        ) as u64)),
        (ScalarKind::Bool, 1) => Some(CapturedValue::Scalar((bytes[0] != 0) as u64)),
        (ScalarKind::Float32, 4) => Some(CapturedValue::Float(f32::from_bits(u32::from_le_bytes(
            bytes.try_into().ok()?,
        )) as f64)),
        (ScalarKind::Float64, 8) => Some(CapturedValue::Float(f64::from_bits(u64::from_le_bytes(
            bytes.try_into().ok()?,
        )))),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScalarDecodeError {
    #[error("invalid event envelope: {0}")]
    Envelope(#[from] EnvelopeError),
    #[error("event field count mismatch: declared {declared}, actual {actual}")]
    FieldCount { declared: usize, actual: usize },
    #[error("event payload length mismatch: declared {declared}, actual {actual}")]
    Length { declared: usize, actual: usize },
    #[error("invalid scalar field {0:?}")]
    InvalidField(String),
    #[error("field {field:?} ends at {end}, payload length is {payload_len}")]
    Truncated {
        field: String,
        end: usize,
        payload_len: usize,
    },
    #[error("unsupported width {size} for scalar field {field:?}")]
    UnsupportedWidth { field: String, size: usize },
    #[error("enum metadata for field {0:?} is incomplete")]
    IncompleteEnum(String),
    #[error("unknown enum discriminant {discriminant} for field {field:?}")]
    UnknownEnumDiscriminant { field: String, discriminant: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rust_scalar_and_address_fields() {
        let envelope = EventEnvelope::new("rust", "sha256:scalar", 2, 16);
        let payload = [7u64.to_le_bytes(), 0x1234u64.to_le_bytes()].concat();
        let fields = vec![
            ScalarFieldSpec {
                name: "counter".into(),
                offset: 0,
                size: 8,
                kind: ScalarKind::Unsigned,
            },
            ScalarFieldSpec {
                name: "ptr".into(),
                offset: 8,
                size: 8,
                kind: ScalarKind::Address,
            },
        ];
        let decoded = decode_scalar_record(&envelope, &payload, &fields, 64).unwrap();
        assert_eq!(decoded[0].value.as_u64(), Some(7));
        assert_eq!(decoded[1].value.as_u64(), Some(0x1234));
    }

    #[test]
    fn rejects_truncated_field_before_conversion() {
        let envelope = EventEnvelope::new("rust", "sha256:scalar", 1, 4);
        let fields = vec![ScalarFieldSpec {
            name: "counter".into(),
            offset: 2,
            size: 4,
            kind: ScalarKind::Unsigned,
        }];
        assert!(matches!(
            decode_scalar_record(&envelope, &[1, 2, 3, 4], &fields, 64),
            Err(ScalarDecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn decodes_bounded_inline_composite_bytes() {
        let envelope = EventEnvelope::new("rust", "sha256:blob", 1, 4);
        let fields = [BlobFieldSpec {
            name: "request".into(),
            offset: 0,
            size: 4,
        }];
        let decoded =
            decode_blob_record(&envelope, &[0xde, 0xad, 0xbe, 0xef], &fields, 64).unwrap();
        assert_eq!(decoded[0].bytes, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn rejects_oversized_or_truncated_composite_before_interpretation() {
        let envelope = EventEnvelope::new("rust", "sha256:blob", 1, 4);
        let fields = [BlobFieldSpec {
            name: "request".into(),
            offset: 1,
            size: 4,
        }];
        assert!(matches!(
            decode_blob_record(&envelope, &[1, 2, 3, 4], &fields, 64),
            Err(ScalarDecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn decodes_bounded_string_header_without_following_pointer() {
        let blob = DecodedBlob {
            name: "name".into(),
            bytes: [0x00, 0x10, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0].to_vec(),
        };
        assert_eq!(
            decode_composite(
                &blob,
                CompositeKind::String {
                    pointer_offset: 0,
                    length_offset: 8,
                    word_size: 8,
                },
                64,
            )
            .unwrap(),
            DecodedComposite::String {
                pointer: 0x1000,
                length: 5,
            }
        );
    }

    #[test]
    fn decodes_slice_header_and_enum_discriminant() {
        let blob = DecodedBlob {
            name: "items".into(),
            bytes: [
                0x00, 0x20, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0,
            ]
            .to_vec(),
        };
        assert_eq!(
            decode_composite(
                &blob,
                CompositeKind::Slice {
                    pointer_offset: 0,
                    length_offset: 8,
                    capacity_offset: 16,
                    word_size: 8,
                },
                64,
            )
            .unwrap(),
            DecodedComposite::Slice {
                pointer: 0x2000,
                length: 3,
                capacity: 8,
            }
        );
        assert_eq!(
            decode_composite(
                &DecodedBlob {
                    name: "state".into(),
                    bytes: vec![0, 0, 2, 0],
                },
                CompositeKind::EnumDiscriminant { offset: 2, size: 1 },
                64,
            )
            .unwrap(),
            DecodedComposite::EnumDiscriminant(2)
        );
    }

    #[test]
    fn rejects_composite_word_outside_blob_or_unsupported_width() {
        let blob = DecodedBlob {
            name: "bad".into(),
            bytes: vec![0; 3],
        };
        assert!(matches!(
            decode_composite(
                &blob,
                CompositeKind::EnumDiscriminant { offset: 0, size: 3 },
                64,
            ),
            Err(ScalarDecodeError::UnsupportedWidth { .. })
        ));
        assert!(matches!(
            decode_composite(
                &blob,
                CompositeKind::EnumDiscriminant { offset: 2, size: 2 },
                64,
            ),
            Err(ScalarDecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn decodes_explicit_enum_variant_and_bounded_payload() {
        let decoded = decode_enum_variant(
            &DecodedBlob {
                name: "state".into(),
                bytes: vec![1, 42, 0, 0],
            },
            0,
            1,
            &[
                EnumVariantSpec {
                    name: "Pending".into(),
                    discriminant: 0,
                    payload_offset: None,
                    payload_size: None,
                },
                EnumVariantSpec {
                    name: "Settled".into(),
                    discriminant: 1,
                    payload_offset: Some(1),
                    payload_size: Some(1),
                },
            ],
            64,
        )
        .unwrap();
        assert_eq!(decoded.variant, "Settled");
        assert_eq!(decoded.payload, Some(vec![42]));
    }

    #[test]
    fn enum_decoder_rejects_unknown_or_incomplete_metadata() {
        let blob = DecodedBlob {
            name: "state".into(),
            bytes: vec![2, 0],
        };
        let variants = [EnumVariantSpec {
            name: "Pending".into(),
            discriminant: 0,
            payload_offset: None,
            payload_size: None,
        }];
        assert!(matches!(
            decode_enum_variant(&blob, 0, 1, &variants, 64),
            Err(ScalarDecodeError::IncompleteEnum(_))
        ));
        let variants = [
            variants[0].clone(),
            EnumVariantSpec {
                name: "Settled".into(),
                discriminant: 1,
                payload_offset: None,
                payload_size: None,
            },
        ];
        assert!(matches!(
            decode_enum_variant(&blob, 0, 1, &variants, 64),
            Err(ScalarDecodeError::UnknownEnumDiscriminant {
                discriminant: 2,
                ..
            })
        ));
    }
}
