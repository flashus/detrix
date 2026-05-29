//! AgentFileSource — remote file source for agent-managed connections.
//!
//! Fetches file content from the agent via the gRPC stream.
//! Used by the VFS when serving file requests for agent connections.

use async_trait::async_trait;
use detrix_core::{Connection, Result};
use detrix_logging::warn;
use std::time::Duration;
use uuid::Uuid;

use crate::services::agent_connection_manager::{
    AgentConnectionManagerRef, IncomingAgentMessage, OutgoingAgentMessage,
};
use detrix_ports::{FetchResult, FileSource, SourceMetadata};

const READ_FILE_TIMEOUT_SECS: u64 = 10;

/// Fetches files from agent-managed connections by sending ReadFile commands
/// to the agent via the gRPC stream.
pub struct AgentFileSource {
    agent_manager: AgentConnectionManagerRef,
}

impl AgentFileSource {
    pub fn new(agent_manager: AgentConnectionManagerRef) -> Self {
        Self { agent_manager }
    }
}

#[async_trait]
impl FileSource for AgentFileSource {
    fn name(&self) -> &str {
        "agent"
    }

    async fn fetch(&self, connection: &Connection, file_path: &str) -> Result<Option<FetchResult>> {
        if !self.agent_manager.is_agent_managed(&connection.id) {
            return Ok(None);
        }

        let msg = OutgoingAgentMessage::ReadFile {
            request_id: format!("readfile-{}-{}", connection.id.0, Uuid::new_v4()),
            connection_id: connection.id.0.clone(),
            path: file_path.to_string(),
        };

        let response = match self
            .agent_manager
            .send_and_await_raw(
                &connection.id,
                msg,
                Duration::from_secs(READ_FILE_TIMEOUT_SECS),
            )
            .await
        {
            Ok(IncomingAgentMessage::FileResponse { content, error, .. }) => {
                if let Some(err) = error {
                    warn!(
                        file = file_path,
                        error = %err,
                        "Agent ReadFile failed"
                    );
                    return Ok(None);
                }
                content
            }
            Ok(other) => {
                warn!(
                    file = file_path,
                    response_type = ?other,
                    "Unexpected response to ReadFile"
                );
                return Ok(None);
            }
            Err(e) => {
                warn!(
                    file = file_path,
                    error = %e,
                    "Agent ReadFile request failed"
                );
                return Ok(None);
            }
        };

        Ok(Some(FetchResult {
            content: String::from_utf8_lossy(&response).into_owned(),
            metadata: SourceMetadata {
                source_kind: "agent".to_string(),
                ..Default::default()
            },
        }))
    }
}
