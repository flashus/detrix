//! eBPF logpoint configuration
//!
//! Controls capture limits for the eBPF uprobe adapter used for Go on Linux.

use crate::constants::{
    DEFAULT_EBPF_BLOB_CAPTURE_BYTES, DEFAULT_EBPF_MAX_CAPTURE_VARS,
    DEFAULT_EBPF_STRING_CAPTURE_BYTES,
};
use serde::{Deserialize, Serialize};

/// eBPF capture configuration.
///
/// These settings control how much data the BPF programs capture per uprobe hit.
/// Changes take effect when a new metric is added (the BPF program is compiled fresh).
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

impl Default for EbpfConfig {
    fn default() -> Self {
        EbpfConfig {
            max_capture_vars: default_max_capture_vars(),
            string_capture_bytes: default_string_capture_bytes(),
            blob_capture_bytes: default_blob_capture_bytes(),
        }
    }
}
