//! LSP server lifecycle management
//!
//! Handles starting, stopping, and restarting LSP servers.

use crate::client::LspClient;
use crate::error::{Error, Result, ToLspResult};
use crate::protocol::{
    CallHierarchyClientCapabilities, ClientCapabilities, InitializeParams, InitializeResult,
    ServerCapabilities, TextDocumentClientCapabilities, WorkspaceFolder,
};
use detrix_config::LspConfig;
use serde_json::json;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// State of the LSP server
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspState {
    /// Server not started
    NotStarted,
    /// Server is starting
    Starting,
    /// Server is ready
    Ready,
    /// Server failed to start
    Failed(String),
    /// Server stopped
    Stopped,
}

/// Manager for LSP server lifecycle
pub struct LspManager {
    /// Configuration
    config: LspConfig,
    /// Current state
    state: Arc<RwLock<LspState>>,
    /// Active client connection
    client: Arc<RwLock<Option<LspClient>>>,
    /// Server capabilities (cached after initialization)
    capabilities: Arc<RwLock<Option<ServerCapabilities>>>,
}

impl LspManager {
    /// Create a new LSP manager with configuration
    pub fn new(config: LspConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(LspState::NotStarted)),
            client: Arc::new(RwLock::new(None)),
            capabilities: Arc::new(RwLock::new(None)),
        }
    }

    /// Get the current state
    pub async fn state(&self) -> LspState {
        self.state.read().await.clone()
    }

    /// Check if the server is ready
    pub async fn is_ready(&self) -> bool {
        *self.state.read().await == LspState::Ready
    }

    /// Check if call hierarchy is supported
    pub async fn supports_call_hierarchy(&self) -> bool {
        if let Some(caps) = self.capabilities.read().await.as_ref() {
            caps.call_hierarchy_provider.unwrap_or(false)
        } else {
            false
        }
    }

    /// Start the LSP server (lazy start on first use)
    ///
    /// Uses atomic check-and-set to prevent TOCTOU race conditions.
    /// If multiple tasks call start() concurrently, only one will proceed
    /// and others will either return Ok (if already ready) or error (if starting).
    pub async fn start(&self, workspace_root: &Path) -> Result<()> {
        // Atomic check-and-set: hold write lock for the entire state transition
        // to prevent TOCTOU race where two concurrent start() calls both proceed
        {
            let mut state = self.state.write().await;
            match &*state {
                LspState::Ready => return Ok(()),
                LspState::Starting => {
                    return Err(Error::ServerNotAvailable("Server is starting".into()));
                }
                _ => {
                    // Atomically transition to Starting while holding the lock
                    *state = LspState::Starting;
                }
            }
            // Lock is released here, but state is now Starting so other callers will error
        }

        // Start the server process (state is already Starting, so concurrent calls will fail)
        match self.start_server_process(workspace_root).await {
            Ok(()) => {
                let mut state = self.state.write().await;
                *state = LspState::Ready;
                Ok(())
            }
            Err(e) => {
                let mut state = self.state.write().await;
                *state = LspState::Failed(e.to_string());
                Err(e)
            }
        }
    }

    /// Start the LSP server process
    async fn start_server_process(&self, workspace_root: &Path) -> Result<()> {
        let working_dir = self
            .config
            .working_directory
            .as_deref()
            .unwrap_or(workspace_root);

        info!(
            "Starting LSP server: {} {}",
            self.config.command,
            self.config.args.join(" ")
        );

        // Start the process
        let process = Command::new(&self.config.command)
            .args(&self.config.args)
            .current_dir(working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .lsp_server_start(&self.config.command)?;

        // Create client
        let client = LspClient::new(process)?;

        // Initialize LSP
        let init_result = self.initialize(&client, workspace_root).await?;

        // Store capabilities
        {
            let mut caps = self.capabilities.write().await;
            *caps = Some(init_result.capabilities);
        }

        // Store client
        {
            let mut client_lock = self.client.write().await;
            *client_lock = Some(client);
        }

        // Send initialized notification
        self.send_initialized().await?;

        debug!("LSP server initialized successfully");
        Ok(())
    }

    /// Send initialize request
    async fn initialize(
        &self,
        client: &LspClient,
        workspace_root: &Path,
    ) -> Result<InitializeResult> {
        let root_uri = format!("file://{}", workspace_root.display());

        let params = InitializeParams {
            process_id: Some(std::process::id() as i32),
            root_path: Some(workspace_root.to_string_lossy().into_owned()),
            root_uri: Some(root_uri.clone()),
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    call_hierarchy: Some(CallHierarchyClientCapabilities {
                        dynamic_registration: false,
                    }),
                }),
                workspace: None,
            },
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: workspace_root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "workspace".to_string()),
            }]),
        };

        let response = client.request("initialize", Some(json!(params))).await?;

        let result = response.into_result().map_err(|e| Error::Rpc {
            code: e.code,
            message: e.message,
        })?;

        let init_result: InitializeResult = serde_json::from_value(result)?;
        Ok(init_result)
    }

    /// Send initialized notification
    async fn send_initialized(&self) -> Result<()> {
        let client = self.client.read().await;
        if let Some(client) = client.as_ref() {
            client.notify("initialized", Some(json!({}))).await?;
        }
        Ok(())
    }

    /// Get the active client (if available)
    pub async fn client(&self) -> Result<tokio::sync::RwLockReadGuard<'_, Option<LspClient>>> {
        let client = self.client.read().await;
        if client.is_none() {
            return Err(Error::ServerNotAvailable("No active client".into()));
        }
        Ok(client)
    }

    /// Stop the LSP server
    pub async fn stop(&self) -> Result<()> {
        let mut client_lock = self.client.write().await;
        if let Some(mut client) = client_lock.take() {
            // Send shutdown request
            let _ = client.request("shutdown", None).await;
            // Send exit notification
            let _ = client.notify("exit", None).await;
            // Kill the process
            let _ = client.kill();
        }

        let mut state = self.state.write().await;
        *state = LspState::Stopped;

        info!("LSP server stopped");
        Ok(())
    }

    /// Restart the LSP server
    pub async fn restart(&self, workspace_root: &Path) -> Result<()> {
        warn!("Restarting LSP server");
        self.stop().await?;

        // Reset state
        {
            let mut state = self.state.write().await;
            *state = LspState::NotStarted;
        }

        self.start(workspace_root).await
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        // Best effort cleanup - can't await in drop
        // The LspClient's drop will handle killing the process
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> LspConfig {
        LspConfig {
            enabled: true,
            command: "pylsp".to_string(),
            args: vec![],
            max_call_depth: 10,
            analysis_timeout_ms: 5000,
            cache_enabled: true,
            working_directory: None,
        }
    }

    #[tokio::test]
    async fn test_manager_initial_state() {
        let manager = LspManager::new(test_config());
        assert_eq!(manager.state().await, LspState::NotStarted);
        assert!(!manager.is_ready().await);
    }

    #[tokio::test]
    async fn test_manager_supports_call_hierarchy_without_caps() {
        let manager = LspManager::new(test_config());
        assert!(!manager.supports_call_hierarchy().await);
    }

    #[tokio::test]
    async fn test_lsp_state_equality() {
        assert_eq!(LspState::NotStarted, LspState::NotStarted);
        assert_eq!(LspState::Ready, LspState::Ready);
        assert_ne!(LspState::NotStarted, LspState::Ready);
        assert_eq!(
            LspState::Failed("test".into()),
            LspState::Failed("test".into())
        );
    }

    #[test]
    fn test_workspace_folder_creation() {
        let path = PathBuf::from("/home/user/project");
        let uri = format!("file://{}", path.display());
        let folder = WorkspaceFolder {
            uri: uri.clone(),
            name: "project".to_string(),
        };
        assert_eq!(folder.uri, "file:///home/user/project");
        assert_eq!(folder.name, "project");
    }
}
