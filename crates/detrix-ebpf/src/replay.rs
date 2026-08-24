//! Offline raw-event replay fixtures.
//!
//! Replay lets profile/decoder changes be tested without Linux privileges or a
//! live target process. Records are validated against the negotiated envelope
//! before a decoder sees their payload.

use crate::wire::{EnvelopeError, EventEnvelope};
use crate::{
    decode_blob_record, decode_enum_variant, decode_scalar_record, BlobFieldSpec, DecodedBlob,
    DecodedEnum, DecodedScalar, EnumVariantSpec, ScalarDecodeError, ScalarFieldSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRecord {
    pub envelope: EventEnvelope,
    pub payload: Vec<u8>,
}

impl ReplayRecord {
    pub fn validate(&self, max_payload: usize) -> Result<&[u8], ReplayError> {
        self.envelope.validate(max_payload)?;
        if self.envelope.payload_len as usize != self.payload.len() {
            return Err(ReplayError::LengthMismatch {
                declared: self.envelope.payload_len as usize,
                actual: self.payload.len(),
            });
        }
        Ok(&self.payload)
    }

    pub fn decode_scalars(
        &self,
        fields: &[ScalarFieldSpec],
        max_payload: usize,
    ) -> Result<Vec<DecodedScalar>, ReplayError> {
        decode_scalar_record(&self.envelope, &self.payload, fields, max_payload)
            .map_err(ReplayError::Decode)
    }

    pub fn decode_blobs(
        &self,
        fields: &[BlobFieldSpec],
        max_payload: usize,
    ) -> Result<Vec<DecodedBlob>, ReplayError> {
        decode_blob_record(&self.envelope, &self.payload, fields, max_payload)
            .map_err(ReplayError::Decode)
    }

    pub fn decode_enum(
        &self,
        discriminant_offset: usize,
        discriminant_size: usize,
        variants: &[EnumVariantSpec],
        max_payload: usize,
    ) -> Result<DecodedEnum, ReplayError> {
        self.validate(max_payload)?;
        let blob = DecodedBlob {
            name: "enum".into(),
            bytes: self.payload.clone(),
        };
        decode_enum_variant(
            &blob,
            discriminant_offset,
            discriminant_size,
            variants,
            max_payload,
        )
        .map_err(ReplayError::Decode)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplayError {
    #[error("invalid event envelope: {0}")]
    Envelope(#[from] EnvelopeError),
    #[error("replay payload length mismatch: declared {declared}, actual {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("scalar decode failed: {0}")]
    Decode(#[from] ScalarDecodeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_fixture() -> ReplayRecord {
        ReplayRecord {
            envelope: EventEnvelope::new("rust", "sha256:scalar", 1, 8),
            payload: 42u64.to_le_bytes().to_vec(),
        }
    }

    #[test]
    fn scalar_fixture_replays_without_target() {
        assert_eq!(scalar_fixture().validate(64).unwrap(), 42u64.to_le_bytes());
    }

    #[test]
    fn stale_length_is_rejected_before_decode() {
        let mut record = scalar_fixture();
        record.payload.pop();
        assert!(matches!(
            record.validate(64),
            Err(ReplayError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn scalar_fixture_uses_the_shared_decoder() {
        let fields = [ScalarFieldSpec {
            name: "value".into(),
            offset: 0,
            size: 8,
            kind: crate::ScalarKind::Unsigned,
        }];
        let decoded = scalar_fixture().decode_scalars(&fields, 64).unwrap();
        assert_eq!(decoded[0].value.as_u64(), Some(42));
    }

    #[test]
    fn bounded_blob_fixture_replays_without_target() {
        let record = ReplayRecord {
            envelope: EventEnvelope::new("rust", "sha256:blob", 1, 4),
            payload: vec![1, 2, 3, 4],
        };
        let fields = [BlobFieldSpec {
            name: "request".into(),
            offset: 0,
            size: 4,
        }];
        assert_eq!(
            record.decode_blobs(&fields, 64).unwrap()[0].bytes,
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn explicit_enum_fixture_replays_without_target() {
        let record = ReplayRecord {
            envelope: EventEnvelope::new("rust", "sha256:enum", 1, 2),
            payload: vec![1, 42],
        };
        let decoded = record
            .decode_enum(
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
    fn rust_special_layout_fixture_preserves_addresses_and_state() {
        // This is the ABI fixture emitted by fixtures/rust/src/special_layout.rs:
        // a niche Option pointer, a trait-object data/vtable pair, and an
        // explicit state byte.  Replay intentionally does not dereference
        // either pointer, matching the live decoder's safety contract.
        let option_bytes = 0x1234_u64.to_le_bytes();
        let trait_bytes = [0x1111_u64.to_le_bytes(), 0x2222_u64.to_le_bytes()].concat();
        assert_eq!(
            crate::rust_layout::infer("Option<&u64>", 8),
            Some(crate::rust_layout::RustLayoutContract::NicheOption {
                pointer_offset: 0,
                word_size: 8
            })
        );
        assert_eq!(
            crate::rust_layout::infer("&dyn Display", 16),
            Some(crate::rust_layout::RustLayoutContract::TraitObject {
                data_offset: 0,
                vtable_offset: 8,
                word_size: 8
            })
        );
        assert_eq!(
            crate::rust_layout::infer("DetrixAsyncState", 8),
            Some(crate::rust_layout::RustLayoutContract::AsyncState {
                state_offset: 0,
                state_size: 1
            })
        );
        assert_eq!(u64::from_le_bytes(option_bytes), 0x1234);
        assert_eq!(
            u64::from_le_bytes(trait_bytes[0..8].try_into().unwrap()),
            0x1111
        );
        assert_eq!(
            u64::from_le_bytes(trait_bytes[8..16].try_into().unwrap()),
            0x2222
        );
    }
}
