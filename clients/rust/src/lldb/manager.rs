//! LLDB process lifecycle management.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tracing::{debug, trace, warn};

use crate::error::{Error, Result};

/// Maximum number of port allocation retries.
const MAX_PORT_RETRIES: u32 = 3;

/// Information about a running lldb-dap process.
#[derive(Debug)]
pub struct LldbProcess {
    /// Child process handle.
    pub child: Child,

    /// Host the DAP server is listening on.
    #[allow(dead_code)]
    pub host: String,

    /// Port the DAP server is listening on.
    pub port: u16,
}

/// Manager for lldb-dap process lifecycle.
pub struct LldbManager {
    /// Path to lldb-dap binary.
    lldb_dap_path: PathBuf,

    /// Timeout for lldb-dap to start.
    timeout: Duration,
}

impl LldbManager {
    /// Create a new LLDB manager.
    pub fn new(lldb_dap_path: PathBuf, timeout: Duration) -> Self {
        Self {
            lldb_dap_path,
            timeout,
        }
    }

    /// Spawn lldb-dap and attach to the current process.
    ///
    /// When port is 0, an ephemeral port is allocated. Due to a TOCTOU race
    /// (the port may be taken between allocation and lldb-dap startup), this
    /// operation is retried up to MAX_PORT_RETRIES times.
    pub fn spawn_and_attach(&self, host: &str, port: u16) -> Result<LldbProcess> {
        // If port is specified (non-zero), no retries needed
        if port != 0 {
            return self.spawn_lldb(host, port);
        }

        // Port 0: ephemeral port allocation with retry logic
        let mut last_err = None;
        for attempt in 0..MAX_PORT_RETRIES {
            // Allocate an ephemeral port
            let actual_port = allocate_port(host)?;
            debug!(
                "Attempt {}: allocated ephemeral port {}",
                attempt + 1,
                actual_port
            );

            match self.spawn_lldb(host, actual_port) {
                Ok(process) => return Ok(process),
                Err(e) => {
                    if is_port_bind_error(&e) {
                        warn!("Port {} taken (TOCTOU race), retrying...", actual_port);
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Err(last_err
            .unwrap_or_else(|| Error::LldbStartFailed("failed after max port retries".to_string())))
    }

    /// Spawn lldb-dap on the specified port.
    ///
    /// IMPORTANT: We do NOT send the DAP attach request here! The Detrix daemon
    /// (a separate process) will connect to lldb-dap and send the attach request.
    /// If we tried to send attach from within this process, lldb-dap would use
    /// ptrace to stop us, causing a deadlock.
    fn spawn_lldb(&self, host: &str, port: u16) -> Result<LldbProcess> {
        // Build command:
        // lldb-dap --connection listen://host:port
        let listen_addr = format!("listen://{}:{}", host, port);

        debug!(
            "Starting lldb-dap: {:?} --connection {}",
            self.lldb_dap_path, listen_addr
        );

        let mut child = Command::new(&self.lldb_dap_path)
            .args(["--connection", &listen_addr])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::LldbStartFailed(e.to_string()))?;

        // Wait for lldb-dap to accept connections
        if let Err(e) = self.wait_for_ready(host, port, &mut child) {
            // Kill process if startup failed
            let _ = self.kill_process(&mut child);
            return Err(e);
        }

        debug!(
            "lldb-dap ready on {}:{}, daemon will attach to PID {}",
            host,
            port,
            std::process::id()
        );

        // NOTE: We do NOT send DAP initialize/attach here.
        // The Detrix daemon will connect and handle the DAP protocol.
        // This avoids a deadlock where lldb-dap would stop this process
        // via ptrace while we're waiting for its response.

        Ok(LldbProcess {
            child,
            host: host.to_string(),
            port,
        })
    }

    /// Wait for lldb-dap to accept connections.
    fn wait_for_ready(&self, host: &str, port: u16, child: &mut Child) -> Result<()> {
        let deadline = Instant::now() + self.timeout;
        let check_interval = Duration::from_millis(100);

        while Instant::now() < deadline {
            // Check if process died
            if let Ok(Some(status)) = child.try_wait() {
                // Read stderr for error message
                let stderr = child
                    .stderr
                    .take()
                    .map(|s| {
                        BufReader::new(s)
                            .lines()
                            .take(10)
                            .filter_map(|l| l.ok())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();

                return Err(Error::LldbStartFailed(format!(
                    "lldb-dap exited with status {}: {}",
                    status, stderr
                )));
            }

            // Try to connect
            let addr = format!("{}:{}", host, port);
            if TcpStream::connect_timeout(
                &addr
                    .parse()
                    .map_err(|e| Error::LldbStartFailed(format!("invalid address: {}", e)))?,
                check_interval,
            )
            .is_ok()
            {
                trace!("lldb-dap accepting connections on {}", addr);
                return Ok(());
            }

            std::thread::sleep(check_interval);
        }

        Err(Error::Timeout(format!(
            "lldb-dap did not become ready within {:?}",
            self.timeout
        )))
    }

    /// Send DAP initialize and attach requests.
    ///
    /// NOTE: This is kept for potential standalone testing, but is NOT used
    /// in normal operation. The Detrix daemon handles the DAP protocol.
    #[allow(dead_code)]
    fn send_attach_request(&self, host: &str, port: u16, pid: u32) -> Result<()> {
        let addr = format!("{}:{}", host, port);
        debug!("Connecting to lldb-dap at {}", addr);
        let mut stream = TcpStream::connect(&addr)
            .map_err(|e| Error::LldbStartFailed(format!("failed to connect to lldb-dap: {}", e)))?;
        debug!("Connected to lldb-dap");

        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

        // Send DAP initialize request
        let init_request = serde_json::json!({
            "seq": 1,
            "type": "request",
            "command": "initialize",
            "arguments": {
                "clientID": "detrix-rust-client",
                "clientName": "Detrix Rust Client",
                "adapterID": "lldb-dap",
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsVariableType": true,
                "supportsVariablePaging": true,
                "supportsRunInTerminalRequest": false,
                "locale": "en-US"
            }
        });

        debug!("Sending initialize request");
        send_dap_message(&mut stream, &init_request)?;

        // Read initialize response
        debug!("Waiting for initialize response");
        let response = read_dap_response(&mut stream, "initialize")?;
        debug!("Initialize response: {:?}", response);

        if response.get("success") != Some(&serde_json::Value::Bool(true)) {
            let message = response
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(Error::LldbStartFailed(format!(
                "initialize failed: {}",
                message
            )));
        }

        // Send attach request
        let attach_request = serde_json::json!({
            "seq": 2,
            "type": "request",
            "command": "attach",
            "arguments": {
                "pid": pid,
                "stopOnEntry": false
            }
        });

        debug!("Sending attach request for PID {}", pid);
        send_dap_message(&mut stream, &attach_request)?;

        // Read attach response - NOTE: lldb-dap may send events before the response
        debug!("Waiting for attach response (may receive events first)");
        let response = read_dap_response(&mut stream, "attach")?;
        debug!("Attach response: {:?}", response);

        if response.get("success") != Some(&serde_json::Value::Bool(true)) {
            let message = response
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(Error::LldbStartFailed(format!(
                "attach failed: {}",
                message
            )));
        }

        // Send configurationDone request
        let config_done_request = serde_json::json!({
            "seq": 3,
            "type": "request",
            "command": "configurationDone",
            "arguments": {}
        });

        debug!("Sending configurationDone request");
        send_dap_message(&mut stream, &config_done_request)?;

        // Read configurationDone response
        debug!("Waiting for configurationDone response");
        let response = read_dap_response(&mut stream, "configurationDone")?;
        debug!("ConfigurationDone response: {:?}", response);

        // Keep the connection open for the daemon to use
        // The stream will be closed when it goes out of scope

        debug!("lldb-dap attached to PID {}", pid);
        Ok(())
    }

    /// Kill the lldb-dap process gracefully.
    pub fn kill(&self, process: &mut LldbProcess) -> Result<()> {
        self.kill_process(&mut process.child)
    }

    /// Kill a child process gracefully.
    fn kill_process(&self, child: &mut Child) -> Result<()> {
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;

            let pid = Pid::from_raw(child.id() as i32);

            // Try graceful shutdown first (SIGTERM)
            if kill(pid, Signal::SIGTERM).is_ok() {
                // Wait with timeout
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    if child.try_wait().ok().flatten().is_some() {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }

            // Force kill
            let _ = child.kill();
            let _ = child.wait();
        }

        #[cfg(not(unix))]
        {
            let _ = child.kill();
            let _ = child.wait();
        }

        Ok(())
    }
}

/// Allocate an ephemeral port.
fn allocate_port(host: &str) -> Result<u16> {
    use std::net::TcpListener;

    let addr = format!("{}:0", host);
    let listener = TcpListener::bind(&addr)
        .map_err(|e| Error::PortBindError(format!("failed to bind to {}: {}", addr, e)))?;

    let port = listener
        .local_addr()
        .map_err(|e| Error::PortBindError(e.to_string()))?
        .port();

    // Drop listener to release port for lldb-dap
    drop(listener);

    Ok(port)
}

/// Check if the error indicates a port bind failure.
fn is_port_bind_error(err: &Error) -> bool {
    match err {
        Error::PortBindError(_) => true,
        Error::LldbStartFailed(msg) => {
            msg.contains("address already in use") || msg.contains("bind")
        }
        _ => false,
    }
}

/// Send a DAP message over the stream.
fn send_dap_message(stream: &mut TcpStream, message: &serde_json::Value) -> Result<()> {
    use std::io::Write;

    let body = serde_json::to_string(message)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());

    stream
        .write_all(header.as_bytes())
        .map_err(|e| Error::LldbStartFailed(format!("failed to write header: {}", e)))?;
    stream
        .write_all(body.as_bytes())
        .map_err(|e| Error::LldbStartFailed(format!("failed to write body: {}", e)))?;
    stream
        .flush()
        .map_err(|e| Error::LldbStartFailed(format!("failed to flush: {}", e)))?;

    Ok(())
}

/// Read a DAP response, skipping any events that come before it.
fn read_dap_response(stream: &mut TcpStream, expected_command: &str) -> Result<serde_json::Value> {
    let start = Instant::now();
    let timeout = Duration::from_secs(10);

    loop {
        if start.elapsed() > timeout {
            return Err(Error::Timeout(format!(
                "timed out waiting for {} response",
                expected_command
            )));
        }

        let msg = read_dap_message(stream)?;
        let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match msg_type {
            "response" => {
                // This is what we're looking for
                return Ok(msg);
            }
            "event" => {
                // Skip events, but log them
                let event_name = msg.get("event").and_then(|e| e.as_str()).unwrap_or("?");
                eprintln!("[LLDB] Skipping event: {}", event_name);
                continue;
            }
            other => {
                eprintln!("[LLDB] Unexpected message type: {}", other);
                continue;
            }
        }
    }
}

/// Read a DAP message from the stream.
fn read_dap_message(stream: &mut TcpStream) -> Result<serde_json::Value> {
    use std::io::{BufRead, BufReader, Read};

    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| Error::LldbStartFailed(format!("failed to clone stream: {}", e)))?,
    );

    // Read headers
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| Error::LldbStartFailed(format!("failed to read header: {}", e)))?;

        let line = line.trim();
        if line.is_empty() {
            break;
        }

        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
    }

    let content_length = content_length
        .ok_or_else(|| Error::LldbStartFailed("missing Content-Length header".to_string()))?;

    // Read body
    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|e| Error::LldbStartFailed(format!("failed to read body: {}", e)))?;

    serde_json::from_slice(&body)
        .map_err(|e| Error::LldbStartFailed(format!("failed to parse response: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_port() {
        let port = allocate_port("127.0.0.1").unwrap();
        assert!(port > 0);
    }

    #[test]
    fn test_is_port_bind_error() {
        assert!(is_port_bind_error(&Error::PortBindError(
            "test".to_string()
        )));
        assert!(is_port_bind_error(&Error::LldbStartFailed(
            "address already in use".to_string()
        )));
        assert!(!is_port_bind_error(&Error::LldbNotFound(
            "test".to_string()
        )));
    }
}
