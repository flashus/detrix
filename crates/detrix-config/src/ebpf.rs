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

impl Default for EbpfConfig {
    fn default() -> Self {
        EbpfConfig {
            max_capture_vars: default_max_capture_vars(),
            string_capture_bytes: default_string_capture_bytes(),
            blob_capture_bytes: default_blob_capture_bytes(),
            max_capture_depth: default_max_capture_depth(),
            max_struct_fields: default_max_struct_fields(),
            max_array_values: default_max_array_values(),
        }
    }
}
