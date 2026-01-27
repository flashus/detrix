//! DAP Initialization
//!
//! DAP handshake and mode-specific initialization logic.

use super::config::{AdapterConfig, ConnectionMode};
use crate::{Capabilities, DapBroker, Error, InitializeRequestArguments, Result};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// Initialize DAP connection (handshake)
pub async fn initialize_dap(
    broker: &DapBroker,
    config: &AdapterConfig,
    capabilities: &Arc<Mutex<Option<Capabilities>>>,
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
        .send_request("initialize", Some(serde_json::to_value(init_args)?))
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

            // Send attach request WITHOUT waiting - debugpy responds after configurationDone
            let seq = broker.next_sequence().await;
            let request = crate::Request {
                seq,
                command: "attach".to_string(),
                arguments: Some(attach_args),
            };
            broker
                .send_message_no_wait(crate::ProtocolMessage::Request(request))
                .await?;
            debug!(
                "Attach request sent (seq={}), not waiting for response",
                seq
            );
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
            let response = broker.send_request("attach", Some(attach_args)).await?;

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
            // For launch mode, the launch request would be sent separately
            // TODO: actual implementation of launch mode deferred for later
            debug!("Launch mode - launch request would be sent separately");
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

            // Send launch request and wait for response
            let response = broker.send_request("launch", Some(launch_args)).await?;

            if !response.success {
                return Err(Error::InitializationFailed(
                    response
                        .message
                        .unwrap_or_else(|| "Launch failed".to_string()),
                ));
            }
            debug!("Launch successful");
        }
        ConnectionMode::AttachPid {
            host,
            port,
            pid,
            program,
            wait_for,
        } => {
            // For lldb-dap, send attach request with PID or program name.
            // lldb-dap will attach to the running process.
            // See: https://github.com/llvm/llvm-project/blob/main/lldb/tools/lldb-dap/README.md
            let mut attach_args = serde_json::json!({});

            if let Some(p) = pid {
                attach_args["pid"] = serde_json::json!(p);
            }
            if let Some(ref prog) = program {
                attach_args["program"] = serde_json::json!(prog);
            }
            if *wait_for {
                attach_args["waitFor"] = serde_json::json!(true);
            }

            debug!(
                "Sending attach request to {}:{} with pid={:?}, program={:?}, waitFor={}",
                host, port, pid, program, wait_for
            );

            // Send attach request and wait for response
            let response = broker.send_request("attach", Some(attach_args)).await?;

            if !response.success {
                return Err(Error::InitializationFailed(
                    response
                        .message
                        .unwrap_or_else(|| "Attach failed".to_string()),
                ));
            }
            debug!("Attach successful");
        }
    }

    // Send configurationDone request to signal we're ready
    let config_done_response = broker.send_request("configurationDone", None).await?;
    if !config_done_response.success {
        return Err(Error::InitializationFailed(
            config_done_response
                .message
                .unwrap_or_else(|| "configurationDone failed".to_string()),
        ));
    }

    // For AttachRemote mode (e.g., Delve headless), the program is paused after attach.
    // We do NOT auto-continue here - logpoints should be set first via set_metric(),
    // then the caller should use continue_execution() to resume the program.
    // This ensures logpoints are active before the program starts running.
    // See: https://github.com/go-delve/delve/blob/master/Documentation/api/dap/README.md
    if matches!(config.connection_mode, ConnectionMode::AttachRemote { .. }) {
        debug!(
            "AttachRemote mode: program is paused, waiting for logpoints to be set before continue"
        );
    }

    debug!("DAP connection initialized for {}", config.adapter_id);
    Ok(())
}
