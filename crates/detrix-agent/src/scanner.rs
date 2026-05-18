//! /proc scanner for discovering Go binaries with DWARF debug info.
//!
//! Tracks binaries by `(pid, exe_inode)` to detect PID reuse — when a process
//! exits and a new process gets the same PID, the inode of `/proc/<pid>/exe`
//! will differ, so the scanner correctly reports the new process.

use detrix_config::ScannerConfig;
use detrix_logging::warn;
use glob::Pattern;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

/// Information about a discovered binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryInfo {
    pub binary_path: String,
    pub pid: u32,
    /// Inode number of the binary file (`/proc/<pid>/exe` target).
    /// Used with pid to detect PID reuse.
    pub inode: u64,
    pub build_info: String,
    pub has_dwarf: bool,
    pub exported_functions: Vec<String>,
}

/// /proc scanner that tracks discovered binaries and detects changes.
pub struct ProcScanner {
    #[allow(dead_code)]
    include_patterns: Vec<Pattern>,
    #[allow(dead_code)]
    exclude_patterns: Vec<Pattern>,
    require_dwarf: bool,
    /// Key is (pid, exe_inode). Same PID + new inode = PID was reused.
    known: HashMap<(u32, u64), BinaryInfo>,
    last_registered_at: Option<Instant>,
    min_reregister_secs: u64,
}

impl ProcScanner {
    pub fn new(config: &ScannerConfig) -> Self {
        let include_patterns = config
            .include_patterns
            .iter()
            .filter_map(|p| Pattern::new(p).ok())
            .collect();
        let exclude_patterns = config
            .exclude_patterns
            .iter()
            .filter_map(|p| Pattern::new(p).ok())
            .collect();

        Self {
            include_patterns,
            exclude_patterns,
            require_dwarf: config.require_dwarf,
            known: HashMap::new(),
            last_registered_at: None,
            min_reregister_secs: 300, // 5 minutes cooldown
        }
    }

    /// Full scan — returns all currently observable binaries.
    pub fn scan_full(&mut self) -> Vec<BinaryInfo> {
        let binaries = self.do_scan();
        self.known.clear();
        for binary in &binaries {
            self.known
                .insert((binary.pid, binary.inode), binary.clone());
        }
        binaries
    }

    /// Delta scan — returns full list only if changes detected.
    pub fn scan_delta(&mut self) -> Option<Vec<BinaryInfo>> {
        if let Some(last) = self.last_registered_at {
            if last.elapsed().as_secs() < self.min_reregister_secs {
                // Cooldown active — scan anyway to update known, but don't report changes
                let _ = self.do_scan();
                return None;
            }
        }

        let new_binaries = self.do_scan();
        let new_known: HashMap<(u32, u64), BinaryInfo> = new_binaries
            .iter()
            .map(|b| ((b.pid, b.inode), b.clone()))
            .collect();

        let changed = new_known.len() != self.known.len()
            || new_known.iter().any(|(k, v)| self.known.get(k) != Some(v));

        self.known = new_known;

        if changed {
            self.last_registered_at = Some(Instant::now());
            Some(new_binaries)
        } else {
            None
        }
    }

    fn do_scan(&self) -> Vec<BinaryInfo> {
        let mut binaries = Vec::new();
        let mut warned: HashMap<String, bool> = HashMap::new();

        for entry in fs::read_dir("/proc").ok().into_iter().flatten() {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };

            // Store the procfs path directly — do NOT resolve symlink target for the
            // adapter input. In Docker with pid:host, agent sees host PIDs but not the
            // host filesystem. `/proc/<pid>/exe` stays kernel-accessible.
            let exe_path = path.join("exe");
            if !exe_path.exists() {
                continue;
            }
            let binary_path_str = exe_path.to_string_lossy().to_string();
            // Pattern matching still benefits from the resolved target path because
            // `/proc/<pid>/exe` is not descriptive enough to distinguish binaries.
            let match_path = fs::read_link(&exe_path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| binary_path_str.clone());

            if self.exclude_patterns.iter().any(|p| p.matches(&match_path)) {
                continue;
            }

            if !self.include_patterns.is_empty()
                && !self.include_patterns.iter().any(|p| p.matches(&match_path))
            {
                continue;
            }

            // Read inode from `/proc/<pid>/exe` metadata directly (kernel-accessible).
            // Used for PID-reuse detection — same PID + different inode = new process.
            let inode = fs::metadata(&exe_path).map(|m| m.ino()).unwrap_or(0);

            let Ok(mut file) = fs::File::open(&exe_path) else {
                if !warned.contains_key(&binary_path_str) {
                    warn!(path = %binary_path_str, "Cannot read binary, skipping");
                    warned.insert(binary_path_str.clone(), true);
                }
                continue;
            };
            let mut magic = [0u8; 4];
            if file.read_exact(&mut magic).is_err() || &magic != b"\x7fELF" {
                continue;
            }

            let has_dwarf = if self.require_dwarf {
                has_dwarf_sections(&binary_path_str)
            } else {
                true
            };
            binaries.push(BinaryInfo {
                binary_path: binary_path_str,
                pid,
                inode,
                build_info: String::new(),
                has_dwarf,
                exported_functions: Vec::new(),
            });
        }

        binaries
    }
}

/// Returns true if the ELF binary at `path` contains a `.debug_info` DWARF section.
///
/// Used to honour `ScannerConfig.require_dwarf`: binaries without DWARF cannot be
/// instrumented by the eBPF adapter and must not be reported as observable.
fn has_dwarf_sections(path: &str) -> bool {
    let Ok(data) = std::fs::read(path) else {
        return false;
    };
    match object::File::parse(data.as_slice()) {
        Ok(obj) => {
            use object::Object;
            obj.section_by_name(".debug_info").is_some()
        }
        Err(_) => false,
    }
}

/// Convert agent BinaryInfo to proto BinaryInfo.
pub fn binary_info_to_proto(info: &BinaryInfo) -> detrix_api::generated::detrix::v1::BinaryInfo {
    detrix_api::generated::detrix::v1::BinaryInfo {
        binary_path: info.binary_path.clone(),
        pid: info.pid,
        build_info: info.build_info.clone(),
        has_dwarf: info.has_dwarf,
        exported_functions: info.exported_functions.clone(),
    }
}
