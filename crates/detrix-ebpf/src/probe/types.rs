//! Probe event types — the data structure flowing through the ring buffer
//!
//! These types represent the raw captured data from a uprobe hit,
//! before it gets converted to a `MetricEvent`.

use crate::dwarf::types::VariableSize;
/// Maximum number of variables captured per probe hit.
pub const MAX_CAPTURE_VARS: usize = 8;

/// Maximum string data captured per variable (bytes).
pub const MAX_STRING_CAPTURE: usize = 256;

/// Raw captured value from a single BPF read.
#[derive(Debug, Clone, PartialEq)]
pub enum CapturedValue {
    /// Scalar value (register or stack read), up to 8 bytes.
    Scalar(u64),
    /// String value (pointer + length read, then data read).
    String { data: Vec<u8>, len: usize },
    /// Read failed (invalid address, variable optimized out, etc.)
    Error(String),
}

impl CapturedValue {
    /// Interpret as a signed 64-bit integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Scalar(v) => Some(*v as i64),
            _ => None,
        }
    }

    /// Interpret as an unsigned 64-bit integer.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Scalar(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret as a float64 (reinterpret bits).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Scalar(v) => Some(f64::from_bits(*v)),
            _ => None,
        }
    }

    /// Interpret as a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Scalar(v) => Some(*v != 0),
            _ => None,
        }
    }

    /// Interpret as a UTF-8 string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String { data, len } => {
                let actual_len = (*len).min(data.len());
                std::str::from_utf8(&data[..actual_len]).ok()
            }
            _ => None,
        }
    }

    /// Convert to a JSON-compatible string representation.
    pub fn to_json_value(&self, size: VariableSize) -> String {
        match self {
            Self::Scalar(v) => match size {
                VariableSize::Byte => {
                    if *v <= 1 {
                        format!("{}", *v != 0) // bool
                    } else {
                        format!("{v}")
                    }
                }
                VariableSize::DWord => {
                    // Could be int32 or float32 — default to int
                    format!("{}", *v as i32)
                }
                VariableSize::QWord => format!("{}", *v as i64),
                VariableSize::Word => format!("{}", *v as i16),
            },
            Self::String { data, len } => {
                let actual_len = (*len).min(data.len());
                match std::str::from_utf8(&data[..actual_len]) {
                    Ok(s) => format!("\"{s}\""),
                    Err(_) => format!("\"<binary {actual_len} bytes>\""),
                }
            }
            Self::Error(msg) => format!("\"<error: {msg}>\""),
        }
    }
}

/// A single probe hit event from the ring buffer.
///
/// This is the user-space representation of data sent from the BPF program.
#[derive(Debug, Clone)]
pub struct ProbeEvent {
    /// Process ID.
    pub pid: u32,
    /// Thread ID (OS-level, not goroutine).
    pub tid: u32,
    /// Goroutine ID (if extracted from runtime.g).
    pub goid: Option<u64>,
    /// Timestamp (kernel monotonic, nanoseconds).
    pub timestamp_ns: u64,
    /// Captured variable values, in the same order as ProbePoint.variables.
    pub values: Vec<CapturedValue>,
}

/// Configuration for a probe to be attached.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Path to the target ELF binary.
    pub binary_path: std::path::PathBuf,
    /// Offset from .text base for uprobe attachment.
    pub symbol_offset: u64,
    /// Number of variables to capture.
    pub var_count: usize,
    /// Whether to extract goroutine ID.
    pub capture_goid: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_value_scalar_conversions() {
        let val = CapturedValue::Scalar(42);
        assert_eq!(val.as_i64(), Some(42));
        assert_eq!(val.as_u64(), Some(42));
        assert_eq!(val.as_bool(), Some(true));

        let zero = CapturedValue::Scalar(0);
        assert_eq!(zero.as_bool(), Some(false));
    }

    #[test]
    fn captured_value_f64() {
        let bits = f64::to_bits(3.14);
        let val = CapturedValue::Scalar(bits);
        let f = val.as_f64().unwrap();
        assert!((f - 3.14).abs() < f64::EPSILON);
    }

    #[test]
    fn captured_value_string() {
        let val = CapturedValue::String {
            data: b"hello world".to_vec(),
            len: 5,
        };
        assert_eq!(val.as_str(), Some("hello"));
        assert_eq!(val.as_i64(), None);
    }

    #[test]
    fn captured_value_string_truncated() {
        let val = CapturedValue::String {
            data: b"hi".to_vec(),
            len: 100, // len > data.len()
        };
        assert_eq!(val.as_str(), Some("hi"));
    }

    #[test]
    fn captured_value_error() {
        let val = CapturedValue::Error("segfault".to_string());
        assert_eq!(val.as_i64(), None);
        assert_eq!(val.as_str(), None);
    }

    #[test]
    fn to_json_value_scalar() {
        let val = CapturedValue::Scalar(42);
        assert_eq!(val.to_json_value(VariableSize::QWord), "42");
        assert_eq!(val.to_json_value(VariableSize::DWord), "42");
    }

    #[test]
    fn to_json_value_bool() {
        let val = CapturedValue::Scalar(1);
        assert_eq!(val.to_json_value(VariableSize::Byte), "true");

        let val = CapturedValue::Scalar(0);
        assert_eq!(val.to_json_value(VariableSize::Byte), "false");
    }

    #[test]
    fn to_json_value_string() {
        let val = CapturedValue::String {
            data: b"test".to_vec(),
            len: 4,
        };
        assert_eq!(val.to_json_value(VariableSize::QWord), "\"test\"");
    }

    #[test]
    fn to_json_value_error() {
        let val = CapturedValue::Error("oops".to_string());
        assert_eq!(
            val.to_json_value(VariableSize::QWord),
            "\"<error: oops>\""
        );
    }

    #[test]
    fn probe_event_construction() {
        let event = ProbeEvent {
            pid: 1234,
            tid: 5678,
            goid: Some(42),
            timestamp_ns: 1_000_000_000,
            values: vec![CapturedValue::Scalar(100)],
        };
        assert_eq!(event.pid, 1234);
        assert_eq!(event.goid, Some(42));
        assert_eq!(event.values.len(), 1);
    }
}
