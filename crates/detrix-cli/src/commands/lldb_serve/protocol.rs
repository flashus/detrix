//! DAP protocol handling

use detrix_config::DEFAULT_LOG_PAYLOAD_PREVIEW_LEN;
use detrix_config::RUST_LLDB_TYPE_FORMATTERS;
use std::io::{BufRead, Write};

/// Read a DAP message (Content-Length header + JSON body)
///
/// This function is resilient to program output that may leak into the DAP stream
/// (particularly on Windows with CodeLLDB). Any non-header lines encountered are
/// treated as program output and printed with a prefix.
pub fn read_dap_message<R: BufRead>(reader: &mut R) -> Option<String> {
    // Read headers, handling potential program output mixed in
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    if content_length.is_some() {
                        break; // End of headers - we have Content-Length
                    }
                    // Empty line but no Content-Length yet - might be between program output lines
                    continue;
                }
                if trimmed.to_lowercase().starts_with("content-length:") {
                    if let Some(len_str) = trimmed.split(':').nth(1) {
                        if let Ok(len) = len_str.trim().parse() {
                            content_length = Some(len);
                        }
                    }
                } else if !trimmed.to_lowercase().starts_with("content-type:") {
                    // Line doesn't look like a DAP header (Content-Length or Content-Type)
                    // This is likely program output that leaked into the DAP stream
                    // Print it with a prefix so the user can see it
                    print!("[program] {}", line);
                    if !line.ends_with('\n') {
                        println!();
                    }
                    // If we already have content_length but got non-header content,
                    // the stream might be corrupted - reset and keep looking
                    if content_length.is_some() {
                        detrix_logging::debug!(
                            "read_dap_message: program output after Content-Length header, resetting"
                        );
                        content_length = None;
                    }
                }
            }
            Err(e) => {
                detrix_logging::debug!("Error reading header: {}", e);
                return None;
            }
        }
    }

    // Read body
    let content_length = content_length?;
    let mut body = vec![0u8; content_length];
    match reader.read_exact(&mut body) {
        Ok(_) => {
            // Check if the body looks like valid JSON (starts with '{')
            // If not, it might be program output that got into the body
            if let Ok(s) = String::from_utf8(body.clone()) {
                let trimmed_body = s.trim_start();
                if !trimmed_body.starts_with('{') && !trimmed_body.starts_with('[') {
                    // Body doesn't look like JSON - might be corrupted with program output
                    // Try to find where the JSON actually starts
                    if let Some(json_start) = trimmed_body.find('{') {
                        let (program_output, json_part) = trimmed_body.split_at(json_start);
                        if !program_output.trim().is_empty() {
                            print!("[program] {}", program_output);
                            if !program_output.ends_with('\n') {
                                println!();
                            }
                        }
                        // Return just the JSON part
                        // Note: this may be truncated, but at least it won't corrupt parsing
                        return Some(json_part.to_string());
                    }
                    // No JSON found at all - this message is completely corrupted
                    detrix_logging::debug!(
                        "read_dap_message: body doesn't contain JSON, treating as program output"
                    );
                    print!("[program] {}", s);
                    return None;
                }
                return Some(s);
            }
            None
        }
        Err(e) => {
            detrix_logging::debug!("Error reading body: {}", e);
            None
        }
    }
}

/// Write a DAP message (Content-Length header + JSON body)
pub fn write_dap_message<W: Write>(writer: &mut W, message: &str) -> std::io::Result<()> {
    let content_length = message.len();
    write!(
        writer,
        "Content-Length: {}\r\n\r\n{}",
        content_length, message
    )?;
    writer.flush()
}

/// Create a DAP launch request with Rust type formatters
///
/// # Arguments
/// * `program` - Path to the program to debug
/// * `args` - Arguments to pass to the program
/// * `stop_on_entry` - Whether to stop on entry
/// * `cwd` - Working directory for the program
/// * `stdio_log_path` - Optional path for program stdout/stderr redirection
///
/// # Returns
/// A JSON string containing the DAP launch request, or an error if serialization fails.
pub fn create_launch_request(
    program: &str,
    args: &[String],
    stop_on_entry: bool,
    cwd: Option<&str>,
    stdio_log_path: Option<&str>,
) -> anyhow::Result<String> {
    // LLDB init commands for Rust type visualization
    // These make String, Vec, etc. display their actual values instead of raw struct representation
    // Using simple summary strings that work without Python dependencies
    // initCommands run BEFORE target creation - good for settings
    let init_commands = build_init_commands(program);

    // preRunCommands run AFTER target creation but before launch - good for symbol loading
    let pre_run_commands = build_pre_run_commands(program);

    // CRITICAL: Redirect program stdio to prevent output from mixing with DAP protocol
    // messages on the same stdout stream. Without this, program prints corrupt the DAP stream.
    // Use array format [stdin, stdout, stderr] for explicit control - this works better on Windows.
    // If a log path is provided, redirect stdout/stderr to that file; stdin is null (no input).
    let stdio_value = match stdio_log_path {
        Some(path) => serde_json::json!([serde_json::Value::Null, path, path]),
        None => serde_json::Value::Null,
    };

    let mut launch_args = serde_json::json!({
        "program": program,
        "args": args,
        "stopOnEntry": stop_on_entry,
        "cwd": cwd.unwrap_or("."),
        "initCommands": init_commands,
        "stdio": stdio_value,
    });

    // Add preRunCommands only if we have any
    if !pre_run_commands.is_empty() {
        launch_args["preRunCommands"] = serde_json::json!(pre_run_commands);
    }

    let request = serde_json::json!({
        "seq": 9999, // High seq number to avoid conflicts
        "type": "request",
        "command": "launch",
        "arguments": launch_args
    });

    Ok(serde_json::to_string(&request)?)
}

/// Build LLDB init commands for Rust debugging.
///
/// Includes:
/// - Type formatters for readable Rust types
/// - Windows-specific symbol search path settings
///
/// NOTE: initCommands run BEFORE target creation, so only settings work here.
/// Use build_pre_run_commands() for commands that require a target.
#[allow(unused_variables, unused_mut)]
fn build_init_commands(program: &str) -> Vec<String> {
    let mut commands: Vec<String> = get_rust_type_formatters()
        .iter()
        .map(|s| s.to_string())
        .collect();

    // On Windows, add settings to help LLDB find PDB symbols
    // NOTE: Only SETTINGS can go in initCommands (before target creation)
    #[cfg(target_os = "windows")]
    {
        // Get the directory containing the program
        if let Some(parent) = std::path::Path::new(program).parent() {
            let parent_str = parent.to_string_lossy();

            // Check if PDB exists for debugging
            let pdb_path = program
                .strip_suffix(".exe")
                .or_else(|| program.strip_suffix(".EXE"))
                .map(|p| format!("{}.pdb", p))
                .unwrap_or_else(|| format!("{}.pdb", program));

            let pdb_exists = std::path::Path::new(&pdb_path).exists();
            detrix_logging::info!(
                "[Windows PDB] Program: {}, Search path: {}, PDB path: {}, PDB exists: {}",
                program,
                parent_str,
                pdb_path,
                pdb_exists
            );

            // Add the program's directory to symbol search paths
            commands.insert(
                0,
                format!(
                    r#"settings set target.debug-file-search-paths "{}""#,
                    parent_str
                ),
            );
        }
    }

    commands
}

/// Build LLDB preRunCommands for Rust debugging.
///
/// These commands run AFTER target creation but BEFORE launch/attach.
/// Use this for commands that require a target to exist (like loading symbols).
///
/// NOTE: We no longer explicitly load PDB files here because LLDB's PDB support
/// is limited and often fails with "does not match any existing module".
/// Instead, we rely on `settings set target.debug-file-search-paths` in initCommands
/// to let LLDB auto-discover symbols.
#[allow(unused_variables)]
fn build_pre_run_commands(_program: &str) -> Vec<String> {
    // Currently empty - LLDB auto-discovers symbols via debug-file-search-paths
    Vec::new()
}

/// Get LLDB type formatter commands for Rust types
///
/// These commands configure LLDB to display Rust types in a readable format.
/// Delegates to the shared constant `RUST_LLDB_TYPE_FORMATTERS` from detrix-config.
pub fn get_rust_type_formatters() -> Vec<&'static str> {
    RUST_LLDB_TYPE_FORMATTERS.to_vec()
}

/// Preview length constant for logging
pub const LOG_PAYLOAD_PREVIEW_LEN: usize = DEFAULT_LOG_PAYLOAD_PREVIEW_LEN;
