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
        self.last_registered_at = Some(Instant::now());
        binaries
    }

    /// Delta scan — returns full list only if changes detected.
    pub fn scan_delta(&mut self) -> Option<Vec<BinaryInfo>> {
        if let Some(last) = self.last_registered_at {
            if last.elapsed().as_secs() < self.min_reregister_secs {
                // Cooldown active — scan to keep known map fresh so the first
                // post-cooldown delta is incremental, not a full diff.
                let current = self.do_scan();
                self.known = current.into_iter().map(|b| ((b.pid, b.inode), b)).collect();
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

/// Returns true if the ELF64 binary at `path` contains a `.debug_info` DWARF section.
///
/// Uses a manual ELF64 section header parser — reads only the header (64B), section
/// header table, and section name string table. Avoids loading the full binary into
/// memory (important for large Go/Rust binaries in production).
///
/// Used to honour `ScannerConfig.require_dwarf`: binaries without DWARF cannot be
/// instrumented by the eBPF adapter and must not be reported as observable.
fn has_dwarf_sections(path: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };

    // Read 64-byte ELF64 header.
    let mut hdr = [0u8; 64];
    if f.read_exact(&mut hdr).is_err() {
        return false;
    }
    // Magic + ELF64 class check.
    if &hdr[0..4] != b"\x7fELF" || hdr[4] != 2 {
        return false;
    }

    // Fixed-size slice conversions: slice lengths are compile-time-known from the
    // 64-byte header buffer; `.unwrap_or_default()` satisfies clippy::unwrap_used
    // while being unreachable in practice.
    let e_shoff = u64::from_le_bytes(hdr[40..48].try_into().unwrap_or_default());
    let e_shentsize = u16::from_le_bytes(hdr[58..60].try_into().unwrap_or_default()) as u64;
    let e_shnum = u16::from_le_bytes(hdr[60..62].try_into().unwrap_or_default()) as u64;
    let e_shstrndx = u16::from_le_bytes(hdr[62..64].try_into().unwrap_or_default()) as u64;

    if e_shoff == 0 || e_shentsize < 64 || e_shnum == 0 || e_shstrndx >= e_shnum {
        return false;
    }

    // Read section header table.
    let table_size = (e_shnum * e_shentsize) as usize;
    if f.seek(SeekFrom::Start(e_shoff)).is_err() {
        return false;
    }
    let mut table = vec![0u8; table_size];
    if f.read_exact(&mut table).is_err() {
        return false;
    }

    // Locate the section name string table using e_shstrndx.
    let shstr_base = (e_shstrndx * e_shentsize) as usize;
    let sh_offset = u64::from_le_bytes(
        table[shstr_base + 24..shstr_base + 32]
            .try_into()
            .unwrap_or_default(),
    );
    let sh_size = u64::from_le_bytes(
        table[shstr_base + 32..shstr_base + 40]
            .try_into()
            .unwrap_or_default(),
    );

    // Sanity cap: string table larger than 1 MB is suspect.
    if sh_offset == 0 || sh_size > 1_000_000 {
        return false;
    }
    if f.seek(SeekFrom::Start(sh_offset)).is_err() {
        return false;
    }
    let mut strtab = vec![0u8; sh_size as usize];
    if f.read_exact(&mut strtab).is_err() {
        return false;
    }

    // Walk section headers looking for ".debug_info".
    for i in 0..e_shnum as usize {
        let sh = &table[i * e_shentsize as usize..][..e_shentsize as usize];
        let name_idx = u32::from_le_bytes(sh[0..4].try_into().unwrap_or_default()) as usize;
        if name_idx < strtab.len() {
            let name = strtab[name_idx..].split(|&b| b == 0).next().unwrap_or(&[]);
            if name == b".debug_info" {
                return true;
            }
        }
    }
    false
}

/// Convert agent BinaryInfo to proto BinaryInfo.
pub fn binary_info_to_proto(info: &BinaryInfo) -> detrix_api::generated::detrix::v1::BinaryInfo {
    detrix_api::generated::detrix::v1::BinaryInfo {
        binary_path: info.binary_path.clone(),
        pid: info.pid,
        build_info: info.build_info.clone(),
        has_dwarf: info.has_dwarf,
        exported_functions: info.exported_functions.clone(),
        inode: info.inode,
    }
}
