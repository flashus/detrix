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
}
