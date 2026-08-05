//! Serve command - Start Detrix server and set up metrics
//!
//! This command starts the Detrix daemon WITHOUT blocking on debugger connections.
//! The daemon starts HTTP/gRPC servers immediately, and debugger connections
//! are managed separately via ConnectionService and API endpoints.

use crate::utils::init::{init_infrastructure, InitOptions};
use crate::utils::pid::PidFile;
use anyhow::{Context, Result};
use detrix_api::generated::detrix::v1::{
    agent_service_server::AgentServiceServer, connection_service_server::ConnectionServiceServer,
    metrics_service_server::MetricsServiceServer, streaming_service_server::StreamingServiceServer,
};
use detrix_api::grpc::{
    agent::AgentServiceImpl, create_agent_auth_interceptor, create_auth_interceptor,
    AuthInterceptorState, ConnectionServiceImpl, MetricsServiceImpl, StreamingServiceImpl,
};
use detrix_api::http::HttpServer;
use detrix_api::tonic::transport::Server;
use detrix_api::ApiState;
use detrix_application::{
    EventRepositoryRef, JwksValidator, McpUsageRepositoryRef, SystemEventRepositoryRef,
    SystemEventService,
};
use detrix_config::{PortRegistry, ServiceType, AUTO_AUTH_DEFAULT_USER_ID};
use detrix_core::ParseLanguageExt;
#[allow(unused_imports)]
use detrix_logging::{debug, error, info, warn};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;

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
    // Install SIGTERM handler with SA_SIGINFO as early as possible to capture sender PID.
    // Must be done before PID file acquisition — otherwise the default SIGTERM action
    // (terminate) can kill the daemon during the startup window, leaving a zombie that
    // appears alive to kill(pid, 0) but doesn't hold the flock.
    #[cfg(unix)]
    let sigterm_pipe_fd =
        sigterm_info::install().context("Failed to install early SIGTERM info handler")?;

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

    // DETRIX_CONFIG env var overrides the --config flag (useful for Docker restart tests
    // that need to switch config without rebuilding the image).
    let config_path_override;
    let config_path = if let Some(env_path) = std::env::var("DETRIX_CONFIG")
        .ok()
        .filter(|s| !s.is_empty())
    {
        config_path_override = env_path;
        &config_path_override
    } else {
        config_path
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

    // Auto-generate auth token for all daemons when auth is not explicitly configured.
    // This ensures daemons are secure by default — clients discover the token via
    // DETRIX_TOKEN env var or ~/detrix/auth-token file.
    // `our_written_token` holds the token we wrote to auth-token, if any.
    // Kept until shutdown so we can verify the file still contains our token
    // before deleting — concurrent test daemons overwrite the same file.
    let mut our_written_token: Option<String> = None;
    if config.api.auth.mode.is_none() {
        // Use DETRIX_TOKEN env var if set, otherwise generate a new one
        let (auto_token, from_env) = match std::env::var("DETRIX_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        {
            Some(t) => (t, true),
            None => (generate_secure_token(), false),
        };

        // Write token file for both auto-generated and env-supplied tokens so that
        // other bridge instances running in different processes can discover it.
        // Note: Single token file means one daemon per machine for auto-auth.
        // For concurrent daemons, use explicit [api.auth] config.
        let token_path = detrix_config::paths::auth_token_path();
        if let Err(e) = detrix_config::paths::ensure_parent_dir(&token_path) {
            warn!("Failed to create token directory: {}", e);
        } else if let Err(e) = write_token_securely(&token_path, &auto_token) {
            warn!("Failed to write auth token file: {}", e);
        } else {
            if from_env {
                info!(
                    "🔐 Auth token (from DETRIX_TOKEN) saved to {}",
                    token_path.display()
                );
            } else {
                info!("🔐 Auth token saved to {}", token_path.display());
            }
            our_written_token = Some(auto_token.clone());
        }

        // Enable auth with the auto-generated token as default admin user
        config.api.auth.mode = Some(detrix_config::AuthMode::Simple);
        config.api.auth.users = vec![detrix_config::StaticUser::new(
            auto_token,
            AUTO_AUTH_DEFAULT_USER_ID.to_string(),
            detrix_config::UserRole::Admin,
        )];
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

    // Allocate HTTP port and immediately bind to hold it.
    //
    // Binding before writing to the PID file eliminates a TOCTOU race that occurs when
    // multiple daemons start in parallel (e.g., during E2E tests): without early binding
    // another process can grab the port between our availability check and the actual bind
    // inside HttpServer::start_with_shutdown, causing "address already in use" failures.
    let (http_port, http_listener) = if http_enabled {
        let preferred = port_registry
            .allocate(ServiceType::Http)
            .context("Failed to allocate HTTP port")?;

        // Actually bind the socket now to hold the port.
        // If the preferred port was taken (TOCTOU), try fallback ports.
        let http_addr_str = format!("{}:{}", config.api.rest.host, preferred);
        let listener = match TcpListener::bind(&http_addr_str).await {
            Ok(l) => l,
            Err(_) if fallback_enabled => {
                // preferred was taken between check and bind; scan for a free port
                let fallback = detrix_config::ports::find_available_port(
                    preferred.saturating_add(1),
                    preferred.saturating_add(100),
                )
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Failed to bind HTTP port {} and no fallback found in range {}–{}",
                        preferred,
                        preferred + 1,
                        preferred + 100
                    )
                })?;
                let fallback_addr = format!("{}:{}", config.api.rest.host, fallback);
                TcpListener::bind(&fallback_addr)
                    .await
                    .with_context(|| format!("Failed to bind fallback HTTP port {}", fallback))?
            }
            Err(e) => {
                return Err(e).context(format!("Failed to bind HTTP port {}", preferred));
            }
        };
        let actual_port = listener.local_addr()?.port();
        if actual_port != config.api.rest.port {
            warn!(
                "⚠️  Port {} is unavailable (in use by another process), using port {} instead. \
                 Set 'api.port_fallback = false' in config to fail instead of auto-selecting a port.",
                config.api.rest.port, actual_port
            );
        }
        (actual_port, Some(listener))
    } else {
        let preferred = port_registry
            .allocate(ServiceType::Http)
            .context("Failed to allocate HTTP port")?;
        (preferred, None)
    };

    // Ensure the registry reflects the actual bound port so the PID file is correct.
    // In the normal case these are the same; in the rare TOCTOU fallback the actual
    // bound port differs from what allocate() returned.
    port_registry.set_actual(ServiceType::Http, http_port);

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

    // Move the PID file guard into a dedicated sentinel task.
    //
    // Problem: in release builds the MIR optimizer may drop `pid_file_guard`
    // early (right after the last *syntactic* reference at set_ports_with_host)
    // because it is not referenced again until the explicit `drop()` at the end
    // of the shutdown sequence. Dropping it early releases the exclusive flock,
    // allowing a second daemon to start while the first is still running.
    //
    // Fix: move the guard into a separate Tokio task that holds it until it
    // receives an explicit "drop now" signal via a oneshot channel. The guard
    // never enters the `serve()` async state machine again, so the optimizer
    // cannot touch it.
    let pid_guard_sentinel_tx = if let Some(guard) = pid_file_guard.take() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let my_pid = std::process::id();
        tokio::spawn(async move {
            let guard = guard; // holds the flock for the daemon's lifetime
            info!("PID {} sentinel task started — holding flock", my_pid);
            let result = rx.await; // blocks until sender signals or is dropped
            info!(
                "PID {} sentinel task received shutdown signal (result={:?}) — releasing flock",
                my_pid, result
            );
            drop(guard); // explicit drop AFTER await - ensures compiler keeps guard in state machine
            info!("PID {} sentinel task — flock released", my_pid);
        });
        Some(tx)
    } else {
        None
    };
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
    let ctx = infra
        .into_app_context(
            &config.api,
            &config.safety,
            &config.storage,
            &config.daemon,
            &config.adapter,
            &config.anchor,
            &config.limits,
            &config.vfs,
            gelf_output.clone(),
            Some(config.agent.clone()),
        )
        .await;
    let app_context = ctx.app_context;
    let storage = ctx.storage;
    let bridge_file_source = ctx.bridge_file_source;

    // Cleanup stale connections based on TTL
    let ttl_days = config.connection_ttl_days;
    let cleanup_result = app_context
        .connection_service
        .cleanup_stale_connections(ttl_days)
        .await;

    match cleanup_result {
        Ok(count) => {
            if count > 0 {
                info!(removed = count, ttl_days, "Cleaned up stale connections");
            }
        }
        Err(e) => {
            warn!(error = ?e, "Failed to cleanup stale connections");
        }
    }

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
                expressions: metric_def.expressions.clone(),
                language: metric_def.language.parse_language().with_context(|| {
                    format!("Invalid language for metric '{}'", metric_def.name)
                })?,
                mode: metric_def.mode.clone(),
                enabled: metric_def.enabled,
                condition: metric_def.condition.clone(),
                safety_level: metric_def.safety_level,
                created_at: Some(chrono::Utc::now().timestamp_micros()),
                user_id: None,
                agent_id: None,
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

    // Apply DETRIX_ADVERTISE_URL env var override (takes precedence over config file)
    if let Ok(env_url) = std::env::var("DETRIX_ADVERTISE_URL") {
        let env_url = env_url.trim().to_string();
        if !env_url.is_empty() {
            config.daemon.advertise_url = Some(env_url);
        }
    }
    if let Some(ref url) = config.daemon.advertise_url {
        info!("📢 Advertise URL: {}", url);
    }

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
        .bridge_file_source(bridge_file_source)
        .advertise_url(config.daemon.advertise_url.clone())
        .build(),
    );

    // Shutdown coordination channel
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Create JWT validator for external auth mode (if configured)
    let jwt_validator = if config.api.auth.mode == Some(detrix_config::AuthMode::External) {
        info!("🔑 Creating JWT validator for external auth mode...");
        match JwksValidator::new(&config.api.auth.jwt) {
            Ok(validator) => {
                // Pre-fetch JWKS keys with retry — transient 502/network errors at container
                // startup (common in Docker Desktop on macOS) must not leave the cache empty.
                let mut preload_ok = false;
                for attempt in 1u32..=5 {
                    match validator.force_refresh().await {
                        Ok(()) => {
                            info!(
                                key_count = validator.cached_key_count(),
                                "JWKS keys pre-fetched"
                            );
                            preload_ok = true;
                            break;
                        }
                        Err(e) if attempt < 5 => {
                            let delay_ms = 200 * (1u64 << (attempt - 1)); // 200, 400, 800, 1600
                            warn!(
                                error = %e,
                                attempt,
                                retry_ms = delay_ms,
                                "JWKS preload failed, retrying"
                            );
                            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        }
                        Err(e) => {
                            warn!(error = %e, "JWKS preload failed after 5 attempts — JWT auth may reject until keys are refreshed");
                        }
                    }
                }
                let _ = preload_ok;
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

    // Snapshot connections from previous sessions BEFORE the HTTP server starts.
    // This is critical: once the HTTP server is up, clients can create new connections.
    // By taking the snapshot here (pre-HTTP), we guarantee that the background restore
    // task only ever touches connections that existed at daemon launch time and never
    // interferes with adapters started by the current session's client requests.
    let startup_connections = app_context
        .connection_service
        .list_connections_for_startup_restore()
        .await;

    // Restore connections from previous sessions in the BACKGROUND.
    // We pass the pre-captured snapshot so the task cannot race with incoming clients.
    // - If debugger is still running → reconnect
    // - If debugger is gone → delete the connection
    {
        let connection_service = Arc::clone(&app_context.connection_service);
        tokio::spawn(async move {
            let (reconnected, deleted) = connection_service
                .restore_connections_on_startup(startup_connections)
                .await;
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

        // Pass the pre-bound listener so axum uses the socket we already hold.
        // This eliminates the TOCTOU window between port allocation and server bind.
        let http_server = match http_listener {
            Some(listener) => http_server.with_listener(listener),
            None => http_server,
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
        let agent_service = api_state
            .context
            .agent_connection_manager
            .as_ref()
            .map(|_| AgentServiceImpl::new(Arc::clone(&api_state)));

        // Create auth interceptor for gRPC (mirrors HTTP auth middleware)
        let grpc_auth_state = match jwt_validator {
            Some(validator) => {
                AuthInterceptorState::with_jwt_validator(config.api.auth.clone(), validator)
            }
            None => AuthInterceptorState::new(config.api.auth.clone()),
        };
        let auth_interceptor = create_auth_interceptor(grpc_auth_state);
        let agent_auth_interceptor = create_agent_auth_interceptor(
            config.agent.agent_token_hashes.clone(),
            config.agent.dev_mode,
        );

        if config.api.auth.is_enabled() {
            info!(mode = ?config.api.auth.mode, "✓ gRPC authentication enabled");
        } else {
            info!("✓ gRPC authentication disabled (all endpoints public)");
        }
        if !config.agent.agent_token_hashes.is_empty() {
            info!(
                count = config.agent.agent_token_hashes.len(),
                "✓ Agent gRPC auth: token hash(es) configured"
            );
        } else if config.agent.dev_mode {
            warn!("Agent gRPC auth DISABLED (dev_mode=true) — not safe for production");
        } else {
            warn!("Agent gRPC endpoint active but no token hashes configured and dev_mode=false — all agent connections will be rejected");
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
            let server = Server::builder()
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
                ));

            let server = if let Some(agent_service) = agent_service {
                info!("Registering AgentService on gRPC server");
                server.add_service(AgentServiceServer::with_interceptor(
                    agent_service,
                    agent_auth_interceptor,
                ))
            } else {
                info!("AgentService disabled on gRPC server (no agent manager)");
                server
            };

            if let Err(e) = server.serve_with_shutdown(grpc_addr, shutdown_signal).await {
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
        use std::os::unix::io::FromRawFd;
        use tokio::io::unix::AsyncFd;
        use tokio::signal::unix::{signal, SignalKind};

        // Wrap the SIGTERM self-pipe (installed at top of run()) for async notification.
        // SAFETY: sigterm_pipe_fd is a valid pipe fd from sigterm_info::install().
        let sigterm_file: std::fs::File = unsafe { FromRawFd::from_raw_fd(sigterm_pipe_fd) };
        let sigterm_async =
            AsyncFd::new(sigterm_file).context("Failed to create async SIGTERM notifier")?;

        // SIGINT still uses tokio's handler (no need for sender PID on Ctrl+C)
        let mut sigint =
            signal(SignalKind::interrupt()).context("Failed to install SIGINT handler")?;

        loop {
            tokio::select! {
                _ = sigterm_async.readable() => {
                    let sender = sigterm_info::sender_pid();
                    let my_pid = std::process::id();
                    info!("Received SIGTERM (my PID: {}, sender PID: {})", my_pid, sender);
                    break;
                }
                _ = sigint.recv() => {
                    info!("Received SIGINT (Ctrl+C)");
                    break;
                }
                result = mcp_shutdown_rx.changed() => {
                    match result {
                        Ok(()) => {
                            if *mcp_shutdown_rx.borrow() {
                                info!("All MCP clients disconnected - auto-shutdown triggered");
                                break;
                            }
                            // Value changed to false or spurious wake — continue waiting
                        }
                        Err(_) => {
                            // Sender dropped — shut down
                            break;
                        }
                    }
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

        loop {
            tokio::select! {
                _ = ctrl_c.recv() => {
                    info!("Received Ctrl+C");
                    break;
                }
                _ = ctrl_break.recv() => {
                    info!("Received Ctrl+Break");
                    break;
                }
                result = mcp_shutdown_rx.changed() => {
                    match result {
                        Ok(()) => {
                            if *mcp_shutdown_rx.borrow() {
                                info!("All MCP clients disconnected - auto-shutdown triggered");
                                break;
                            }
                            // Value changed to false or spurious wake — continue waiting
                        }
                        Err(_) => {
                            // Sender dropped — shut down
                            break;
                        }
                    }
                }
            }
        }
    }

    // Fallback for other non-unix platforms (unlikely but handles edge cases)
    #[cfg(all(not(unix), not(windows)))]
    {
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Received Ctrl+C");
                    break;
                }
                result = mcp_shutdown_rx.changed() => {
                    match result {
                        Ok(()) => {
                            if *mcp_shutdown_rx.borrow() {
                                info!("All MCP clients disconnected - auto-shutdown triggered");
                                break;
                            }
                            // Value changed to false or spurious wake — continue waiting
                        }
                        Err(_) => {
                            // Sender dropped — shut down
                            break;
                        }
                    }
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

    // NOTE: We intentionally do NOT delete the auth-token file on shutdown.
    //
    // The file is always overwritten when a new daemon starts, so there is no
    // stale-token problem. Deleting it causes a worse failure mode: if a
    // concurrent daemon (e.g. an integration test) overwrites the file and then
    // shuts down, the production daemon's clients suddenly get 401 instead of
    // a clean "connection refused". Leaving a stale file on disk is harmless —
    // bridges will get a connection error (daemon is down), not a spurious 401.
    let _ = our_written_token; // suppress unused warning

    // Signal the PID file sentinel task to drop the guard (releases the flock).
    // Dropping the sender causes the sentinel's rx.await to resolve, which drops
    // the PidFile and releases the exclusive flock.
    drop(pid_guard_sentinel_tx);
    if daemon {
        info!("✓ PID file released");
    }

    info!("✓ Detrix server stopped");
    Ok(())
}

/// SIGTERM handler that captures the sender PID via SA_SIGINFO.
///
/// Uses a self-pipe to notify the tokio event loop (instead of tokio's built-in
/// signal handler) so we can inspect `siginfo_t.si_pid` to identify which process
/// sent the signal — critical for debugging cross-test PID interference.
#[cfg(unix)]
mod sigterm_info {
    use std::sync::atomic::{AtomicI32, Ordering};

    /// PID of the process that sent SIGTERM (0 if not yet received).
    static SENDER_PID: AtomicI32 = AtomicI32::new(0);

    /// Write end of the self-pipe for notifying the event loop.
    static PIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

    /// SA_SIGINFO handler: stores sender PID and writes to self-pipe.
    /// Only uses async-signal-safe operations (atomic store + write).
    extern "C" fn handler(
        _sig: nix::libc::c_int,
        info: *mut nix::libc::siginfo_t,
        _ctx: *mut nix::libc::c_void,
    ) {
        // SAFETY: Atomic store and libc::write are async-signal-safe.
        unsafe {
            if !info.is_null() {
                SENDER_PID.store((*info).si_pid(), Ordering::SeqCst);
            }
            let fd = PIPE_WRITE_FD.load(Ordering::SeqCst);
            if fd >= 0 {
                let byte: u8 = 1;
                nix::libc::write(fd, &byte as *const u8 as *const nix::libc::c_void, 1);
            }
        }
    }

    /// Install SIGTERM handler with SA_SIGINFO and return the read-end fd
    /// of a self-pipe that gets a byte when SIGTERM arrives.
    pub fn install() -> anyhow::Result<std::os::unix::io::RawFd> {
        use anyhow::Context;

        let mut fds = [0i32; 2];
        if unsafe { nix::libc::pipe(fds.as_mut_ptr()) } != 0 {
            anyhow::bail!("pipe() failed: {}", std::io::Error::last_os_error());
        }
        unsafe {
            nix::libc::fcntl(fds[0], nix::libc::F_SETFL, nix::libc::O_NONBLOCK);
            nix::libc::fcntl(fds[1], nix::libc::F_SETFL, nix::libc::O_NONBLOCK);
            nix::libc::fcntl(fds[0], nix::libc::F_SETFD, nix::libc::FD_CLOEXEC);
            nix::libc::fcntl(fds[1], nix::libc::F_SETFD, nix::libc::FD_CLOEXEC);
        }

        PIPE_WRITE_FD.store(fds[1], Ordering::SeqCst);

        let action = nix::sys::signal::SigAction::new(
            nix::sys::signal::SigHandler::SigAction(handler),
            nix::sys::signal::SaFlags::SA_SIGINFO | nix::sys::signal::SaFlags::SA_RESTART,
            nix::sys::signal::SigSet::empty(),
        );
        unsafe {
            nix::sys::signal::sigaction(nix::sys::signal::Signal::SIGTERM, &action)
                .context("sigaction(SIGTERM) failed")?;
        }

        Ok(fds[0])
    }

    /// PID of the process that sent SIGTERM (0 if not yet received).
    pub fn sender_pid() -> i32 {
        SENDER_PID.load(Ordering::SeqCst)
    }
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
/// Delegates to `detrix_config::credentials::write_file_securely`.
fn write_token_securely(token_path: &std::path::Path, token: &str) -> std::io::Result<()> {
    detrix_config::credentials::write_file_securely(token_path, token)
}
