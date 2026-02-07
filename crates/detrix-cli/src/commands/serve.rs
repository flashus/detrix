//! Serve command - Start Detrix server and set up metrics
//!
//! This command starts the Detrix daemon WITHOUT blocking on debugger connections.
//! The daemon starts HTTP/gRPC servers immediately, and debugger connections
//! are managed separately via ConnectionService and API endpoints.

use crate::utils::init::{init_infrastructure, InitOptions};
use crate::utils::pid::PidFile;
use anyhow::{Context, Result};
use detrix_api::generated::detrix::v1::{
    connection_service_server::ConnectionServiceServer,
    metrics_service_server::MetricsServiceServer, streaming_service_server::StreamingServiceServer,
};
use detrix_api::grpc::{
    create_auth_interceptor, AuthInterceptorState, ConnectionServiceImpl, MetricsServiceImpl,
    StreamingServiceImpl,
};
use detrix_api::http::HttpServer;
use detrix_api::tonic::transport::Server;
use detrix_api::ApiState;
use detrix_application::{
    EventRepositoryRef, JwksValidator, McpUsageRepositoryRef, SystemEventRepositoryRef,
    SystemEventService,
};
use detrix_config::{PortRegistry, ServiceType};
use detrix_core::ParseLanguageExt;
#[allow(unused_imports)]
use detrix_logging::{debug, error, info, warn};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config_path: &str,
    _script: Option<String>,
    port: u16,
    grpc: bool,
    grpc_port: u16,
    daemon: bool,
    pid_file: Option<String>,
    mcp_spawned: bool,
) -> Result<()> {
    // Log executable path for debugging (helps identify which binary is running)
    if let Ok(exe_path) = std::env::current_exe() {
        info!("Detrix daemon starting (exe: {})", exe_path.display());
    }

    // Log if this daemon was spawned by MCP
    if mcp_spawned {
        info!("Daemon spawned by MCP client (auto-spawn)");
    }
    // HTTP server configuration (always enabled for protocol multiplexing)
    let http_enabled = true;

    // Acquire PID file if daemon mode is enabled (must do this BEFORE port allocation
    // so we can store the port in the PID file)
    let mut pid_file_guard = if daemon {
        let pid_path = pid_file
            .map(PathBuf::from)
            .unwrap_or_else(detrix_config::paths::default_pid_path);

        info!("🔒 Acquiring PID file: {}", pid_path.display());

        let guard = PidFile::acquire(&pid_path)
            .context("Failed to acquire PID file - another instance may be running")?;

        info!("✓ PID file acquired (PID: {})", std::process::id());
        Some(guard)
    } else {
        None
    };

    // Load configuration early to check port_fallback setting
    let mut config = detrix_config::load_config(Path::new(config_path))?;

    // Clean up old log files based on retention policy
    if daemon && config.daemon.logging.file_logging_enabled {
        let log_dir = &config.daemon.logging.log_dir;
        let retention_days = config.daemon.logging.log_retention_days;

        match detrix_logging::cleanup_old_logs(log_dir, retention_days) {
            Ok(0) => {
                debug!("No old log files to clean up");
            }
            Ok(n) => {
                info!("🧹 Cleaned up {} old log file(s)", n);
            }
            Err(e) => {
                warn!("Failed to clean up old logs: {}", e);
            }
        }
    }

    // Initialize AST cache with configured max_entries (must be done before first safety check)
    if detrix_application::safety::treesitter::init_global_cache_from_config(&config.safety) {
        info!(
            max_entries = config.safety.ast_cache_max_entries,
            "✓ AST cache initialized"
        );
    }

    // Auto-generate auth token for MCP-spawned daemons (transparent local auth)
    // This ensures local MCP clients can authenticate without manual config
    if mcp_spawned && !config.api.auth.is_enabled() {
        let auto_token = generate_secure_token();
        let token_path = detrix_config::paths::mcp_token_path();

        // Ensure parent directory exists
        if let Err(e) = detrix_config::paths::ensure_parent_dir(&token_path) {
            warn!("Failed to create token directory: {}", e);
        } else {
            // Write token to file with restricted permissions (atomically where possible)
            if let Err(e) = write_token_securely(&token_path, &auto_token) {
                warn!("Failed to write MCP token file: {}", e);
            } else {
                // Enable auth with auto-generated token (simple mode)
                config.api.auth.mode = detrix_config::AuthMode::Simple;
                config.api.auth.bearer_token = Some(auto_token);

                info!(
                    "🔐 MCP auto-auth enabled, token saved to {}",
                    token_path.display()
                );
            }
        }
    }

    // Determine if gRPC should be enabled (CLI flag OR config setting)
    let grpc_enabled = grpc || config.api.grpc.enabled;

    // Create PortRegistry for centralized port management
    let mut port_registry = PortRegistry::new();
    let fallback_enabled = config.api.port_fallback;

    // Register HTTP port (always needed)
    port_registry.register(ServiceType::Http, config.api.rest.port, fallback_enabled);

    // Register gRPC port (use CLI override if provided, otherwise config)
    let preferred_grpc_port = if grpc {
        grpc_port
    } else {
        config.api.grpc.port
    };
    port_registry.register(ServiceType::Grpc, preferred_grpc_port, fallback_enabled);

    // Allocate HTTP port
    let http_port = port_registry
        .allocate(ServiceType::Http)
        .context("Failed to allocate HTTP port")?;

    if http_port != config.api.rest.port {
        warn!(
            "⚠️  Port {} is unavailable (in use by another process), using port {} instead. \
             Set 'api.port_fallback = false' in config to fail instead of auto-selecting a port.",
            config.api.rest.port, http_port
        );
    }

    // Allocate gRPC port (if gRPC is enabled)
    let grpc_port = if grpc_enabled {
        let port = port_registry
            .allocate(ServiceType::Grpc)
            .context("Failed to allocate gRPC port")?;

        if port != preferred_grpc_port {
            warn!(
                "⚠️  gRPC port {} is unavailable, using port {} instead.",
                preferred_grpc_port, port
            );
        }
        port
    } else {
        preferred_grpc_port // Won't be used, but needed for later reference
    };

    // Store all ports and host in the PID file so clients can discover them
    if let Some(ref mut guard) = pid_file_guard {
        guard
            .set_ports_with_host(&port_registry, config.api.rest.host.clone())
            .context("Failed to store ports in PID file")?;
        if grpc_enabled {
            info!(
                "✓ Ports stored in PID file (host: {}, HTTP: {}, gRPC: {})",
                config.api.rest.host, http_port, grpc_port
            );
        } else {
            info!(
                "✓ Port {} stored in PID file (host: {})",
                http_port, config.api.rest.host
            );
        }
    }
    info!("🚀 Starting Detrix server...");
    info!("✓ Configuration loaded from {}", config_path);
    info!("📁 Database path: {:?}", config.storage.path);

    // Build GELF output if configured
    let gelf_output = crate::utils::output::build_gelf_output(&config.output.gelf)
        .await
        .context("Failed to initialize GELF output")?;

    // Initialize infrastructure using centralized initialization
    // Use config_path's parent directory as base for resolving relative paths
    let config_dir = Path::new(config_path).parent().unwrap_or(Path::new("."));
    let infra = init_infrastructure(&config, config_dir, InitOptions::from_config(&config)).await?;

    // Create application context from infrastructure components
    let ctx = infra.into_app_context(
        &config.api,
        &config.safety,
        &config.storage,
        &config.daemon,
        &config.adapter,
        &config.anchor,
        &config.limits,
        gelf_output.clone(),
    );
    let app_context = ctx.app_context;
    let storage = ctx.storage;

    // Load metrics from config into database if needed (via MetricService)
    let metrics = app_context
        .metric_service
        .list_metrics()
        .await
        .context("Failed to load metrics from storage")?;

    if metrics.is_empty() && !config.metric.is_empty() {
        info!(
            "📝 Loading {} metrics from config into database...",
            config.metric.len()
        );

        // Import metrics via MetricService (storage only, no adapter calls)
        // Logpoints will be set when a debugger connection is established
        for metric_def in &config.metric {
            let metric = detrix_core::Metric {
                id: None,
                name: metric_def.name.clone(),
                connection_id: detrix_core::ConnectionId::from(metric_def.connection_id.as_str()),
                group: metric_def.group.clone(),
                location: metric_def.location.clone(),
                expression: metric_def.expression.clone(),
                language: metric_def.language.parse_language().with_context(|| {
                    format!("Invalid language for metric '{}'", metric_def.name)
                })?,
                mode: metric_def.mode.clone(),
                enabled: metric_def.enabled,
                condition: metric_def.condition.clone(),
                safety_level: metric_def.safety_level,
                created_at: Some(chrono::Utc::now().timestamp_micros()),
                // Default values for introspection fields (loaded from config later if needed)
                capture_stack_trace: false,
                stack_trace_ttl: None,
                stack_trace_slice: None,
                capture_memory_snapshot: false,
                snapshot_scope: None,
                snapshot_ttl: None,
                // Anchor tracking defaults
                anchor: None,
                anchor_status: Default::default(),
            };

            app_context
                .metric_service
                .import_metric(metric)
                .await
                .context(format!("Failed to import metric: {}", metric_def.name))?;

            info!("  ✓ Imported metric: {}", metric_def.name);
        }
    }

    // Reload metrics after importing from config
    let metrics = app_context
        .metric_service
        .list_metrics()
        .await
        .context("Failed to reload metrics from storage")?;

    if metrics.is_empty() {
        info!("📝 No metrics in config - add them via API or connect to a debugger");
    } else {
        info!(
            "✓ Loaded {} metrics (will be synced when debugger connects)",
            metrics.len()
        );
    }

    // NOTE: No blocking wait for debugger!
    // Users connect debuggers via 'connect' command or API endpoint.
    info!("🔌 To connect a debugger:");
    info!(
        "   1. Run: python -m debugpy --listen {} your_script.py",
        port
    );
    info!("   2. Use: detrix connect localhost:{}", port);
    info!("");

    // Create API state from the pre-configured AppContext
    // This ensures the connection_service is available in the API layer
    // Pass mcp_spawned flag to enable auto-shutdown when all MCP clients disconnect
    let api_state = Arc::new(
        ApiState::builder(
            app_context.clone(),
            Arc::clone(&storage) as EventRepositoryRef,
        )
        .full_config(config.clone())
        .config_path(PathBuf::from(config_path))
        .mcp_spawned(mcp_spawned)
        .system_event_repository(Arc::clone(&storage) as SystemEventRepositoryRef)
        .mcp_usage_repository(Arc::clone(&storage) as McpUsageRepositoryRef)
        .build(),
    );

    // Shutdown coordination channel
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Create JWT validator for external auth mode (if configured)
    let jwt_validator = if config.api.auth.mode == detrix_config::AuthMode::External {
        info!("🔑 Creating JWT validator for external auth mode...");
        match JwksValidator::new(&config.api.auth.jwt) {
            Ok(validator) => {
                info!(
                    jwks_url = ?config.api.auth.jwt.jwks_url,
                    "✓ JWT validator created"
                );
                Some(validator)
            }
            Err(e) => {
                error!(error = %e, "Failed to create JWT validator");
                return Err(anyhow::anyhow!(
                    "External auth mode requires valid JWT configuration: {}",
                    e
                ));
            }
        }
    } else {
        None
    };

    // Restore connections from previous sessions in the BACKGROUND
    // This allows the HTTP server to start immediately without blocking on
    // reconnection attempts that may take a long time (especially if debuggers are not running)
    // - If debugger is still running → reconnect
    // - If debugger is gone → delete the connection
    {
        let connection_service = Arc::clone(&app_context.connection_service);
        tokio::spawn(async move {
            let (reconnected, deleted) = connection_service.restore_connections_on_startup().await;
            if reconnected > 0 || deleted > 0 {
                info!(
                    "🔄 Connection restore: {} reconnected, {} removed (debuggers not running)",
                    reconnected, deleted
                );
            }
        });
    }

    // Start HTTP server (REST, WebSocket, MCP HTTP)
    let http_handle = if http_enabled {
        info!("🌐 Starting HTTP server on port {}...", http_port);

        let http_addr: SocketAddr = format!("{}:{}", config.api.rest.host, http_port)
            .parse()
            .context("Invalid HTTP address")?;

        // Create HTTP server with JWT validator if in external mode
        let http_server = match &jwt_validator {
            Some(_) => {
                // Create a separate validator for HTTP server (gRPC will use the original)
                let http_validator = JwksValidator::new(&config.api.auth.jwt)
                    .context("Failed to create HTTP JWT validator")?;
                HttpServer::with_jwt_validator(http_addr, Arc::clone(&api_state), http_validator)
            }
            None => HttpServer::new(http_addr, Arc::clone(&api_state)),
        };

        let handle = http_server
            .start_with_shutdown(shutdown_rx.clone())
            .await
            .context("Failed to start HTTP server")?;

        Some(handle)
    } else {
        None
    };

    // Start gRPC server if enabled (via --grpc flag or config)
    let grpc_handle = if grpc_enabled {
        info!("📡 Starting gRPC server on port {}...", grpc_port);

        // Create gRPC services
        let metrics_service = MetricsServiceImpl::new(Arc::clone(&api_state));
        let streaming_service = StreamingServiceImpl::new(Arc::clone(&api_state));
        let connection_service = ConnectionServiceImpl::new(Arc::clone(&api_state));

        // Create auth interceptor for gRPC (mirrors HTTP auth middleware)
        let grpc_auth_state = match jwt_validator {
            Some(validator) => {
                AuthInterceptorState::with_jwt_validator(config.api.auth.clone(), validator)
            }
            None => AuthInterceptorState::new(config.api.auth.clone()),
        };
        let auth_interceptor = create_auth_interceptor(grpc_auth_state);

        if config.api.auth.is_enabled() {
            info!(mode = ?config.api.auth.mode, "✓ gRPC authentication enabled");
        } else {
            info!("✓ gRPC authentication disabled (all endpoints public)");
        }

        let grpc_addr: SocketAddr = format!("{}:{}", config.api.grpc.host, grpc_port)
            .parse()
            .context("Invalid gRPC address")?;

        info!("✓ gRPC server listening on {}", grpc_addr);

        // Spawn gRPC server with graceful shutdown
        let shutdown_signal = {
            let mut rx = shutdown_rx.clone();
            async move {
                loop {
                    if *rx.borrow() {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            }
        };

        let handle = tokio::spawn(async move {
            if let Err(e) = Server::builder()
                .add_service(MetricsServiceServer::with_interceptor(
                    metrics_service,
                    auth_interceptor.clone(),
                ))
                .add_service(StreamingServiceServer::with_interceptor(
                    streaming_service,
                    auth_interceptor.clone(),
                ))
                .add_service(ConnectionServiceServer::with_interceptor(
                    connection_service,
                    auth_interceptor,
                ))
                .serve_with_shutdown(grpc_addr, shutdown_signal)
                .await
            {
                error!("gRPC server error: {}", e);
            }
        });

        Some(handle)
    } else {
        None
    };

    // Start MCP client cleanup task (removes stale clients)
    let _mcp_cleanup_handle = api_state.start_mcp_cleanup_task();
    let mut mcp_shutdown_rx = api_state.mcp_shutdown_receiver();

    // Start system event retention cleanup task
    let retention_config = &config.storage.system_event_retention;
    let system_event_service =
        SystemEventService::new(Arc::clone(&storage) as SystemEventRepositoryRef);
    let _retention_cleanup_handle = system_event_service.start_retention_cleanup_task(
        retention_config.retention_hours,
        retention_config.cleanup_interval_secs,
        retention_config.max_events,
        shutdown_rx.clone(),
    );

    // Start system event persistence task (subscribes to broadcast, persists to DB)
    let mut system_event_rx = api_state.subscribe_system_events();
    let persist_service = system_event_service.clone();
    let mut persist_shutdown_rx = shutdown_rx.clone();
    let _system_event_persist_handle = tokio::spawn(async move {
        info!("Starting system event persistence task");
        loop {
            tokio::select! {
                result = system_event_rx.recv() => {
                    match result {
                        Ok(event) => {
                            if let Err(e) = persist_service.capture_event(&event).await {
                                error!(error = %e, event_type = %event.event_type.as_str(), "Failed to persist system event");
                            } else {
                                info!(event_type = %event.event_type.as_str(), "Persisted system event");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(missed = n, "System event persistence lagged behind");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("System event channel closed");
                            break;
                        }
                    }
                }
                _ = persist_shutdown_rx.changed() => {
                    if *persist_shutdown_rx.borrow() {
                        info!("System event persistence task shutting down");
                        break;
                    }
                }
            }
        }
    });

    // Start file watcher orchestrator for auto-relocation (if enabled)
    let _file_watcher_handle = if config.anchor.enabled && config.anchor.auto_relocate {
        use detrix_application::services::FileWatcherOrchestrator;

        match FileWatcherOrchestrator::new(
            Arc::clone(&app_context.metric_service),
            config.anchor.clone(),
        ) {
            Ok((orchestrator, event_rx)) => {
                let orchestrator: Arc<FileWatcherOrchestrator> = Arc::new(orchestrator);

                // Start watching paths
                if let Err(e) = orchestrator.start_watching().await {
                    warn!(error = %e, "Failed to start file watching");
                }

                // Spawn event loop
                let handle = orchestrator.spawn(event_rx, shutdown_rx.clone());
                info!("✓ File watcher started for auto-relocation");
                Some(handle)
            }
            Err(e) => {
                warn!(error = %e, "Failed to create file watcher orchestrator");
                None
            }
        }
    } else {
        None
    };

    info!("📊 Ready for connections... (Ctrl+C to stop)");
    if http_enabled {
        info!("   HTTP API available at http://localhost:{}", http_port);
    }
    if grpc_enabled {
        info!("   gRPC API available at localhost:{}", grpc_port);
    }
    if mcp_spawned {
        info!("   (MCP-spawned daemon - will auto-shutdown when all MCP clients disconnect)");
    }
    info!("");

    // NOTE: Event broadcasting is handled automatically via the shared broadcast channel.
    // AdapterLifecycleManager publishes events to StreamingService.event_sender(),
    // and WebSocket/gRPC subscribers subscribe to the same channel via ApiState.event_tx.
    // No republisher task is needed - the channel is shared.

    // Wait for shutdown signal (Ctrl+C, SIGTERM, or MCP auto-shutdown)
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm =
            signal(SignalKind::terminate()).context("Failed to install SIGTERM handler")?;
        let mut sigint =
            signal(SignalKind::interrupt()).context("Failed to install SIGINT handler")?;

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT (Ctrl+C)");
            }
            _ = mcp_shutdown_rx.changed() => {
                if *mcp_shutdown_rx.borrow() {
                    info!("All MCP clients disconnected - auto-shutdown triggered");
                }
            }
        }
    }

    #[cfg(windows)]
    {
        use tokio::signal::windows;

        // Create signal handlers for Windows
        // ctrl_c: Ctrl+C (SIGINT equivalent)
        // ctrl_break: Ctrl+Break (useful for services)
        let mut ctrl_c = windows::ctrl_c().context("Failed to install Ctrl+C handler")?;
        let mut ctrl_break =
            windows::ctrl_break().context("Failed to install Ctrl+Break handler")?;

        tokio::select! {
            _ = ctrl_c.recv() => {
                info!("Received Ctrl+C");
            }
            _ = ctrl_break.recv() => {
                info!("Received Ctrl+Break");
            }
            _ = mcp_shutdown_rx.changed() => {
                if *mcp_shutdown_rx.borrow() {
                    info!("All MCP clients disconnected - auto-shutdown triggered");
                }
            }
        }
    }

    // Fallback for other non-unix platforms (unlikely but handles edge cases)
    #[cfg(all(not(unix), not(windows)))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Received Ctrl+C");
            }
            _ = mcp_shutdown_rx.changed() => {
                if *mcp_shutdown_rx.borrow() {
                    info!("All MCP clients disconnected - auto-shutdown triggered");
                }
            }
        }
    }

    info!("");
    info!("🛑 Shutting down...");

    // Signal shutdown to all servers
    let _ = shutdown_tx.send(true);

    // Wait for HTTP server to stop
    if let Some(handle) = http_handle {
        info!("✓ HTTP server stopping...");
        let _ = handle.await;
        info!("✓ HTTP server stopped");
    }

    // Wait for gRPC server to stop
    if let Some(handle) = grpc_handle {
        info!("✓ gRPC server stopping...");
        let _ = handle.await;
        info!("✓ gRPC server stopped");
    }

    // Stop all adapters via lifecycle manager
    if let Err(e) = app_context.adapter_lifecycle_manager.stop_all().await {
        error!("Error stopping adapters: {}", e);
    }
    info!("✓ All adapters stopped");

    // Clean up token file if daemon was MCP-spawned
    // This prevents stale tokens from being used after restart
    if mcp_spawned {
        let token_path = detrix_config::paths::mcp_token_path();
        match std::fs::remove_file(&token_path) {
            Ok(()) => {
                debug!("✓ Removed MCP token file: {}", token_path.display());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Token file already gone, that's fine
            }
            Err(e) => {
                warn!("Could not remove MCP token file: {}", e);
            }
        }
    }

    // PID file will be automatically released when pid_file_guard is dropped
    if daemon {
        info!("✓ PID file released");
    }

    info!("✓ Detrix server stopped");
    Ok(())
}

/// Generate a cryptographically secure random token for MCP auto-auth.
///
/// Returns a 64-character hex string (256 bits of entropy) using a
/// cryptographically secure random number generator.
fn generate_secure_token() -> String {
    use rand::Rng;

    let mut rng = rand::rng();
    (0..32)
        .map(|_| format!("{:02x}", rng.random::<u8>()))
        .collect()
}

/// Write token to file securely with platform-specific permissions.
///
/// On Unix: Creates file with mode 0600 (owner read/write only) atomically.
/// On Windows: Creates file then applies restrictive ACL (owner-only access).
fn write_token_securely(token_path: &std::path::Path, token: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        // Create file with restricted permissions atomically (avoids race condition)
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600) // Owner read/write only - set at creation time
            .open(token_path)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::io::Write;

        // Write the file first
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(token_path)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        drop(file); // Release file handle before ACL change

        // Apply Windows ACL using icacls (equivalent to Unix 0600)
        // This removes inherited permissions and grants full control only to current user
        let path_str = token_path.to_string_lossy();

        // Get current username for ACL
        let username = std::env::var("USERNAME").unwrap_or_else(|_| "CURRENT_USER".to_string());

        // Apply restrictive ACL:
        // /inheritance:r = Remove inherited permissions
        // /grant:r = Reset and grant specified permissions
        // :F = Full control
        let output = std::process::Command::new("icacls")
            .args([
                path_str.as_ref(),
                "/inheritance:r",           // Remove inherited ACLs
                "/grant:r",                 // Reset permissions
                &format!("{}:F", username), // Grant full control to current user
            ])
            .output();

        match output {
            Ok(result) if result.status.success() => {
                debug!("Token file ACL hardened successfully");
            }
            Ok(result) => {
                // Log warning but don't fail - file was still created
                warn!(
                    "icacls command failed (exit code {:?}): {}",
                    result.status.code(),
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            Err(e) => {
                // icacls might not be available, log but don't fail
                warn!("Failed to harden token file ACL: {}", e);
            }
        }

        Ok(())
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        // Fallback for other platforms - just write the file
        std::fs::write(token_path, token)
    }
}
