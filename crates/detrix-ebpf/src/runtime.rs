//! Profile-aware capture runtime lifecycle and live event decoding.
//!
//! Probe attachment remains backend-specific, but lifecycle and wire
//! validation are shared. This prevents Rust support from duplicating the Go
//! adapter's state and from accepting records for a stale plan.

use crate::decode::{decode_scalar_record, DecodedScalar, ScalarFieldSpec};
use crate::wire::{EnvelopeError, EventEnvelope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Created,
    Prepared,
    Attached,
    Active,
    Draining,
    Detached,
    Failed,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCounters {
    pub records_decoded: u64,
    pub decode_drops: u64,
    pub unavailable_fields: u64,
}

pub struct ProfiledCaptureRuntime {
    profile_id: String,
    plan_hash: String,
    fields: Vec<ScalarFieldSpec>,
    max_payload: usize,
    state: RuntimeState,
    counters: RuntimeCounters,
}

impl ProfiledCaptureRuntime {
    pub fn new(
        profile_id: impl Into<String>,
        plan_hash: impl Into<String>,
        fields: Vec<ScalarFieldSpec>,
        max_payload: usize,
    ) -> Result<Self, RuntimeError> {
        let profile_id = profile_id.into();
        let plan_hash = plan_hash.into();
        if profile_id.trim().is_empty() || plan_hash.trim().is_empty() {
            return Err(RuntimeError::MissingIdentity);
        }
        if fields.is_empty() {
            return Err(RuntimeError::EmptyPlan);
        }
        Ok(Self {
            profile_id,
            plan_hash,
            fields,
            max_payload,
            state: RuntimeState::Created,
            counters: RuntimeCounters::default(),
        })
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }
    pub fn counters(&self) -> RuntimeCounters {
        self.counters
    }

    /// Build the versioned record that the live decoder consumes. The Linux
    /// legacy Go ring-buffer path can use this bridge while Rust migrates to
    /// the envelope-native probe template.
    pub fn encode_payload(&self, payload: &[u8], partial: bool) -> Result<Vec<u8>, RuntimeError> {
        let mut envelope = EventEnvelope::new(
            self.profile_id.clone(),
            self.plan_hash.clone(),
            self.fields.len() as u16,
            payload.len(),
        );
        envelope.partial = partial;
        Ok(envelope.encode_record(payload, self.max_payload)?)
    }

    pub fn prepare(&mut self) -> Result<(), RuntimeError> {
        self.transition(RuntimeState::Created, RuntimeState::Prepared)
    }

    pub fn attach(&mut self) -> Result<(), RuntimeError> {
        self.transition(RuntimeState::Prepared, RuntimeState::Attached)
    }

    pub fn activate(&mut self) -> Result<(), RuntimeError> {
        self.transition(RuntimeState::Attached, RuntimeState::Active)
    }

    pub fn drain(&mut self) -> Result<(), RuntimeError> {
        self.transition(RuntimeState::Active, RuntimeState::Draining)
    }

    pub fn detach(&mut self) -> Result<(), RuntimeError> {
        match self.state {
            RuntimeState::Attached | RuntimeState::Active | RuntimeState::Draining => {
                self.state = RuntimeState::Detached;
                Ok(())
            }
            RuntimeState::Detached => Ok(()),
            state => Err(RuntimeError::InvalidTransition {
                from: state,
                to: RuntimeState::Detached,
            }),
        }
    }

    pub fn fail(&mut self) {
        self.state = RuntimeState::Failed;
    }

    pub fn ingest(&mut self, record: &[u8]) -> Result<Vec<DecodedScalar>, RuntimeError> {
        if self.state != RuntimeState::Active {
            return Err(RuntimeError::NotActive(self.state));
        }
        let result = self.decode_record(record);
        match result {
            Ok(values) => {
                self.counters.records_decoded += 1;
                if self.current_partial(record).unwrap_or(false) {
                    self.counters.unavailable_fields += 1;
                }
                Ok(values)
            }
            Err(error) => {
                self.counters.decode_drops += 1;
                Err(error)
            }
        }
    }

    fn decode_record(&self, record: &[u8]) -> Result<Vec<DecodedScalar>, RuntimeError> {
        let (envelope, payload) = EventEnvelope::decode_record(record, self.max_payload)?;
        if envelope.profile_id != self.profile_id || envelope.plan_hash != self.plan_hash {
            return Err(RuntimeError::StalePlan {
                expected_profile: self.profile_id.clone(),
                actual_profile: envelope.profile_id,
                expected_plan: self.plan_hash.clone(),
                actual_plan: envelope.plan_hash,
            });
        }
        Ok(decode_scalar_record(
            &envelope,
            &payload,
            &self.fields,
            self.max_payload,
        )?)
    }

    fn current_partial(&self, record: &[u8]) -> Result<bool, EnvelopeError> {
        EventEnvelope::decode_record(record, self.max_payload).map(|(envelope, _)| envelope.partial)
    }

    fn transition(&mut self, from: RuntimeState, to: RuntimeState) -> Result<(), RuntimeError> {
        if self.state != from {
            return Err(RuntimeError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("runtime profile and plan identity are required")]
    MissingIdentity,
    #[error("runtime capture plan has no fields")]
    EmptyPlan,
    #[error("invalid runtime transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: RuntimeState,
        to: RuntimeState,
    },
    #[error("runtime is not active: {0:?}")]
    NotActive(RuntimeState),
    #[error("record belongs to {actual_profile}/{actual_plan}, expected {expected_profile}/{expected_plan}")]
    StalePlan {
        expected_profile: String,
        actual_profile: String,
        expected_plan: String,
        actual_plan: String,
    },
    #[error("wire envelope error: {0}")]
    Envelope(#[from] EnvelopeError),
    #[error("scalar decode error: {0}")]
    Decode(#[from] crate::decode::ScalarDecodeError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScalarFieldSpec, ScalarKind};

    fn runtime() -> ProfiledCaptureRuntime {
        ProfiledCaptureRuntime::new(
            "rust",
            "sha256:plan",
            vec![ScalarFieldSpec {
                name: "counter".into(),
                offset: 0,
                size: 8,
                kind: ScalarKind::Unsigned,
            }],
            64,
        )
        .unwrap()
    }

    #[test]
    fn lifecycle_is_explicit_and_ordered() {
        let mut runtime = runtime();
        assert_eq!(runtime.state(), RuntimeState::Created);
        runtime.prepare().unwrap();
        runtime.attach().unwrap();
        runtime.activate().unwrap();
        runtime.drain().unwrap();
        runtime.detach().unwrap();
        runtime.detach().unwrap();
        assert_eq!(runtime.state(), RuntimeState::Detached);
    }

    #[test]
    fn stale_plan_is_dropped_and_counted() {
        let mut runtime = runtime();
        runtime.prepare().unwrap();
        runtime.attach().unwrap();
        runtime.activate().unwrap();
        let envelope = EventEnvelope::new("rust", "sha256:old", 1, 8);
        let record = envelope.encode_record(&1u64.to_le_bytes(), 64).unwrap();
        assert!(matches!(
            runtime.ingest(&record),
            Err(RuntimeError::StalePlan { .. })
        ));
        assert_eq!(runtime.counters().decode_drops, 1);
    }
}
