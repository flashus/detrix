//! Unified client process tester
//!
//! Spawns and controls client test applications for testing.
//! Works with any language client that has a test fixture with optional Detrix client.

use super::config::ClientTestConfig;
use super::{ClientStatus, ClientTester, HealthResponse, SleepResponse, WakeResponse};
use crate::e2e::executor::{
    get_http_port, get_workspace_root, register_e2e_process, unregister_e2e_process,
};
use async_trait::async_trait;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Unified client process tester
///
/// Spawns a client test application based on configuration and provides
/// the `ClientTester` interface for testing.
pub struct ClientProcessTester {
    /// Configuration for this client
    config: ClientTestConfig,
    /// The spawned process
    process: Option<Child>,
    /// Control plane port
    control_port: u16,
    /// Configured daemon URL
    daemon_url: String,
    /// Path to log file
    log_path: String,
}

impl ClientProcessTester {
    /// Spawn a new client for testing
    ///
    /// # Arguments
    /// * `config` - Client configuration (language, fixture path, etc.)
    /// * `daemon_url` - URL of the Detrix daemon
    /// * `control_port` - Port for control plane (0 = auto-assign)
    ///
    /// # Returns
    /// A new ClientProcessTester instance
    pub async fn spawn(
        config: ClientTestConfig,
        daemon_url: &str,
        control_port: u16,
    ) -> Result<Self, String> {
        let workspace_root = get_workspace_root();
        let fixture_full_path = workspace_root.join(&config.fixture_path);

        if !fixture_full_path.exists() {
            return Err(format!(
                "Test fixture not found at: {}",
                fixture_full_path.display()
            ));
        }

        // Find an available port if not specified
        let actual_port = if control_port == 0 {
            get_http_port()
        } else {
            control_port
        };

        // Build spawn command and environment
        let (cmd, args) = config.build_spawn_args(&fixture_full_path);
        let env_vars = config.build_env_vars(daemon_url, actual_port);

        // Get working directory
        let working_dir = workspace_root.join(&config.working_dir);
        if !working_dir.exists() {
            return Err(format!(
                "Working directory not found at: {}",
                working_dir.display()
            ));
        }

        // Create log file
        let log_path = format!(
            "/tmp/{}_client_e2e_{}.log",
            config.language.display_name().to_lowercase(),
            actual_port
        );
        let log =
            std::fs::File::create(&log_path).map_err(|e| format!("Failed to create log: {}", e))?;
        let log_stderr = log
            .try_clone()
            .map_err(|e| format!("Failed to clone log: {}", e))?;

        // Spawn the process
        let mut command = Command::new(&cmd);
        command
            .args(&args)
            .current_dir(&working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped()) // Pipe stdout to read control plane URL
            .stderr(Stdio::from(log_stderr));

        // Set environment variables for Detrix client
        for (key, value) in &env_vars {
            command.env(key, value);
        }
        // Ensure unbuffered output
        command.env("PYTHONUNBUFFERED", "1");

        // Create new process group on Unix
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut process = command.spawn().map_err(|e| {
            format!(
                "Failed to spawn {} client: {}",
                config.language.display_name(),
                e
            )
        })?;

        let pid = process.id();
        let process_name = format!("{}_client", config.language.display_name().to_lowercase());
        register_e2e_process(&process_name, pid);

        // Wait for control plane URL in output
        let mut actual_control_port = actual_port;
        let stdout = process.stdout.take();
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            let start = std::time::Instant::now();
            let timeout = Duration::from_secs(15);

            // Also write to log file
            let mut log_file = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .ok();

            for line in reader.lines() {
                if start.elapsed() > timeout {
                    return Err(format!(
                        "{} client did not output control plane URL within timeout. Check log: {}",
                        config.language.display_name(),
                        log_path
                    ));
                }

                let line = line.map_err(|e| format!("Failed to read stdout: {}", e))?;

                // Log the line
                if let Some(ref mut f) = log_file {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", line);
                }

                // Look for control plane URL
                if let Some(port_str) = line
                    .strip_prefix("Control plane: http://127.0.0.1:")
                    .or_else(|| line.strip_prefix("Control plane: http://localhost:"))
                {
                    if let Ok(port) = port_str.parse::<u16>() {
                        actual_control_port = port;
                        break;
                    }
                }

                // Check if process exited
                if let Ok(Some(status)) = process.try_wait() {
                    return Err(format!(
                        "{} client exited with {} before outputting control plane URL. Check log: {}",
                        config.language.display_name(),
                        status,
                        log_path
                    ));
                }
            }
        }

        Ok(Self {
            config,
            process: Some(process),
            control_port: actual_control_port,
            daemon_url: daemon_url.to_string(),
            log_path,
        })
    }

    /// Make HTTP request to control plane
    async fn http_get(&self, path: &str) -> Result<String, String> {
        let url = format!("http://127.0.0.1:{}{}", self.control_port, path);
        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        response
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))
    }

    /// Make HTTP POST request to control plane
    async fn http_post(&self, path: &str, body: Option<&str>) -> Result<String, String> {
        let url = format!("http://127.0.0.1:{}{}", self.control_port, path);
        let client = reqwest::Client::new();
        let mut request = client.post(&url).timeout(Duration::from_secs(10));

        if let Some(b) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(b.to_string());
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("HTTP error {}: {}", status, body));
        }

        response
            .text()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))
    }

    /// Print client logs (for debugging)
    pub fn print_logs(&self, last_n_lines: usize) {
        println!(
            "\n=== {} CLIENT LOG (last {} lines) ===",
            self.config.language.display_name().to_uppercase(),
            last_n_lines
        );
        if let Ok(content) = std::fs::read_to_string(&self.log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read client log)");
        }
        println!("==========================================\n");
    }

    /// Get the client configuration
    pub fn config(&self) -> &ClientTestConfig {
        &self.config
    }

    /// Get the log file path
    pub fn log_path(&self) -> &str {
        &self.log_path
    }
}

#[async_trait]
impl ClientTester for ClientProcessTester {
    async fn status(&self) -> Result<ClientStatus, String> {
        let body = self.http_get("/detrix/status").await?;
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse status: {}", e))
    }

    async fn wake(&self, daemon_url: Option<&str>) -> Result<WakeResponse, String> {
        let body = if let Some(url) = daemon_url {
            let req_body = serde_json::json!({ "daemon_url": url }).to_string();
            self.http_post("/detrix/wake", Some(&req_body)).await?
        } else {
            self.http_post("/detrix/wake", None).await?
        };

        serde_json::from_str(&body).map_err(|e| format!("Failed to parse wake response: {}", e))
    }

    async fn sleep(&self) -> Result<SleepResponse, String> {
        let body = self.http_post("/detrix/sleep", None).await?;
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse sleep response: {}", e))
    }

    async fn health(&self) -> Result<HealthResponse, String> {
        let body = self.http_get("/detrix/health").await?;
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse health: {}", e))
    }

    async fn shutdown(&self) -> Result<(), String> {
        // The test app should handle SIGTERM gracefully
        // Process cleanup happens in Drop
        Ok(())
    }

    fn control_port(&self) -> u16 {
        self.control_port
    }

    fn daemon_url(&self) -> &str {
        &self.daemon_url
    }
}

impl Drop for ClientProcessTester {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let pid = process.id();
            let process_name = format!(
                "{}_client",
                self.config.language.display_name().to_lowercase()
            );

            // Try graceful shutdown first
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
                std::thread::sleep(Duration::from_millis(100));
            }

            // Force kill if still running
            let _ = process.kill();
            let _ = process.wait();
            unregister_e2e_process(&process_name, pid);
        }
    }
}
