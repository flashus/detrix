//! Remote app control port
//!
//! Abstracts the ability to wake/sleep remote applications via their control planes.
//! The implementation handles HTTP communication; this port defines the contract.

use async_trait::async_trait;

/// Response from waking a remote app
#[derive(Debug, Clone)]
pub struct RemoteWakeResponse {
    /// The app URL that was woken
    pub app_url: String,
    /// Status reported by the app (e.g., "ok", "already_awake")
    pub status: String,
    /// Connection ID if the app registered with the daemon
    pub connection_id: Option<String>,
    /// Debug port the app's debugger is listening on
    pub debug_port: Option<i32>,
}

/// Response from sleeping a remote app
#[derive(Debug, Clone)]
pub struct RemoteSleepResponse {
    /// The app URL that was put to sleep
    pub app_url: String,
    /// Status reported by the app (e.g., "sleeping")
    pub status: String,
}

/// Port trait for controlling remote applications via their control planes.
///
/// Implementations send HTTP requests to `{app_url}/detrix/wake` and
/// `{app_url}/detrix/sleep` endpoints. This port isolates the HTTP
/// communication from the application layer.
#[async_trait]
pub trait RemoteAppControl: Send + Sync {
    /// Wake a remote app by calling its control plane.
    ///
    /// Sends POST to `{app_url}/detrix/wake` with optional `daemon_url` in body.
    /// The `auth_token` is sent as a Bearer token if provided.
    async fn wake_app(
        &self,
        app_url: &str,
        daemon_url: Option<&str>,
        auth_token: Option<&str>,
    ) -> detrix_core::Result<RemoteWakeResponse>;

    /// Sleep a remote app by calling its control plane.
    ///
    /// Sends POST to `{app_url}/detrix/sleep`.
    /// The `auth_token` is sent as a Bearer token if provided.
    async fn sleep_app(
        &self,
        app_url: &str,
        auth_token: Option<&str>,
    ) -> detrix_core::Result<RemoteSleepResponse>;
}
