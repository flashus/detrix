//! DAP Initialization
//!
//! DAP handshake and mode-specific initialization logic.

use super::config::{AdapterConfig, ConnectionMode};
use crate::constants::{defaults, requests};
use crate::{Capabilities, DapBroker, Error, InitializeRequestArguments, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// How long to wait for an early `success: false` response after sending an attach/launch
/// request without blocking.  Adapters that don't respond at all (lldb-dap 22.x) will
/// simply not reply within this window, so we proceed normally.  Adapters that respond
/// immediately (lldb-dap 21.x on error, debugpy) will be caught within this window.
const ATTACH_FAILURE_WINDOW: Duration = Duration::from_millis(500);

/// Initialize DAP connection (handshake)
pub async fn initialize_dap(
    broker: &DapBroker,
    config: &AdapterConfig,
    capabilities: &Arc<Mutex<Option<Capabilities>>>,
    connection_config: &detrix_config::AdapterConnectionConfig,
) -> Result<()> {
    debug!("Initializing DAP connection for {}", config.adapter_id);

    // Send initialize request
    let init_args = InitializeRequestArguments {
        client_id: Some("detrix".to_string()),
        client_name: Some("Detrix".to_string()),
        adapter_id: config.adapter_id.clone(),
        locale: Some("en-US".to_string()),
        lines_start_at1: Some(true),
        columns_start_at1: Some(true),
        path_format: Some("path".to_string()),
    };

    let response = broker
        .send_request(requests::INITIALIZE, Some(serde_json::to_value(init_args)?))
        .await?;

    if !response.success {
        return Err(Error::InitializationFailed(
            response
                .message
                .unwrap_or_else(|| "Unknown error".to_string()),
        ));
    }

    // Parse capabilities from response body
    if let Some(body) = response.body {
        let caps: Capabilities = serde_json::from_value(body)?;
        let mut cap_lock = capabilities.lock().await;
        *cap_lock = Some(caps.clone());
        debug!(
            "Adapter capabilities: supports_log_points={:?}",
            caps.supports_log_points
        );
    }

    // Send attach/launch request based on connection mode
    // IMPORTANT: debugpy may not respond to the attach request until after
    // configurationDone is sent. So we send attach without waiting, then
    // send configurationDone and wait for that response.
    match &config.connection_mode {
        ConnectionMode::Attach { host, port } => {
            // For debugpy --listen mode, send attach request without connect info
            // since we're already connected to the listen socket
            let attach_args = serde_json::json!({
                "justMyCode": false,
                "subProcess": true,
                "redirectOutput": true
            });
            debug!("Sending attach request to {}:{}", host, port);

            // Send attach without blocking but detect an immediate failure response.
            // debugpy normally responds only after configurationDone, so the 500ms window
            // passes without a response in the happy path.  If debugpy immediately
            // returns success: false (e.g. bad host/port), we surface it here.
            broker
                .send_and_detect_failure(requests::ATTACH, Some(attach_args), ATTACH_FAILURE_WINDOW)
                .await?;
            debug!("Attach request sent (failure-detection window passed)");
        }
        ConnectionMode::AttachRemote { host, port } => {
            // For headless servers like `dlv exec --headless`, send attach request
            // with mode="remote" which tells the server we expect it to already
            // be debugging a target (started via command line).
            // See: https://github.com/go-delve/delve/blob/master/Documentation/api/dap/README.md
            let attach_args = serde_json::json!({
                "mode": "remote",
                "stopOnEntry": false
            });
            debug!("Sending remote attach request to {}:{}", host, port);

            // Send attach request and wait for response (Delve responds immediately)
            let response = broker
                .send_request(requests::ATTACH, Some(attach_args))
                .await?;

            if !response.success {
                return Err(Error::InitializationFailed(
                    response
                        .message
                        .unwrap_or_else(|| "Remote attach failed".to_string()),
                ));
            }
            debug!("Remote attach successful");
        }
        ConnectionMode::Launch => {
            return Err(crate::error::Error::InitializationFailed(
                "Launch mode is not yet implemented".to_string(),
            ));
        }
        ConnectionMode::LaunchProgram {
            host,
            port,
            program,
            args,
            stop_on_entry,
            init_commands,
            pre_run_commands,
        } => {
            // For lldb-dap server mode (--connection listen://), send launch request
            // with program path. lldb-dap will load and start the target.
            // See: https://github.com/llvm/llvm-project/blob/main/lldb/tools/lldb-dap/README.md
            let mut launch_args = serde_json::json!({
                "program": program,
                "args": args,
                "stopOnEntry": stop_on_entry,
                "cwd": std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
            });

            // Include initCommands if provided (e.g., Rust type formatters, settings)
            // These run BEFORE target creation
            if !init_commands.is_empty() {
                launch_args["initCommands"] = serde_json::json!(init_commands);
                debug!(
                    "Including {} initCommands in launch request",
                    init_commands.len()
                );
            }

            // Include preRunCommands if provided (e.g., PDB symbol loading)
            // These run AFTER target creation but BEFORE launch
            if !pre_run_commands.is_empty() {
                launch_args["preRunCommands"] = serde_json::json!(pre_run_commands);
                debug!(
                    "Including {} preRunCommands in launch request",
                    pre_run_commands.len()
                );
            }

            debug!(
                "Sending launch request to {}:{} for program: {}",
                host, port, program
            );

            // Send launch WITHOUT waiting for a response.
            //
            // lldb-dap 21.x sent a `launch` response before the `initialized` event.
            // lldb-dap 22.x emits events (`output`, `initialized`, `module`, …) instead
            // and never sends a `launch` response at all.  Waiting for the response would
            // block until the 30 s broker timeout.
            //
            // The broker's reader loop drains incoming events while we wait for the
            // subsequent `configurationDone` response, so no events are lost.
            let seq = broker.next_sequence().await;
            let request = crate::Request {
                seq,
                command: requests::LAUNCH.to_string(),
                arguments: Some(launch_args),
            };
            broker
                .send_message_no_wait(crate::ProtocolMessage::Request(request))
                .await?;
            debug!(
                "Launch request sent (seq={}), not waiting for response (lldb-dap 22+ emits events instead)",
                seq
            );
        }
        ConnectionMode::AttachPid {
            host: _,
            port: _,
            pid,
            program,
            wait_for,
            init_commands,
        } => {
            // For lldb-dap, send attach request with PID or program name.
            // lldb-dap will attach to the running process.
            // See: https://github.com/llvm/llvm-project/blob/main/lldb/tools/lldb-dap/README.md
            //
            // CRITICAL: stopOnEntry must be false to prevent deadlock when attaching
            // to a process that is waiting for an HTTP response (e.g., Rust client
            // waiting for daemon registration response). Without this, lldb-dap
            // pauses the target via ptrace, the target can't receive the HTTP response,
            // and the registration times out.
            let mut attach_args = serde_json::json!({
                "stopOnEntry": false
            });

            if let Some(p) = pid {
                attach_args["pid"] = serde_json::json!(p);
            }
            if let Some(ref prog) = program {
                attach_args["program"] = serde_json::json!(prog);
            }
            if *wait_for {
                attach_args["waitFor"] = serde_json::json!(true);
            }
            // Add init commands for type formatters (e.g., Rust &str display)
            if !init_commands.is_empty() {
                attach_args["initCommands"] = serde_json::json!(init_commands);
            }

            // Send attach without blocking but detect an immediate failure response.
            //
            // lldb-dap 22.x does not send an `attach` response — it processes the
            // attach, emits `module` events for each loaded library, and eventually
            // handles `configurationDone`.  The 500ms failure-detection window passes
            // without a response in that case, which is normal.
            //
            // lldb-dap 21.x sends an `attach` response.  If `success: false` arrives
            // within the window (e.g. bad PID, ptrace permission denied), we surface
            // the error immediately instead of waiting for the configurationDone timeout.
            broker
                .send_and_detect_failure(requests::ATTACH, Some(attach_args), ATTACH_FAILURE_WINDOW)
                .await?;
            info!(
                "AttachPid: attach request sent (failure-detection window passed), \
                 pid={:?} init_commands={}",
                pid,
                init_commands.len()
            );
        }
    }

    // Send configurationDone request to signal we're ready.
    //
    // For AttachPid mode, configurationDone is queued behind the attach processing
    // inside lldb-dap (module enumeration, ptrace setup, etc.).
    // Use a long timeout to cover the full cycle.
    let config_done_response = if matches!(config.connection_mode, ConnectionMode::AttachPid { .. })
    {
        let timeout = connection_config
            .attach_config_done_timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(120));
        info!(
            "AttachPid: waiting for configurationDone (timeout={}s, covers attach processing)",
            timeout.as_secs()
        );
        broker
            .send_request_with_timeout(requests::CONFIGURATION_DONE, None, timeout)
            .await?
    } else {
        broker
            .send_request(requests::CONFIGURATION_DONE, None)
            .await?
    };
    if !config_done_response.success {
        return Err(Error::InitializationFailed(
            config_done_response
                .message
                .unwrap_or_else(|| "configurationDone failed".to_string()),
        ));
    }

    // For AttachRemote mode (e.g., Delve headless), the program is paused after attach.
    // We must send a continue request to resume execution, otherwise logpoints won't fire.
    // Note: For languages like Go (Delve), the debugger pauses the program when DAP connects,
    // even if the process was started with `dlv attach --continue`. We need to explicitly
    // resume it via DAP continue request after initialization.
    if matches!(config.connection_mode, ConnectionMode::AttachRemote { .. }) {
        debug!("AttachRemote mode: sending continue request to resume program execution");

        let continue_args = serde_json::json!({
            "threadId": defaults::THREAD_ID
        });

        let continue_response = broker
            .send_request(requests::CONTINUE, Some(continue_args))
            .await?;

        if continue_response.success {
            info!("Program execution resumed after attach (required for logpoints to fire)");
        } else {
            // Don't fail initialization if continue fails - the program might already be running
            debug!(
                "Continue request failed (program may already be running): {:?}",
                continue_response.message
            );
        }
    }

    debug!("DAP connection initialized for {}", config.adapter_id);
    Ok(())
}
