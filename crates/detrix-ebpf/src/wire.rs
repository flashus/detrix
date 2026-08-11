//! Versioned raw event envelope. Public MetricEvent compatibility is separate
//! from this private transport ABI.

use crate::capture_plan::CAPTURE_PLAN_SCHEMA_VERSION;

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
        if self.schema_version != CAPTURE_PLAN_SCHEMA_VERSION {
            return Err(EnvelopeError::UnknownSchema(self.schema_version));
        }
        if self.profile_id.is_empty() || self.plan_hash.is_empty() {
            return Err(EnvelopeError::MissingIdentity);
        }
        if self.payload_len as usize > max_payload {
            return Err(EnvelopeError::Oversized {
                size: self.payload_len as usize,
                limit: max_payload,
            });
        }
        Ok(())
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
}
