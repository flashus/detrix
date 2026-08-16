//! Versioned raw event envelope. Public MetricEvent compatibility is separate
//! from this private transport ABI.

use crate::capture_plan::{
    CAPTURE_PLAN_SCHEMA_VERSION, MAX_CAPTURE_FIELDS, MAX_CAPTURE_PAYLOAD_BYTES,
};

/// Bound identity fields independently from the payload. Normal profile and
/// plan identifiers are tiny; keeping this cap explicit prevents malformed
/// records from turning the decoder into an allocation sink.
pub const MAX_IDENTITY_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireCapabilities {
    pub supported_schema_versions: Vec<u16>,
    pub supported_profiles: Vec<String>,
    pub max_payload_bytes: usize,
}

impl Default for WireCapabilities {
    fn default() -> Self {
        Self {
            supported_schema_versions: vec![CAPTURE_PLAN_SCHEMA_VERSION],
            // DRX1 is shared by the built-in profiles; profile-specific
            // lowering and decoding remain outside this transport layer.
            supported_profiles: vec!["go".into(), "rust".into()],
            max_payload_bytes: 4096,
        }
    }
}

impl WireCapabilities {
    pub fn negotiate(
        &self,
        schema_version: u16,
        profile_id: &str,
        max_payload_bytes: usize,
    ) -> Result<(), EnvelopeError> {
        if !self.supported_schema_versions.contains(&schema_version) {
            return Err(EnvelopeError::UnknownSchema(schema_version));
        }
        if !self.supported_profiles.iter().any(|p| p == profile_id) {
            return Err(EnvelopeError::UnknownProfile(profile_id.into()));
        }
        if max_payload_bytes > self.max_payload_bytes {
            return Err(EnvelopeError::Oversized {
                size: max_payload_bytes,
                limit: self.max_payload_bytes,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelope {
    pub schema_version: u16,
    pub profile_id: String,
    pub plan_hash: String,
    pub field_count: u16,
    pub payload_len: u32,
    pub partial: bool,
    pub unavailable: bool,
}

impl EventEnvelope {
    pub fn new(
        profile_id: impl Into<String>,
        plan_hash: impl Into<String>,
        field_count: u16,
        payload_len: usize,
    ) -> Self {
        Self {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            profile_id: profile_id.into(),
            plan_hash: plan_hash.into(),
            field_count,
            payload_len: payload_len as u32,
            partial: false,
            unavailable: false,
        }
    }
    pub fn validate(&self, max_payload: usize) -> Result<(), EnvelopeError> {
        self.validate_with_capabilities(&WireCapabilities {
            max_payload_bytes: max_payload,
            ..WireCapabilities::default()
        })
    }

    /// Validate against the capabilities negotiated for a connection.  The
    /// legacy `validate` helper remains for callers that use the default Rust
    /// profile limits, while live adapters can reject a record before profile
    /// decoding when the peer advertises a narrower ABI.
    pub fn validate_with_capabilities(
        &self,
        capabilities: &WireCapabilities,
    ) -> Result<(), EnvelopeError> {
        if self.schema_version != CAPTURE_PLAN_SCHEMA_VERSION {
            return Err(EnvelopeError::UnknownSchema(self.schema_version));
        }
        if self.profile_id.is_empty() || self.plan_hash.is_empty() {
            return Err(EnvelopeError::MissingIdentity);
        }
        if self.profile_id.len() > MAX_IDENTITY_BYTES || self.plan_hash.len() > MAX_IDENTITY_BYTES {
            return Err(EnvelopeError::IdentityTooLong);
        }
        if self.field_count == 0 || usize::from(self.field_count) > MAX_CAPTURE_FIELDS {
            return Err(EnvelopeError::InvalidFieldCount {
                count: usize::from(self.field_count),
                limit: MAX_CAPTURE_FIELDS,
            });
        }
        // Negotiated peers may advertise a narrower limit, but never a wider
        // one: the CapturePlan bound is the transport-wide safety ceiling.
        if self.payload_len as usize > MAX_CAPTURE_PAYLOAD_BYTES {
            return Err(EnvelopeError::Oversized {
                size: self.payload_len as usize,
                limit: MAX_CAPTURE_PAYLOAD_BYTES,
            });
        }
        capabilities.negotiate(
            self.schema_version,
            &self.profile_id,
            self.payload_len as usize,
        )?;
        Ok(())
    }

    /// Encode the negotiated envelope followed by its payload.
    ///
    /// The fixed header is deliberately self-describing so a future decoder
    /// can reject unknown versions before interpreting any fields.
    pub fn encode_record(
        &self,
        payload: &[u8],
        max_payload: usize,
    ) -> Result<Vec<u8>, EnvelopeError> {
        if self.payload_len as usize != payload.len() {
            return Err(EnvelopeError::LengthMismatch {
                declared: self.payload_len as usize,
                actual: payload.len(),
            });
        }
        self.validate(max_payload)?;
        let profile = self.profile_id.as_bytes();
        let plan = self.plan_hash.as_bytes();
        if profile.len() > MAX_IDENTITY_BYTES || plan.len() > MAX_IDENTITY_BYTES {
            return Err(EnvelopeError::IdentityTooLong);
        }
        let mut out = Vec::with_capacity(16 + profile.len() + plan.len() + payload.len());
        out.extend_from_slice(b"DRX1");
        out.extend_from_slice(&self.schema_version.to_le_bytes());
        let mut flags = 0u16;
        if self.partial {
            flags |= 1;
        }
        if self.unavailable {
            flags |= 2;
        }
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&(profile.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.field_count.to_le_bytes());
        out.extend_from_slice(&(plan.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.payload_len.to_le_bytes());
        out.extend_from_slice(profile);
        out.extend_from_slice(plan);
        out.extend_from_slice(payload);
        Ok(out)
    }

    /// Decode a complete live record and enforce its declared lengths.
    pub fn decode_record(
        record: &[u8],
        max_payload: usize,
    ) -> Result<(Self, Vec<u8>), EnvelopeError> {
        const HEADER: usize = 18;
        if record.len() < HEADER || &record[..4] != b"DRX1" {
            return Err(EnvelopeError::MalformedHeader);
        }
        let schema_version = u16::from_le_bytes(record[4..6].try_into().unwrap());
        let flags = u16::from_le_bytes(record[6..8].try_into().unwrap());
        if flags & !0x3 != 0 {
            return Err(EnvelopeError::UnknownFlags(flags));
        }
        let profile_len = u16::from_le_bytes(record[8..10].try_into().unwrap()) as usize;
        let field_count = u16::from_le_bytes(record[10..12].try_into().unwrap());
        let plan_len = u16::from_le_bytes(record[12..14].try_into().unwrap()) as usize;
        let payload_len = u32::from_le_bytes(record[14..18].try_into().unwrap()) as usize;
        if profile_len == 0 || plan_len == 0 {
            return Err(EnvelopeError::MissingIdentity);
        }
        if profile_len > MAX_IDENTITY_BYTES || plan_len > MAX_IDENTITY_BYTES {
            return Err(EnvelopeError::IdentityTooLong);
        }
        let identity_end = HEADER
            .checked_add(profile_len)
            .and_then(|n| n.checked_add(plan_len))
            .ok_or(EnvelopeError::MalformedHeader)?;
        let end = identity_end
            .checked_add(payload_len)
            .ok_or(EnvelopeError::MalformedHeader)?;
        if end != record.len() {
            return Err(EnvelopeError::LengthMismatch {
                declared: end,
                actual: record.len(),
            });
        }
        let profile_id = String::from_utf8(record[HEADER..HEADER + profile_len].to_vec())
            .map_err(|_| EnvelopeError::InvalidIdentity)?;
        let plan_start = HEADER + profile_len;
        let plan_hash = String::from_utf8(record[plan_start..identity_end].to_vec())
            .map_err(|_| EnvelopeError::InvalidIdentity)?;
        let envelope = Self {
            schema_version,
            profile_id,
            plan_hash,
            field_count,
            payload_len: payload_len as u32,
            partial: flags & 1 != 0,
            unavailable: flags & 2 != 0,
        };
        envelope.validate(max_payload)?;
        Ok((envelope, record[identity_end..].to_vec()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("unknown event schema {0}")]
    UnknownSchema(u16),
    #[error("event is missing profile or plan identity")]
    MissingIdentity,
    #[error("event payload {size} exceeds limit {limit}")]
    Oversized { size: usize, limit: usize },
    #[error("event payload length mismatch: declared {declared}, actual {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("malformed event envelope header")]
    MalformedHeader,
    #[error("event profile or plan identity is not valid UTF-8")]
    InvalidIdentity,
    #[error("event profile is not supported: {0}")]
    UnknownProfile(String),
    #[error("event profile or plan identity is too long")]
    IdentityTooLong,
    #[error("event field count {count} is outside the supported range (limit {limit})")]
    InvalidFieldCount { count: usize, limit: usize },
    #[error("event envelope contains unknown flags {0:#x}")]
    UnknownFlags(u16),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_envelope() {
        assert!(EventEnvelope::new("rust", "h", 1, 8).validate(8).is_ok());
    }
    #[test]
    fn rejects_oversized() {
        assert!(matches!(
            EventEnvelope::new("rust", "h", 1, 9).validate(8),
            Err(EnvelopeError::Oversized { .. })
        ));
    }

    #[test]
    fn rejects_oversized_identity_before_allocating_strings() {
        let mut record = vec![
            b'D', b'R', b'X', b'1', // magic
            1, 0, // schema
            0, 0, // flags
            0x01, 0x04, // profile length: 1025
            1, 0, // field count
            1, 0, // plan length
            0, 0, 0, 0, // payload length
        ];
        record.extend(std::iter::repeat(b'r').take(1025));
        record.push(b'h');
        assert_eq!(
            EventEnvelope::decode_record(&record, 64),
            Err(EnvelopeError::IdentityTooLong)
        );
    }

    #[test]
    fn negotiated_capabilities_reject_unknown_profile_and_payload() {
        let envelope = EventEnvelope::new("rust", "h", 1, 8);
        let capabilities = WireCapabilities {
            supported_schema_versions: vec![CAPTURE_PLAN_SCHEMA_VERSION],
            supported_profiles: vec!["go".into()],
            max_payload_bytes: 4,
        };
        assert!(matches!(
            envelope.validate_with_capabilities(&capabilities),
            Err(EnvelopeError::UnknownProfile(_))
        ));

        let envelope = EventEnvelope::new("rust", "h", 1, 8);
        let capabilities = WireCapabilities {
            supported_schema_versions: vec![CAPTURE_PLAN_SCHEMA_VERSION],
            supported_profiles: vec!["rust".into()],
            max_payload_bytes: 4,
        };
        assert!(matches!(
            envelope.validate_with_capabilities(&capabilities),
            Err(EnvelopeError::Oversized { .. })
        ));
    }

    #[test]
    fn round_trips_versioned_live_record() {
        let envelope = EventEnvelope::new("rust", "sha256:plan", 1, 8);
        let record = envelope.encode_record(&42u64.to_le_bytes(), 64).unwrap();
        let (decoded, payload) = EventEnvelope::decode_record(&record, 64).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(payload, 42u64.to_le_bytes());
    }

    #[test]
    fn rejects_unknown_flags_before_payload_decode() {
        let envelope = EventEnvelope::new("rust", "sha256:plan", 1, 8);
        let mut record = envelope.encode_record(&42u64.to_le_bytes(), 64).unwrap();
        record[6..8].copy_from_slice(&0x8000u16.to_le_bytes());
        assert_eq!(
            EventEnvelope::decode_record(&record, 64),
            Err(EnvelopeError::UnknownFlags(0x8000))
        );
    }

    #[test]
    fn schema_one_has_stable_golden_record() {
        let envelope = EventEnvelope::new("rust", "sha256:plan", 1, 8);
        let record = envelope.encode_record(&42u64.to_le_bytes(), 64).unwrap();
        let mut golden = vec![
            b'D', b'R', b'X', b'1', // magic
            1, 0, // schema
            0, 0, // flags
            4, 0, // profile length
            1, 0, // field count
            11, 0, // plan length
            8, 0, 0, 0, // payload length
        ];
        golden.extend_from_slice(b"rustsha256:plan");
        golden.extend_from_slice(&42u64.to_le_bytes());
        assert_eq!(record, golden);
        let (decoded, payload) = EventEnvelope::decode_record(&golden, 64).unwrap();
        assert_eq!(decoded.schema_version, CAPTURE_PLAN_SCHEMA_VERSION);
        assert_eq!(payload, 42u64.to_le_bytes());
    }

    #[test]
    fn schema_one_golden_record_preserves_partial_unavailable_flags() {
        let mut envelope = EventEnvelope::new("rust", "sha256:plan", 1, 1);
        envelope.partial = true;
        envelope.unavailable = true;
        let record = envelope.encode_record(&[0], 64).unwrap();
        assert_eq!(&record[6..8], &[3, 0]);
        let (decoded, payload) = EventEnvelope::decode_record(&record, 64).unwrap();
        assert!(decoded.partial);
        assert!(decoded.unavailable);
        assert_eq!(payload, vec![0]);
    }

    #[test]
    fn rejects_truncated_live_record() {
        let envelope = EventEnvelope::new("rust", "sha256:plan", 1, 8);
        let mut record = envelope.encode_record(&42u64.to_le_bytes(), 64).unwrap();
        record.pop();
        assert!(matches!(
            EventEnvelope::decode_record(&record, 64),
            Err(EnvelopeError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn negotiation_rejects_unknown_profile_before_decode() {
        let capabilities = WireCapabilities::default();
        assert!(matches!(
            capabilities.negotiate(CAPTURE_PLAN_SCHEMA_VERSION, "python", 8),
            Err(EnvelopeError::UnknownProfile(_))
        ));
    }

    #[test]
    fn rejects_zero_or_oversized_field_count() {
        let zero = EventEnvelope::new("rust", "sha256:plan", 0, 8);
        assert!(matches!(
            zero.validate(64),
            Err(EnvelopeError::InvalidFieldCount { count: 0, .. })
        ));

        let oversized = EventEnvelope::new(
            "rust",
            "sha256:plan",
            (MAX_CAPTURE_FIELDS as u16).saturating_add(1),
            8,
        );
        assert!(matches!(
            oversized.validate(64),
            Err(EnvelopeError::InvalidFieldCount { .. })
        ));
    }

    #[test]
    fn negotiated_limit_cannot_exceed_capture_plan_safety_ceiling() {
        let envelope = EventEnvelope::new("rust", "sha256:plan", 1, MAX_CAPTURE_PAYLOAD_BYTES + 1);
        let capabilities = WireCapabilities {
            supported_schema_versions: vec![CAPTURE_PLAN_SCHEMA_VERSION],
            supported_profiles: vec!["rust".into()],
            max_payload_bytes: MAX_CAPTURE_PAYLOAD_BYTES + 4096,
        };
        assert!(matches!(
            envelope.validate_with_capabilities(&capabilities),
            Err(EnvelopeError::Oversized {
                limit: MAX_CAPTURE_PAYLOAD_BYTES,
                ..
            })
        ));
    }
}
