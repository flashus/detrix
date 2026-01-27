//! Client proxy handling for lldb-serve

use super::config::{ClientResult, ServerState};
use super::discovery::is_codelldb;
use super::protocol::{create_launch_request, read_dap_message, write_dap_message};
use detrix_config::{DEFAULT_LLDB_CONFIG_DONE_DELAY_MS, DEFAULT_LOG_MESSAGE_PREVIEW_LEN};
use detrix_logging::{debug, error, info, warn};
use std::io::{BufRead, BufReader, BufWriter};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
#[cfg(windows)]
use std::time::Duration;

/// Handle a single client connection
pub fn handle_client(
    client_stream: TcpStream,
    lldb_dap_path: &PathBuf,
    program: &str,
    program_args: &[String],
    stop_on_entry: bool,
    cwd: Option<&PathBuf>,
) -> ClientResult {
    debug!("handle_client: starting");
    // Note: Socket already configured with TCP keepalive and read timeout by configure_client_socket()

    // Check if we're using CodeLLDB (better PDB support on Windows)
    let using_codelldb = is_codelldb(lldb_dap_path);

    if using_codelldb {
        info!("Using CodeLLDB adapter (enhanced PDB support)");
    }

    // Start lldb-dap/codelldb as a subprocess with stdio
    debug!(lldb_dap = ?lldb_dap_path, is_codelldb = using_codelldb, "handle_client: spawning debug adapter");

    // On Windows, we need to ensure proper pipe handling
    // Some debuggers need CREATE_NO_WINDOW or special flags
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new(lldb_dap_path);
        // CREATE_NO_WINDOW (0x08000000) - prevents console window popup
        c.creation_flags(0x08000000);
        c
    };

    #[cfg(not(windows))]
    let mut cmd = Command::new(lldb_dap_path);

    // For CodeLLDB, add --settings to enable Rust source language support
    if using_codelldb {
        cmd.args(["--settings", r#"{"sourceLanguages":["rust"]}"#]);
    }

    let mut lldb_process = match cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(p) => {
            debug!(pid = ?p.id(), "handle_client: lldb-dap spawned");
            info!("[lldb-dap] Process spawned with PID {}", p.id());
            p
        }
        Err(e) => {
            error!(err = %e, "Failed to spawn lldb-dap");
            return ClientResult::Error(e.into());
        }
    };

    // Determine stdio handling strategy
    // On Windows + CodeLLDB: we MUST redirect program stdio to a file to prevent it from
    // mixing with the DAP protocol stream. Without this, program output corrupts DAP messages.
    // We pre-create the file to ensure CodeLLDB can open it for writing.
    // On other platforms: also redirect to temp file and tail it for visibility.
    let stdio_log_path = {
        let path = std::env::temp_dir().join(format!("detrix-program-{}.log", lldb_process.id()));

        // Pre-create the file on Windows to ensure CodeLLDB can write to it
        // This helps avoid race conditions and permission issues
        #[cfg(windows)]
        {
            if let Err(e) = std::fs::File::create(&path) {
                warn!(
                    "handle_client: failed to pre-create stdio log file: {} - program output may corrupt DAP stream",
                    e
                );
            } else {
                debug!(
                    stdio_path = %path.display(),
                    "handle_client: pre-created stdio log file for program output"
                );
            }
        }

        let path_str = path.to_string_lossy().to_string();
        info!(
            stdio_path = %path_str,
            "handle_client: program stdio will be redirected to temp file"
        );
        Some((path, path_str))
    };

    // Give lldb-dap time to initialize on Windows
    // Some debuggers need a moment to set up their pipes
    #[cfg(windows)]
    {
        debug!("handle_client: waiting 100ms for lldb-dap to initialize (Windows)");
        thread::sleep(Duration::from_millis(100));

        // Check if process is still alive after startup
        match lldb_process.try_wait() {
            Ok(Some(status)) => {
                error!(
                    exit_code = ?status.code(),
                    "lldb-dap exited immediately after spawn!"
                );
                return ClientResult::Error(anyhow::anyhow!(
                    "lldb-dap exited immediately with code {:?}",
                    status.code()
                ));
            }
            Ok(None) => {
                debug!("handle_client: lldb-dap is still running after startup delay");
            }
            Err(e) => {
                warn!("handle_client: could not check lldb-dap status: {}", e);
            }
        }
    }

    let lldb_stdin = match lldb_process.stdin.take() {
        Some(s) => s,
        None => {
            error!("Failed to capture lldb-dap stdin pipe");
            return ClientResult::Error(anyhow::anyhow!("Failed to capture lldb-dap stdin pipe"));
        }
    };
    let lldb_stdout = match lldb_process.stdout.take() {
        Some(s) => s,
        None => {
            error!("Failed to capture lldb-dap stdout pipe");
            return ClientResult::Error(anyhow::anyhow!("Failed to capture lldb-dap stdout pipe"));
        }
    };
    let lldb_stderr = match lldb_process.stderr.take() {
        Some(s) => s,
        None => {
            error!("Failed to capture lldb-dap stderr pipe");
            return ClientResult::Error(anyhow::anyhow!("Failed to capture lldb-dap stderr pipe"));
        }
    };
    debug!("handle_client: lldb-dap pipes captured (stdin, stdout, stderr)");

    // Clone streams for bidirectional proxy
    let client_reader = match client_stream.try_clone() {
        Ok(r) => r,
        Err(e) => return ClientResult::Error(e.into()),
    };
    let client_writer = client_stream;

    // Shared state
    let state = Arc::new(ServerState {
        launch_sent: AtomicBool::new(false),
        session_established: AtomicBool::new(false),
    });
    let running = Arc::new(AtomicBool::new(true));

    // Track the launch request seq for synthesizing response
    // (daemon's launch request seq, so we can synthesize a matching response)
    let launch_request_seq = Arc::new(AtomicU32::new(0));

    // Channel for synthetic responses (from client_to_lldb to lldb_to_client thread)
    // This allows us to send responses to daemon when we drop/intercept requests
    let (synthetic_tx, synthetic_rx) = mpsc::channel::<String>();

    debug!("handle_client: starting proxy threads");

    // Thread: stderr reader (prevents lldb-dap from blocking on stderr writes)
    let running_clone = Arc::clone(&running);
    let stderr_reader = thread::spawn(move || {
        debug!("[lldb-stderr] Thread started");
        let reader = BufReader::new(lldb_stderr);
        for line in std::io::BufRead::lines(reader) {
            match line {
                Ok(line) => {
                    // Log lldb-dap stderr output - this helps debug issues
                    if !line.is_empty() {
                        warn!("[lldb-dap stderr] {}", line);
                    }
                }
                Err(e) => {
                    debug!("[lldb-stderr] Read error (lldb-dap may have exited): {}", e);
                    break;
                }
            }
            if !running_clone.load(Ordering::Relaxed) {
                break;
            }
        }
        debug!("[lldb-stderr] Thread ending");
    });

    // Thread: program output tailer (reads from stdio temp file)
    // Only spawn if we're using file redirect (not on Windows + CodeLLDB)
    let program_output_tailer = if let Some((ref path, _)) = stdio_log_path {
        let running_clone = Arc::clone(&running);
        let stdio_path_clone = path.clone();
        Some(thread::spawn(move || {
            debug!("[program-output] Tailer thread started");

            // Wait a bit for the file to be created
            thread::sleep(std::time::Duration::from_millis(500));

            // Open file in read mode, keep reading new content
            let file = match std::fs::File::open(&stdio_path_clone) {
                Ok(f) => f,
                Err(e) => {
                    debug!("[program-output] Could not open stdio log file: {}", e);
                    return;
                }
            };

            let mut reader = BufReader::new(file);
            while running_clone.load(Ordering::Relaxed) {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        // EOF - wait and try again (file may have more content later)
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Ok(_) => {
                        // Print program output with prefix
                        if !line.is_empty() {
                            print!("[program] {}", line);
                            if !line.ends_with('\n') {
                                println!();
                            }
                        }
                    }
                    Err(e) => {
                        debug!("[program-output] Read error: {}", e);
                        break;
                    }
                }
            }

            // Clean up temp file
            let _ = std::fs::remove_file(&stdio_path_clone);
            debug!("[program-output] Tailer thread ending");
        }))
    } else {
        None
    };

    // Thread: client -> lldb-dap (with message injection)
    let running_clone = Arc::clone(&running);
    let state_clone = Arc::clone(&state);
    let launch_request_seq_clone = Arc::clone(&launch_request_seq);
    let synthetic_tx_clone = synthetic_tx;
    let program_owned = program.to_string();
    let program_args_owned = program_args.to_vec();
    let cwd_owned = cwd.map(|p| p.to_string_lossy().to_string());
    let stdio_log_path_str_clone = stdio_log_path.as_ref().map(|(_, s)| s.clone());
    let client_to_lldb = thread::spawn(move || {
        info!("[client->lldb] Proxy thread started");
        let mut reader = BufReader::new(client_reader);
        let mut writer = BufWriter::new(lldb_stdin);
        let mut message_count = 0u32;

        while running_clone.load(Ordering::Relaxed) {
            // Read DAP message (Content-Length header + body)
            debug!("[client->lldb] Waiting for message from client...");
            if let Some(message) = read_dap_message(&mut reader) {
                message_count += 1;
                let preview = &message[..message.len().min(DEFAULT_LOG_MESSAGE_PREVIEW_LEN)];
                info!(
                    msg_num = message_count,
                    bytes = message.len(),
                    "[client->lldb] Received from daemon: {}",
                    preview
                );

                // Check if this is an initialize response - we need to inject launch
                let json: serde_json::Value = match serde_json::from_str(&message) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(err = %e, "[client->lldb] JSON parse error, forwarding as-is");
                        // Forward as-is if not valid JSON
                        if let Err(e) = write_dap_message(&mut writer, &message) {
                            error!(err = %e, "[client->lldb] Error forwarding to lldb-dap");
                            break;
                        }
                        continue;
                    }
                };

                // Extract command for logging
                let command = json
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                info!(command = command, "[client->lldb] Processing DAP command");

                // Track if a launch request was forwarded from daemon
                // (daemon may send its own launch when using direct mode with program parameter)
                if command == "launch" {
                    state_clone.launch_sent.store(true, Ordering::Relaxed);
                    // Store the seq number so we can synthesize a matching response
                    if let Some(seq) = json.get("seq").and_then(|v| v.as_u64()) {
                        launch_request_seq_clone.store(seq as u32, Ordering::Relaxed);
                        debug!(
                            seq = seq,
                            "[client->lldb] Launch request seq stored for synthetic response"
                        );
                    }
                    debug!("[client->lldb] Launch request detected from daemon, marking launch_sent=true");
                }

                // DROP attach requests from daemon - lldb-serve handles launch internally
                // The daemon sends "attach" because that's the default mode for Rust adapter,
                // but lldb-serve will inject its own "launch" request with the program path.
                // CodeLLDB fails on bare "attach" without program/pid, so we must skip it.
                // We send a synthetic response via channel to the lldb_to_client thread.
                if command == "attach" {
                    let attach_seq = json.get("seq").and_then(|v| v.as_u64()).unwrap_or(2);
                    info!(
                        seq = attach_seq,
                        "[client->lldb] Dropping 'attach' request - sending synthetic response"
                    );

                    // Create synthetic attach response
                    let synthetic_response = serde_json::json!({
                        "seq": 0,
                        "type": "response",
                        "request_seq": attach_seq,
                        "success": true,
                        "command": "attach",
                        "body": {}
                    });
                    let response_str = synthetic_response.to_string();

                    // Send via channel to lldb_to_client thread
                    if let Err(e) = synthetic_tx_clone.send(response_str) {
                        warn!(err = %e, "[client->lldb] Failed to queue synthetic attach response");
                    } else {
                        info!("[client->lldb] Synthetic attach response queued");
                    }
                    continue;
                }

                // Check if this is configurationDone - inject launch BEFORE forwarding it
                // DAP sequence: initialize -> launch -> (breakpoints etc) -> configurationDone
                // ONLY inject if daemon hasn't already sent a launch request
                if command == "configurationDone"
                    && !state_clone.launch_sent.swap(true, Ordering::Relaxed)
                {
                    debug!("[client->lldb] Injecting launch request before configurationDone");
                    // Inject launch request BEFORE configurationDone
                    let launch_request = match create_launch_request(
                        &program_owned,
                        &program_args_owned,
                        stop_on_entry,
                        cwd_owned.as_deref(),
                        stdio_log_path_str_clone.as_deref(), // redirect program output to temp file (or null)
                    ) {
                        Ok(req) => req,
                        Err(e) => {
                            error!(err = %e, "[client->lldb] Failed to create launch request");
                            break;
                        }
                    };
                    let preview_len = launch_request
                        .len()
                        .min(super::protocol::LOG_PAYLOAD_PREVIEW_LEN);
                    debug!(launch_req = %&launch_request[..preview_len], "[client->lldb] Launch request");
                    info!("Injecting launch request for program: {}", program_owned);
                    if let Err(e) = write_dap_message(&mut writer, &launch_request) {
                        error!(err = %e, "[client->lldb] Error sending launch request");
                        break;
                    }
                    debug!(
                        "[client->lldb] Launch request sent, waiting {}ms before configurationDone",
                        DEFAULT_LLDB_CONFIG_DONE_DELAY_MS
                    );
                    // Wait for launch to complete before sending configurationDone
                    thread::sleep(std::time::Duration::from_millis(
                        DEFAULT_LLDB_CONFIG_DONE_DELAY_MS,
                    ));
                }

                // Forward the message
                info!(command = command, "[client->lldb] Forwarding to lldb-dap");
                if let Err(e) = write_dap_message(&mut writer, &message) {
                    error!(err = %e, "[client->lldb] Error forwarding to lldb-dap");
                    break;
                }
                info!(
                    command = command,
                    "[client->lldb] Forwarded to lldb-dap successfully"
                );

                // Track initialize to mark session as established
                if command == "initialize" {
                    state_clone
                        .session_established
                        .store(true, Ordering::Relaxed);
                    info!("[client->lldb] Session established (initialize received)");
                }
            } else {
                warn!("[client->lldb] Client disconnected or read error (no message received)");
                break;
            }
        }
        info!(
            message_count = message_count,
            "[client->lldb] Thread ending"
        );
        running_clone.store(false, Ordering::Relaxed);
    });

    // Thread: lldb-dap -> client
    // We need to share lldb_process to check its status from the reader thread
    // Note: Using std::sync::Mutex is acceptable here because:
    // 1. Lock duration is very short (no await while holding)
    // 2. This is thread-based code, not async
    let lldb_process = Arc::new(std::sync::Mutex::new(lldb_process));
    let lldb_process_clone = Arc::clone(&lldb_process);

    // Track whether we've synthesized a launch response
    let launch_response_sent = Arc::new(AtomicBool::new(false));
    let launch_response_sent_clone = Arc::clone(&launch_response_sent);
    let launch_request_seq_clone2 = Arc::clone(&launch_request_seq);

    let running_clone = Arc::clone(&running);
    let lldb_to_client = thread::spawn(move || {
        info!("[lldb->client] Proxy thread started, waiting for lldb-dap responses...");
        let mut reader = BufReader::new(lldb_stdout);
        let mut writer = BufWriter::new(client_writer);
        let mut message_count = 0u32;

        let mut consecutive_empty_reads = 0u32;
        const MAX_EMPTY_READS: u32 = 3; // Allow a few empty reads before giving up

        while running_clone.load(Ordering::Relaxed) {
            // First, check for any synthetic messages to send to daemon
            while let Ok(synthetic_msg) = synthetic_rx.try_recv() {
                info!(
                    bytes = synthetic_msg.len(),
                    "[lldb->client] Sending synthetic message to daemon"
                );
                if let Err(e) = write_dap_message(&mut writer, &synthetic_msg) {
                    error!(err = %e, "[lldb->client] Error sending synthetic message");
                } else {
                    info!("[lldb->client] Synthetic message sent successfully");
                }
            }

            debug!("[lldb->client] Waiting for message from lldb-dap...");
            if let Some(message) = read_dap_message(&mut reader) {
                consecutive_empty_reads = 0; // Reset counter on successful read
                message_count += 1;
                let preview = &message[..message.len().min(DEFAULT_LOG_MESSAGE_PREVIEW_LEN)];
                info!(
                    msg_num = message_count,
                    bytes = message.len(),
                    "[lldb->client] Received from lldb-dap: {}",
                    preview
                );

                // Check if this is the "initialized" event and daemon sent a launch request
                // DAP protocol: launch response should come before initialized event, but CodeLLDB
                // sends initialized event first. The daemon blocks waiting for launch response.
                // We synthesize one to unblock the daemon - BUT ONLY if daemon sent a launch request.
                // If daemon sent "attach" (default Rust mode), we already sent synthetic attach response
                // and lldb-serve injected its own launch request - no need to synthesize launch response.
                let daemon_launch_seq = launch_request_seq_clone2.load(Ordering::Relaxed);
                if daemon_launch_seq != 0 && !launch_response_sent_clone.load(Ordering::Relaxed) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&message) {
                        if json.get("type").and_then(|v| v.as_str()) == Some("event")
                            && json.get("event").and_then(|v| v.as_str()) == Some("initialized")
                        {
                            // Mark as sent BEFORE sending (to prevent race)
                            launch_response_sent_clone.store(true, Ordering::Relaxed);

                            // Synthesize a launch response matching the daemon's request
                            let synthetic_launch_response = serde_json::json!({
                                "seq": 0,
                                "type": "response",
                                "request_seq": daemon_launch_seq,
                                "success": true,
                                "command": "launch",
                                "body": {}
                            });
                            let response_str = synthetic_launch_response.to_string();
                            info!(
                                request_seq = daemon_launch_seq,
                                "[lldb->client] Synthesizing launch response to unblock daemon"
                            );
                            if let Err(e) = write_dap_message(&mut writer, &response_str) {
                                error!(err = %e, "[lldb->client] Error sending synthetic launch response");
                            } else {
                                info!("[lldb->client] Synthetic launch response sent");
                            }
                        }
                    }
                }

                // Validate JSON before forwarding - CodeLLDB on Windows sometimes sends
                // truncated messages during rapid initialization (many module events at once).
                // If we forward invalid JSON, the daemon will fail to parse and close the connection.
                if serde_json::from_str::<serde_json::Value>(&message).is_err() {
                    warn!(
                        bytes = message.len(),
                        "[lldb->client] Received invalid JSON from lldb-dap, skipping (possible truncated message)"
                    );
                    debug!(
                        raw = %message,
                        "[lldb->client] Invalid JSON content"
                    );
                    // Don't forward invalid JSON - continue to next message
                    continue;
                }

                // Intercept DAP output events and display program stdout/stderr
                // This catches program output when CodeLLDB sends it as proper DAP events
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&message) {
                    if json.get("type").and_then(|v| v.as_str()) == Some("event")
                        && json.get("event").and_then(|v| v.as_str()) == Some("output")
                    {
                        if let Some(body) = json.get("body") {
                            let category =
                                body.get("category").and_then(|v| v.as_str()).unwrap_or("");
                            // Display stdout and stderr from the program
                            // Skip "console" category (debugger messages) and "telemetry"
                            if category == "stdout" || category == "stderr" {
                                if let Some(output) = body.get("output").and_then(|v| v.as_str()) {
                                    print!("[program] {}", output);
                                    if !output.ends_with('\n') {
                                        println!();
                                    }
                                }
                            }
                        }
                    }
                }

                info!("[lldb->client] Forwarding response to daemon...");
                if let Err(e) = write_dap_message(&mut writer, &message) {
                    error!(err = %e, "[lldb->client] Error forwarding to daemon");
                    break;
                }
                info!("[lldb->client] Response forwarded to daemon successfully");
            } else {
                consecutive_empty_reads += 1;

                // Check if lldb-dap process has exited
                let process_exited = if let Ok(mut proc) = lldb_process_clone.lock() {
                    match proc.try_wait() {
                        Ok(Some(status)) => {
                            error!(
                                exit_code = ?status.code(),
                                "[lldb->client] lldb-dap process exited with code {:?}",
                                status.code()
                            );
                            true
                        }
                        Ok(None) => {
                            // Process still running - might be temporary read issue on Windows
                            debug!(
                                "[lldb->client] Empty read but process still running (attempt {}/{})",
                                consecutive_empty_reads, MAX_EMPTY_READS
                            );
                            false
                        }
                        Err(e) => {
                            warn!("[lldb->client] Could not check lldb-dap status: {}", e);
                            false
                        }
                    }
                } else {
                    false
                };

                if process_exited {
                    warn!("[lldb->client] lldb-dap process has exited");
                    break;
                }

                // If process is still running, wait a bit and retry (Windows pipe buffering issue)
                if consecutive_empty_reads < MAX_EMPTY_READS {
                    debug!("[lldb->client] Waiting 100ms before retry...");
                    thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }

                warn!(
                    "[lldb->client] lldb-dap disconnected or read error after {} attempts",
                    consecutive_empty_reads
                );
                break;
            }
        }
        info!(
            message_count = message_count,
            "[lldb->client] Thread ending"
        );
        running_clone.store(false, Ordering::Relaxed);
    });

    // Wait for threads to finish
    info!("Waiting for proxy threads to finish...");
    let _ = client_to_lldb.join();
    debug!("handle_client: client_to_lldb thread finished");
    let _ = lldb_to_client.join();
    debug!("handle_client: lldb_to_client thread finished");
    let _ = stderr_reader.join();
    debug!("handle_client: stderr_reader thread finished");
    if let Some(tailer) = program_output_tailer {
        let _ = tailer.join();
        debug!("handle_client: program_output_tailer thread finished");
    }

    // Kill lldb-dap process
    debug!("handle_client: killing lldb-dap process");
    if let Ok(mut proc) = lldb_process.lock() {
        let _ = proc.kill();
        let _ = proc.wait();
    }
    debug!("handle_client: lldb-dap process terminated");

    // Return based on whether a real DAP session was established
    let session_was_established = state.session_established.load(Ordering::Relaxed);
    debug!(
        session_established = session_was_established,
        "handle_client: session status"
    );

    if session_was_established {
        debug!("handle_client: returning SessionCompleted");
        ClientResult::SessionCompleted
    } else {
        // No initialize request received - probably a port scan or health check
        debug!("handle_client: returning NoSession (no initialize received)");
        ClientResult::NoSession
    }
}
