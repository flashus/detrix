//! Probe event types — the data structure flowing through the ring buffer
//!
//! These types represent the raw captured data from a uprobe hit,
//! before it gets converted to a `MetricEvent`.

use crate::dwarf::types::VariableSize;

/// Default maximum number of variables captured per probe hit.
///
/// Override via `detrix.toml` `[ebpf] max_capture_vars`.
pub const MAX_CAPTURE_VARS: usize = 8;

/// Default maximum string content bytes captured per Go string variable.
///
/// The BPF event struct embeds a `u8 var{i}_str[N]` buffer for each GoString variable.
/// Larger values waste ring buffer space; 64 bytes covers most identifiers and short values.
///
/// Override via `detrix.toml` `[ebpf] string_capture_bytes`.
pub const MAX_STRING_CAPTURE: usize = 64;

/// Default maximum bytes captured for fixed-size array and struct blobs.
///
/// Override via `detrix.toml` `[ebpf] blob_capture_bytes`.
pub const MAX_BLOB_CAPTURE: usize = 64;

/// Runtime capture limits derived from `detrix_config::EbpfConfig`.
///
/// Both `generate_bpf_program` and `parse_ring_buffer_event` must use the **same**
/// `CaptureConfig` so the BPF struct layout matches what the parser expects.
///
/// # Hybrid capture strategy
///
/// BPF has a 512-byte stack limit, so we use depth-limited capture:
/// - **Depth 0-1**: BPF reads struct fields inline into ring buffer
/// - **Depth 2+**: User-space follows pointers recursively (not yet implemented)
///
/// Future enhancement: Add user-space memory reader for depth 2+ recursion.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Maximum variables per probe hit (validated before BPF generation).
    pub max_capture_vars: usize,
    /// Fixed-size string content buffer in the BPF event struct.
    pub max_string_capture: usize,
    /// Maximum bytes captured for fixed-size array and struct blobs.
    pub max_blob_capture: usize,
    /// Maximum recursion depth for nested types (default: 2).
    ///
    /// Follows Delve's `MaxVariableRecurse` semantics:
    /// - `0`: Only capture top-level scalars (no struct fields, no slice elements)
    /// - `1`: Capture struct fields, but not nested structs/slices/maps
    /// - `2`: Capture one level of nesting (struct fields + their scalar fields)
    /// - `3+`: Deeper nesting (limited by BPF stack and ring buffer size)
    ///
    /// # Hybrid strategy
    ///
    /// - BPF captures depth 0-1 inline (fast, no pointer chasing)
    /// - User-space can recurse to depth N via pointer reads (future)
    pub max_capture_depth: usize,
    /// Maximum struct fields to capture per struct (-1 = all).
    ///
    /// Follows Delve's `MaxStructFields` semantics. Useful for large structs.
    pub max_struct_fields: i32,
    /// Maximum array/slice/map elements to capture.
    ///
    /// Follows Delve's `MaxArrayValues` semantics. Prevents ring buffer blowup.
    pub max_array_values: usize,
    /// Whether the BPF program captures the goroutine ID (goid).
    ///
    /// When `true`, the generated BPF program reads `runtime.g.goid` from the
    /// goroutine struct pointer (R14 on x86-64). The event struct includes
    /// a `goid` field, and user-space parses it from the ring buffer.
    /// When `false`, events use OS thread ID (tid) for correlation.
    pub capture_goid: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            max_capture_vars: MAX_CAPTURE_VARS,
            max_string_capture: MAX_STRING_CAPTURE,
            max_blob_capture: MAX_BLOB_CAPTURE,
            max_capture_depth: 2,
            max_struct_fields: -1,
            max_array_values: 64,
            capture_goid: false,
        }
    }
}

impl From<&detrix_config::EbpfConfig> for CaptureConfig {
    fn from(cfg: &detrix_config::EbpfConfig) -> Self {
        Self {
            max_capture_vars: cfg.max_capture_vars,
            max_string_capture: cfg.string_capture_bytes,
            max_blob_capture: cfg.blob_capture_bytes,
            max_capture_depth: cfg.max_capture_depth,
            max_struct_fields: cfg.max_struct_fields,
            max_array_values: cfg.max_array_values,
            capture_goid: cfg.capture_goid,
        }
    }
}

impl CaptureConfig {
    /// Validate runtime capture config constraints.
    ///
    /// Returns a list of error strings. Empty means valid.
    /// This mirrors `EbpfConfig::validate()` but operates on the runtime type.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.max_capture_vars == 0 {
            errors.push("max_capture_vars must be >= 1".to_string());
        }
        if self.max_capture_vars > 16 {
            errors.push("max_capture_vars must be <= 16 (BPF stack limit)".to_string());
        }
        if self.max_string_capture == 0 {
            errors.push("max_string_capture must be > 0".to_string());
        } else if !self.max_string_capture.is_multiple_of(8) {
            errors.push(format!(
                "max_string_capture must be a multiple of 8, got {}",
                self.max_string_capture
            ));
        }
        if self.max_string_capture > 255 {
            errors.push("max_string_capture must be <= 255 (BPF verifier bound)".to_string());
        }
        if self.max_blob_capture == 0 {
            errors.push("max_blob_capture must be > 0".to_string());
        } else if !self.max_blob_capture.is_multiple_of(8) {
            errors.push(format!(
                "max_blob_capture must be a multiple of 8, got {}",
                self.max_blob_capture
            ));
        }
        if self.max_capture_depth > 32 {
            errors.push(format!(
                "max_capture_depth must be <= 32, got {}",
                self.max_capture_depth
            ));
        }
        if self.max_array_values == 0 {
            errors.push("max_array_values must be >= 1".to_string());
        }

        errors
    }
}

/// Raw captured value from a single BPF read.
#[derive(Debug, Clone, PartialEq)]
pub enum CapturedValue {
    /// Scalar value (register or stack read), up to 8 bytes.
    Scalar(u64),
    /// Float value (float32 or float64 field, decoded from IEEE 754 bits).
    Float(f64),
    /// String value (pointer + length read, then data read).
    String { data: Vec<u8>, len: usize },
    /// Go slice header: len and cap (ptr is captured but not returned to callers).
    Slice { len: u64, cap: u64 },
    /// Fixed-size array with element values.
    ///
    /// Elements are captured inline from the array's memory.
    /// Element type is preserved for JSON serialization.
    Array {
        element_type: String,
        elements: Vec<CapturedValue>,
    },
    /// Raw bytes from a fixed-size array or struct captured as a blob.
    ///
    /// Used when element type is unknown or array is too large to iterate.
    Bytes(Vec<u8>),
    /// Struct with named fields (depth-limited nested capture).
    ///
    /// Fields are captured recursively up to `max_capture_depth`.
    /// Each field is a (name, value) pair.
    Struct {
        type_name: String,
        fields: Vec<(String, Box<CapturedValue>)>,
    },
    /// Map (key-value pairs) — currently unsupported.
    ///
    /// Go maps are complex runtime structures (hmap) that require
    /// special introspection. This variant is a placeholder for future support.
    Map {
        key_type: String,
        value_type: String,
        entries: Vec<(CapturedValue, CapturedValue)>,
        reason: String, // Explanation of why capture failed/partial
    },
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
            Self::Float(f) => Some(*f),
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

    /// Slice length if this is a `Slice` value, else `None`.
    pub fn slice_len(&self) -> Option<u64> {
        match self {
            Self::Slice { len, .. } => Some(*len),
            _ => None,
        }
    }

    /// Raw bytes if this is a `Bytes` blob, else `None`.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// Convert to a JSON-compatible string representation.
    ///
    /// For structs, recursively converts all fields to JSON object notation.
    pub fn to_json_value(&self, size: VariableSize) -> String {
        match self {
            Self::Float(f) => format!("{f}"),
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
            Self::Slice { len, cap } => format!("{{\"len\":{len},\"cap\":{cap}}}"),
            Self::Array {
                element_type,
                elements,
            } => {
                // Convert array elements to JSON array
                let n = elements.len();
                let elem_strs: Vec<String> = elements
                    .iter()
                    .map(|elem| elem.to_json_value(VariableSize::QWord))
                    .collect();
                format!(
                    "{{\"__type\":\"[{n}]{element_type}\",\"elements\":[{}]}}",
                    elem_strs.join(",")
                )
            }
            Self::Bytes(b) => {
                // Hex-encode raw bytes for JSON portability
                let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
                format!("\"0x{hex}\"")
            }
            Self::Struct { type_name, fields } => {
                // Convert struct fields to JSON object
                let field_strs: Vec<String> = fields
                    .iter()
                    .map(|(name, value)| {
                        format!("\"{}\":{}", name, value.to_json_value(VariableSize::QWord))
                    })
                    .collect();
                format!("{{\"__type\":\"{}\",{}}}", type_name, field_strs.join(","))
            }
            Self::Map {
                key_type,
                value_type,
                entries,
                reason,
            } => {
                if entries.is_empty() && !reason.is_empty() {
                    // Map capture failed — return error with explanation
                    format!("{{\"__type\":\"map[{key_type}]{value_type}\",\"error\":\"{reason}\",\"partial_entries\":0}}")
                } else {
                    // Map entries captured — output as array of {key, value} objects
                    let entry_strs: Vec<String> = entries
                        .iter()
                        .map(|(k, v)| {
                            format!(
                                "{{\"key\":{},\"value\":{}}}",
                                k.to_json_value(VariableSize::QWord),
                                v.to_json_value(VariableSize::QWord)
                            )
                        })
                        .collect();
                    format!(
                        "{{\"__type\":\"map[{key_type}]{value_type}\",\"entries\":[{}]}}",
                        entry_strs.join(",")
                    )
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
        let bits = f64::to_bits(std::f64::consts::PI);
        let val = CapturedValue::Scalar(bits);
        let f = val.as_f64().unwrap();
        assert!((f - std::f64::consts::PI).abs() < f64::EPSILON);
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
        assert_eq!(val.to_json_value(VariableSize::QWord), "\"<error: oops>\"");
    }

    #[test]
    fn to_json_value_struct() {
        // Test nested struct serialization
        let struct_val = CapturedValue::Struct {
            type_name: "main.Order".to_string(),
            fields: vec![
                (
                    "Product".to_string(),
                    Box::new(CapturedValue::Struct {
                        type_name: "main.Product".to_string(),
                        fields: vec![
                            (
                                "Name".to_string(),
                                Box::new(CapturedValue::String {
                                    data: b"Laptop".to_vec(),
                                    len: 6,
                                }),
                            ),
                            ("Price".to_string(), Box::new(CapturedValue::Scalar(999))),
                        ],
                    }),
                ),
                ("Total".to_string(), Box::new(CapturedValue::Scalar(1500))),
            ],
        };

        let json = struct_val.to_json_value(VariableSize::QWord);
        assert!(json.contains("\"__type\":\"main.Order\""));
        assert!(json.contains("\"Product\":"));
        assert!(json.contains("\"__type\":\"main.Product\""));
        assert!(json.contains("\"Name\":\"Laptop\""));
        assert!(json.contains("\"Price\":999"));
        assert!(json.contains("\"Total\":1500"));
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
