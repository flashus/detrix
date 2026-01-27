//! LSP client for JSON-RPC communication
//!
//! Handles low-level communication with an LSP server over stdio.

use crate::error::{Error, Result};
use crate::protocol::{LspMessage, LspNotification, LspRequest, LspResponse, RequestId};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, trace, warn};

/// Pending request awaiting response
type PendingRequest = oneshot::Sender<Result<LspResponse>>;

/// LSP client for communicating with a language server
pub struct LspClient {
    /// Child process handle
    process: Child,
    /// Stdin for sending messages
    stdin: Arc<Mutex<ChildStdin>>,
    /// Map of pending request IDs to response channels
    pending: Arc<Mutex<HashMap<RequestId, PendingRequest>>>,
    /// Counter for generating unique request IDs
    next_id: AtomicI64,
    /// Reader task handle
    _reader_handle: Option<std::thread::JoinHandle<()>>,
}

impl LspClient {
    /// Create a new LSP client from a running process
    pub fn new(mut process: Child) -> Result<Self> {
        let stdin = process
            .stdin
            .take()
            .ok_or_else(|| Error::ConnectionFailed("No stdin available".into()))?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| Error::ConnectionFailed("No stdout available".into()))?;

        let pending: Arc<Mutex<HashMap<RequestId, PendingRequest>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = Arc::clone(&pending);

        // Get the current tokio runtime handle
        let runtime = tokio::runtime::Handle::current();

        // Start reader thread for handling responses
        let reader_handle = std::thread::spawn(move || {
            Self::reader_loop(stdout, pending_clone, runtime);
        });

        Ok(Self {
            process,
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            next_id: AtomicI64::new(1),
            _reader_handle: Some(reader_handle),
        })
    }

    /// Send a request and wait for response
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<LspResponse> {
        let id = RequestId::Int(self.next_id.fetch_add(1, Ordering::SeqCst));
        let request = LspRequest::new(id.clone(), method, params);

        // Create channel for response
        let (tx, rx) = oneshot::channel();

        // Register pending request
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), tx);
        }

        // Send request
        self.send_message(&LspMessage::Request(request)).await?;

        // Wait for response
        rx.await
            .map_err(|_| Error::ConnectionFailed("Response channel closed".into()))?
    }

    /// Send a notification (no response expected)
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = LspNotification::new(method, params);
        self.send_message(&LspMessage::Notification(notification))
            .await
    }

    /// Send an LSP message
    async fn send_message(&self, message: &LspMessage) -> Result<()> {
        let content = serde_json::to_string(message)?;
        let header = format!("Content-Length: {}\r\n\r\n", content.len());

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(header.as_bytes())?;
        stdin.write_all(content.as_bytes())?;
        stdin.flush()?;

        trace!("Sent LSP message: {}", content);
        Ok(())
    }

    /// Reader loop running in a separate thread
    fn reader_loop(
        stdout: ChildStdout,
        pending: Arc<Mutex<HashMap<RequestId, PendingRequest>>>,
        runtime: tokio::runtime::Handle,
    ) {
        let mut reader = BufReader::new(stdout);

        loop {
            match Self::read_message(&mut reader) {
                Ok(Some(message)) => {
                    if let LspMessage::Response(response) = message {
                        // Dispatch response to waiting request
                        let pending_clone = Arc::clone(&pending);
                        let id = response.id.clone();

                        // Use the runtime handle to spawn the async task
                        runtime.spawn(async move {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(Ok(response));
                            } else {
                                warn!("Received response for unknown request ID: {:?}", id);
                            }
                        });
                    }
                }
                Ok(None) => {
                    debug!("LSP server closed connection");
                    break;
                }
                Err(e) => {
                    warn!("Error reading LSP message: {}", e);
                    break;
                }
            }
        }
    }

    /// Read a single LSP message from the reader
    fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Option<LspMessage>> {
        // Read headers
        let mut content_length = 0;
        loop {
            let mut header = String::new();
            let bytes_read = reader.read_line(&mut header)?;
            if bytes_read == 0 {
                return Ok(None); // EOF
            }

            let header = header.trim();
            if header.is_empty() {
                break; // End of headers
            }

            if let Some(length_str) = header.strip_prefix("Content-Length: ") {
                content_length = length_str
                    .parse()
                    .map_err(|_| Error::Protocol("Invalid Content-Length".into()))?;
            }
        }

        if content_length == 0 {
            return Err(Error::Protocol("Missing Content-Length header".into()));
        }

        // Read content
        let mut content = vec![0u8; content_length];
        reader.read_exact(&mut content)?;

        let content_str = String::from_utf8(content)?;

        trace!("Received LSP message: {}", content_str);

        let message: LspMessage = serde_json::from_str(&content_str)?;
        Ok(Some(message))
    }

    /// Check if the LSP server process is still running
    pub fn is_running(&mut self) -> bool {
        self.process.try_wait().ok().flatten().is_none()
    }

    /// Kill the LSP server process
    pub fn kill(&mut self) -> Result<()> {
        self.process.kill()?;
        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Try to gracefully kill the process
        let _ = self.kill();
    }
}

// Note: We need to use std::io::Read trait for read_exact
use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_ordering() {
        let counter = AtomicI64::new(1);
        let id1 = counter.fetch_add(1, Ordering::SeqCst);
        let id2 = counter.fetch_add(1, Ordering::SeqCst);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    fn test_lsp_message_header_format() {
        let content = r#"{"jsonrpc":"2.0","id":1,"method":"test"}"#;
        let header = format!("Content-Length: {}\r\n\r\n", content.len());
        assert!(header.starts_with("Content-Length: "));
        assert!(header.ends_with("\r\n\r\n"));
    }

    #[test]
    fn test_request_serialization() {
        let request = LspRequest::new(1i64, "initialize", None);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"initialize\""));
    }
}
