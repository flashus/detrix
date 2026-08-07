//! eBPF logpoint configuration
//!
//! Controls capture depth and limits for the eBPF uprobe adapter used for Go on Linux.
//! Follows Delve's LoadConfig pattern: MaxVariableRecurse, MaxStructFields, MaxArrayValues.

use crate::constants::{
    DEFAULT_EBPF_BLOB_CAPTURE_BYTES, DEFAULT_EBPF_MAX_CAPTURE_VARS,
    DEFAULT_EBPF_STRING_CAPTURE_BYTES,
};
use serde::{Deserialize, Serialize};

/// eBPF capture configuration.
///
/// These settings control how much data the BPF programs capture per uprobe hit.
/// Changes take effect when a new metric is added (the BPF program is compiled fresh).
///
/// # Depth-limited capture
///
/// Follows Delve's `LoadConfig` pattern:
/// - `max_capture_depth` ≈ `MaxVariableRecurse` - how deep to recurse into nested types
/// - `max_struct_fields` ≈ `MaxStructFields` - max fields to read per struct (-1 = all)
/// - `max_array_values` ≈ `MaxArrayValues` - max elements to read from arrays/slices/maps
///
/// # Hybrid BPF strategy
///
/// BPF has a 512-byte stack limit, so we use a hybrid approach:
/// - **Depth 0-1**: BPF reads struct fields directly into ring buffer
/// - **Depth 2+**: User-space recursively reads nested pointers/structs
///
/// This allows capturing deep type hierarchies without BPF stack overflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EbpfConfig {
    /// Maximum number of variables captured per uprobe hit.
    ///
    /// Each additional variable adds 8 bytes to the BPF event struct.
    /// Must be ≥ 1. Values above 16 may trigger BPF stack size limits.
    #[serde(default = "default_max_capture_vars")]
    pub max_capture_vars: usize,

    /// Maximum string content bytes captured per Go string variable.
    ///
    /// The BPF event struct embeds a `u8 var{i}_str[N]` fixed-size buffer for each
    /// `string`-typed variable. Larger values capture more content but waste ring buffer
    /// space on short strings. Must be a multiple of 8 for struct alignment.
    #[serde(default = "default_string_capture_bytes")]
    pub string_capture_bytes: usize,

    /// Maximum bytes captured for fixed-size arrays and struct blobs.
    ///
    /// Arrays (`[N]T`) and user-defined structs are captured as raw byte blobs. The BPF
    /// event struct embeds a `u8 var{i}_blob[M]` buffer of this size. Clamped to the
    /// actual `DW_AT_byte_size` so small types don't over-read. Must be a multiple of 8.
    #[serde(default = "default_blob_capture_bytes")]
    pub blob_capture_bytes: usize,

    /// Maximum recursion depth for nested type capture (default: 2).
    ///
    /// Controls how deep the eBPF adapter will traverse into nested structs, slices,
    /// arrays, maps, and pointers. Follows Delve's `MaxVariableRecurse` semantics.
    ///
    /// # Examples
    ///
    /// ```text
    /// Order {                    // depth 0
    ///   Product {                // depth 1
    ///     Category {             // depth 2
    ///       Metadata { ... }     // depth 3 (not captured if max_capture_depth=2)
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// # Hybrid strategy
    ///
    /// - **Depth 0-1**: BPF reads all fields inline (fast, no pointer chasing)
    /// - **Depth 2+**: User-space follows pointers and reads nested data
    ///
    /// Recommended value: 2-3 for production, 5+ for debugging.
    #[serde(default = "default_max_capture_depth")]
    pub max_capture_depth: usize,

    /// Maximum number of struct fields to capture per struct (default: -1 = all).
    ///
    /// Follows Delve's `MaxStructFields` semantics. When a struct has more fields
    /// than this limit, only the first N fields are captured. Set to -1 to capture
    /// all fields regardless of count.
    ///
    /// Useful for large structs where capturing all fields would exceed ring buffer
    /// limits or BPF stack limits.
    #[serde(default = "default_max_struct_fields")]
    pub max_struct_fields: i32,

    /// Maximum number of array/slice/map elements to capture (default: 64).
    ///
    /// Follows Delve's `MaxArrayValues` semantics. Limits how many elements are
    /// read from arrays, slices, and maps. Prevents capturing huge arrays from
    /// blowing up ring buffer usage.
    ///
    /// Note: For maps, this limits the number of key-value pairs, not individual
    /// entries (which are captured recursively based on `max_capture_depth`).
    #[serde(default = "default_max_array_values")]
    pub max_array_values: usize,

    /// Whether to capture the goroutine ID (goid) in BPF event data.
    ///
    /// When enabled, the generated BPF program reads `runtime.g.goid` from the
    /// goroutine struct pointer stored in R14 (x86-64) or X28 (arm64).
    /// The offset is Go-version-dependent and compiled with `-DGOID_OFFSET=N`.
    ///
    /// Default: `false` (no goid capture — uses OS thread ID for correlation).
    /// Set to `true` to correlate events by goroutine instead of OS thread.
    #[serde(default = "default_capture_goid")]
    pub capture_goid: bool,
}

fn default_max_capture_vars() -> usize {
    DEFAULT_EBPF_MAX_CAPTURE_VARS
}

fn default_string_capture_bytes() -> usize {
    DEFAULT_EBPF_STRING_CAPTURE_BYTES
}

fn default_blob_capture_bytes() -> usize {
    DEFAULT_EBPF_BLOB_CAPTURE_BYTES
}

fn default_max_capture_depth() -> usize {
    2 // Match Delve's default MaxVariableRecurse=1 (0-indexed vs 1-indexed)
}

fn default_max_struct_fields() -> i32 {
    -1 // -1 means "all fields", matching Delve's convention
}

fn default_max_array_values() -> usize {
    64 // Match Delve's default
}

fn default_capture_goid() -> bool {
    false // Disabled by default — OS thread ID used for correlation
}

impl Default for EbpfConfig {
    fn default() -> Self {
        EbpfConfig {
            max_capture_vars: default_max_capture_vars(),
            string_capture_bytes: default_string_capture_bytes(),
            blob_capture_bytes: default_blob_capture_bytes(),
            max_capture_depth: default_max_capture_depth(),
            max_struct_fields: default_max_struct_fields(),
            max_array_values: default_max_array_values(),
            capture_goid: default_capture_goid(),
        }
    }
}

impl EbpfConfig {
    /// Validate all configuration constraints.
    ///
    /// Returns a list of human-readable error strings. An empty list means valid.
    ///
    /// # Validated constraints
    ///
    /// - `max_capture_vars` must be ≥ 1 and ≤ 16 (BPF stack limit)
    /// - `string_capture_bytes` must be a multiple of 8 and ≤ 255 (BPF buffer cap)
    /// - `blob_capture_bytes` must be a multiple of 8
    /// - `max_capture_depth` must be ≤ 32 (matches `MAX_DEPTH` in ringbuf parser)
    /// - `max_array_values` must be ≥ 1
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.max_capture_vars == 0 {
            errors.push(
                "ebpf.max_capture_vars must be >= 1 (at least one variable required)".to_string(),
            );
        }
        if self.max_capture_vars > 16 {
            errors.push(
                "ebpf.max_capture_vars must be <= 16 (higher values may exceed BPF stack limits)"
                    .to_string(),
            );
        }

        if self.string_capture_bytes == 0 {
            errors.push("ebpf.string_capture_bytes must be > 0".to_string());
        } else if !self.string_capture_bytes.is_multiple_of(8) {
            errors.push(format!(
                "ebpf.string_capture_bytes must be a multiple of 8 for struct alignment, got {}",
                self.string_capture_bytes
            ));
        }
        if self.string_capture_bytes > 255 {
            errors
                .push("ebpf.string_capture_bytes must be <= 255 (BPF verifier bound)".to_string());
        }

        if self.blob_capture_bytes == 0 {
            errors.push("ebpf.blob_capture_bytes must be > 0".to_string());
        } else if !self.blob_capture_bytes.is_multiple_of(8) {
            errors.push(format!(
                "ebpf.blob_capture_bytes must be a multiple of 8 for struct alignment, got {}",
                self.blob_capture_bytes
            ));
        }

        if self.max_capture_depth > 32 {
            errors.push(format!(
                "ebpf.max_capture_depth must be <= 32 (hard safety cap), got {}",
                self.max_capture_depth
            ));
        }

        if self.max_array_values == 0 {
            errors.push(
                "ebpf.max_array_values must be >= 1 (at least one element required)".to_string(),
            );
        }

        // max_struct_fields: -1 means "all", >= 0 limits fields. Values < -1 are meaningless.
        if self.max_struct_fields < -1 {
            errors.push("ebpf.max_struct_fields must be -1 (all fields) or >= 0".to_string());
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = EbpfConfig::default();
        let errors = config.validate();
        assert!(
            errors.is_empty(),
            "Default config should be valid, but got: {:?}",
            errors
        );
    }

    #[test]
    fn validate_rejects_zero_capture_vars() {
        let config = EbpfConfig {
            max_capture_vars: 0,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("max_capture_vars")));
    }

    #[test]
    fn validate_rejects_excessive_capture_vars() {
        let config = EbpfConfig {
            max_capture_vars: 17,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("max_capture_vars")));
    }

    #[test]
    fn validate_accepts_max_capture_vars_at_boundary() {
        let config = EbpfConfig {
            max_capture_vars: 16,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().all(|e| !e.contains("max_capture_vars")));
    }

    #[test]
    fn validate_rejects_non_aligned_string_capture_bytes() {
        let config = EbpfConfig {
            string_capture_bytes: 7,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("string_capture_bytes")));
        assert!(errors.iter().any(|e| e.contains("multiple of 8")));
    }

    #[test]
    fn validate_rejects_zero_string_capture_bytes() {
        let config = EbpfConfig {
            string_capture_bytes: 0,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("string_capture_bytes")));
    }

    #[test]
    fn validate_rejects_string_capture_bytes_over_255() {
        let config = EbpfConfig {
            string_capture_bytes: 256,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("255")));
    }

    #[test]
    fn validate_accepts_aligned_string_capture_bytes() {
        let config = EbpfConfig {
            string_capture_bytes: 64,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().all(|e| !e.contains("string_capture_bytes")));
    }

    #[test]
    fn validate_rejects_non_aligned_blob_capture_bytes() {
        let config = EbpfConfig {
            blob_capture_bytes: 100,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("blob_capture_bytes")));
        assert!(errors.iter().any(|e| e.contains("multiple of 8")));
    }

    #[test]
    fn validate_rejects_excessive_capture_depth() {
        let config = EbpfConfig {
            max_capture_depth: 33,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("max_capture_depth")));
        assert!(errors.iter().any(|e| e.contains("32")));
    }

    #[test]
    fn validate_accepts_max_capture_depth_at_boundary() {
        let config = EbpfConfig {
            max_capture_depth: 32,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().all(|e| !e.contains("max_capture_depth")));
    }

    #[test]
    fn validate_rejects_invalid_struct_fields() {
        // Values < -1 are meaningless and should be rejected
        let config = EbpfConfig {
            max_struct_fields: -2,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("max_struct_fields")));
        assert!(errors.iter().any(|e| e.contains("-1")));
    }

    #[test]
    fn validate_rejects_zero_array_values() {
        let config = EbpfConfig {
            max_array_values: 0,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("max_array_values")));
    }

    #[test]
    fn validate_accepts_all_struct_fields() {
        let config = EbpfConfig {
            max_struct_fields: -1,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().all(|e| !e.contains("max_struct_fields")));
    }

    #[test]
    fn validate_accepts_zero_struct_fields() {
        // Zero fields is unusual but valid — just captures no fields
        let config = EbpfConfig {
            max_struct_fields: 0,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        assert!(errors.iter().all(|e| !e.contains("max_struct_fields")));
    }

    #[test]
    fn validate_collects_multiple_errors() {
        let config = EbpfConfig {
            max_capture_vars: 0,
            string_capture_bytes: 7,
            blob_capture_bytes: 0,
            max_capture_depth: 100,
            max_array_values: 0,
            ..EbpfConfig::default()
        };
        let errors = config.validate();
        // Should have errors for all 5 invalid fields
        assert!(
            errors.len() >= 5,
            "Expected >= 5 errors, got {}: {:?}",
            errors.len(),
            errors
        );
    }

    #[test]
    fn validate_accepts_min_valid_config() {
        let config = EbpfConfig {
            max_capture_vars: 1,
            string_capture_bytes: 8,
            blob_capture_bytes: 8,
            max_capture_depth: 0,
            max_struct_fields: 0,
            max_array_values: 1,
            capture_goid: false,
        };
        let errors = config.validate();
        assert!(
            errors.is_empty(),
            "Minimum valid config should pass, but got: {:?}",
            errors
        );
    }
}
