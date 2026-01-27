//! PID file I/O operations
//!
//! File reading, parsing, and writing operations for PID files.

use anyhow::{Context, Result};
use detrix_config::ServiceType;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::process;

// Re-export PidInfo from detrix-config for backward compatibility
pub use detrix_config::pid::PidInfo;

/// Read PID info from file (JSON format)
///
/// Retries on parse failure to handle rare case of reading during a write.
#[cfg_attr(windows, allow(dead_code))] // Only used on Unix
pub(crate) fn read_pid_info_from_file(file: &File) -> Result<PidInfo> {
    use std::io::{BufReader, Seek as _};

    // Retry up to 3 times with small delay to handle read-during-write race
    for attempt in 0..3 {
        // Create a new reader for each attempt, seeking to start
        let mut reader = BufReader::new(file);
        if let Err(e) = reader.seek(SeekFrom::Start(0)) {
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            return Err(e).context("Failed to seek PID file");
        }

        let mut contents = String::new();
        if let Err(e) = reader.read_to_string(&mut contents) {
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
            return Err(e).context("Failed to read PID file");
        }

        match parse_pid_info(contents.trim()) {
            Ok(info) => return Ok(info),
            Err(e) => {
                if attempt < 2 {
                    // Parse failed, might be reading during write - retry
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
                return Err(e);
            }
        }
    }

    anyhow::bail!("Failed to read PID file after retries")
}

/// Parse PID info from string (JSON format)
#[cfg_attr(windows, allow(dead_code))] // Only used via read_pid_info_from_file on Unix
pub(crate) fn parse_pid_info(s: &str) -> Result<PidInfo> {
    serde_json::from_str(s).context("Failed to parse PID file as JSON")
}

/// Write current process PID, host, and ports to file (JSON format)
///
/// Uses write-then-truncate order to avoid empty file window.
/// This ensures readers never see an empty file, only:
/// - Complete old content (before write)
/// - Complete new content (after sync)
/// - Partial new + old trailing during the tiny write window (invalid JSON, reader handles gracefully)
pub(crate) fn write_pid_info(
    file: &mut File,
    ports: &HashMap<ServiceType, u16>,
    host: &str,
) -> Result<()> {
    let pid = process::id();

    // Create PID info and serialize to JSON
    let info = PidInfo {
        pid,
        ports: ports.clone(),
        host: host.to_string(),
    };
    let mut content = serde_json::to_string(&info).context("Failed to serialize PID info")?;
    content.push('\n'); // Trailing newline for readability

    // Write-then-truncate order (safer than truncate-then-write):
    // 1. Seek to start
    file.seek(SeekFrom::Start(0))
        .context("Failed to seek PID file")?;

    // 2. Write new content (overwrites from beginning)
    file.write_all(content.as_bytes())
        .context("Failed to write PID to file")?;

    // 3. Truncate to exact length (removes any old trailing data)
    file.set_len(content.len() as u64)
        .context("Failed to truncate PID file")?;

    // 4. Sync to disk
    file.sync_all().context("Failed to sync PID file")?;

    Ok(())
}
