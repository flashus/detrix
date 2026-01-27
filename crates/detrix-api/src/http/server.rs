//! HTTP server lifecycle management

use super::routes::{create_router_with_config, create_router_with_jwt_validator};
use crate::state::ApiState;
use detrix_application::JwksValidator;
// IntoMakeServiceWithConnectInfo is used via the Router::into_make_service_with_connect_info method
#[allow(unused_imports)]
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::info;

/// HTTP server for REST, WebSocket, and MCP HTTP
pub struct HttpServer {
    addr: SocketAddr,
    state: Arc<ApiState>,
    /// JWT validator for external auth mode (optional)
    jwt_validator: Option<JwksValidator>,
}

impl HttpServer {
    /// Create a new HTTP server
    pub fn new(addr: SocketAddr, state: Arc<ApiState>) -> Self {
        Self {
            addr,
            state,
            jwt_validator: None,
        }
    }

    /// Create a new HTTP server with JWT validator for external auth mode
    pub fn with_jwt_validator(
        addr: SocketAddr,
        state: Arc<ApiState>,
        jwt_validator: JwksValidator,
    ) -> Self {
        Self {
            addr,
            state,
            jwt_validator: Some(jwt_validator),
        }
    }

    /// Start the HTTP server
    ///
    /// Returns a handle to the server task
    pub async fn start(self) -> anyhow::Result<JoinHandle<()>> {
        info!("🌐 Starting HTTP server on {}...", self.addr);

        // Get config from ConfigService for router construction
        // Note: These middleware settings (rate limit, auth, cors) are static after server start
        let config = self.state.config_service.get_config().await;
        let rate_limit = &config.api.rest.rate_limit;
        let auth = &config.api.auth;
        let cors = &config.api.rest.cors;
        let router = match self.jwt_validator {
            Some(validator) => create_router_with_jwt_validator(
                Arc::clone(&self.state),
                rate_limit,
                auth,
                cors,
                Some(validator),
            )?,
            None => create_router_with_config(Arc::clone(&self.state), rate_limit, auth, cors)?,
        };

        // Bind TCP listener
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind HTTP server: {}", e))?;

        info!("✓ HTTP server listening on {}", self.addr);
        info!("   REST API: http://{}/api/v1/metrics", self.addr);
        info!("   WebSocket: ws://{}/ws", self.addr);
        info!("   MCP HTTP: http://{}/mcp", self.addr);
        info!("   Health: http://{}/health", self.addr);

        // Spawn server task
        // NOTE: Must use into_make_service_with_connect_info for rate limiting to work
        // The PeerIpKeyExtractor requires SocketAddr to be available in request extensions
        let handle = tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                tracing::error!("HTTP server error: {}", e);
            }
        });

        Ok(handle)
    }

    /// Start the HTTP server with graceful shutdown
    ///
    /// Returns a handle to the server task that will shutdown when the shutdown signal fires
    pub async fn start_with_shutdown(
        self,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<JoinHandle<()>> {
        info!("🌐 Starting HTTP server on {}...", self.addr);
        info!("📍 About to create router with state: {:?}", self.state);

        // Get config from ConfigService for router construction
        // Note: These middleware settings (rate limit, auth, cors) are static after server start
        let config = self.state.config_service.get_config().await;
        let rate_limit = &config.api.rest.rate_limit;
        let auth = &config.api.auth;
        let cors = &config.api.rest.cors;
        let router = match self.jwt_validator {
            Some(validator) => create_router_with_jwt_validator(
                Arc::clone(&self.state),
                rate_limit,
                auth,
                cors,
                Some(validator),
            )?,
            None => create_router_with_config(Arc::clone(&self.state), rate_limit, auth, cors)?,
        };

        info!("📍 Router created, binding TCP listener...");

        // Bind TCP listener
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind HTTP server: {}", e))?;

        info!("✓ HTTP server listening on {}", self.addr);
        info!("   REST API: http://{}/api/v1/metrics", self.addr);
        info!("   WebSocket: ws://{}/ws", self.addr);
        info!("   MCP HTTP: http://{}/mcp", self.addr);
        info!("   Health: http://{}/health", self.addr);

        // Spawn server task with graceful shutdown
        let handle = tokio::spawn(async move {
            let shutdown_signal = async {
                let mut rx = shutdown_rx;
                loop {
                    if *rx.borrow() {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
                info!("HTTP server received shutdown signal");
            };

            // NOTE: Must use into_make_service_with_connect_info for rate limiting to work
            // The PeerIpKeyExtractor requires SocketAddr to be available in request extensions
            if let Err(e) = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal)
            .await
            {
                tracing::error!("HTTP server error: {}", e);
            }

            info!("HTTP server stopped");
        });

        Ok(handle)
    }
}
