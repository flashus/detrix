//! Python client tester implementation
//!
//! Spawns and controls a Python test application for conformance testing.

use super::{ClientStatus, ClientTester, HealthResponse, SleepResponse, WakeResponse};
use crate::e2e::executor::{register_e2e_process, unregister_e2e_process, wait_for_port};
use async_trait::async_trait;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Python client tester
///
/// Spawns a Python test application that initializes the Detrix client
/// and runs the control plane for testing.
pub struct PythonClientTester {
    /// The spawned Python process
    process: Option<Child>,
    /// Control plane port
    control_port: u16,
    /// Configured daemon URL
    daemon_url: String,
}

impl PythonClientTester {
    /// Spawn a new Python client for conformance testing
    ///
    /// # Arguments
    /// * `test_app_path` - Path to the Python conformance test app
    /// * `daemon_url` - URL of the Detrix daemon
    /// * `control_port` - Port for control plane (0 = auto-assign)
    ///
    /// # Returns
    /// A new PythonClientTester instance
    pub async fn spawn(
        test_app_path: &Path,
        daemon_url: &str,
        control_port: u16,
    ) -> Result<Self, String> {
        // Find an available port if not specified
        let actual_port = if control_port == 0 {
            crate::e2e::executor::get_http_port()
        } else {
            control_port
        };

        // Spawn the Python conformance app
        let log_file = format!("/tmp/python_client_e2e_{}.log", actual_port);
        let log =
            std::fs::File::create(&log_file).map_err(|e| format!("Failed to create log: {}", e))?;
        let log_stderr = log
            .try_clone()
            .map_err(|e| format!("Failed to clone log: {}", e))?;

        let mut cmd = Command::new("python");
        cmd.args([
            test_app_path.to_str().ok_or("Invalid test app path")?,
            daemon_url,
            &actual_port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_stderr));

        // Create new process group on Unix
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let process = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn Python client: {}", e))?;

        let pid = process.id();
        register_e2e_process("python_client", pid);

        // Wait for control plane to be ready
        if !wait_for_port(actual_port, 10).await {
            return Err(format!(
                "Python client control plane not responding on port {} after spawn. Check log: {}",
                actual_port, log_file
            ));
        }

        Ok(Self {
            process: Some(process),
            control_port: actual_port,
            daemon_url: daemon_url.to_string(),
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

    /// Print Python client logs (for debugging)
    pub fn print_logs(&self, last_n_lines: usize) {
        let log_path = format!("/tmp/python_client_e2e_{}.log", self.control_port);
        println!("\n=== PYTHON CLIENT LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read python client log)");
        }
        println!("==========================================\n");
    }
}

#[async_trait]
impl ClientTester for PythonClientTester {
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
        // The conformance app should handle SIGTERM gracefully
        // We'll kill the process directly since there's no shutdown endpoint
        Ok(())
    }

    fn control_port(&self) -> u16 {
        self.control_port
    }

    fn daemon_url(&self) -> &str {
        &self.daemon_url
    }
}

impl Drop for PythonClientTester {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let pid = process.id();

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
            unregister_e2e_process("python_client", pid);
        }
    }
}
