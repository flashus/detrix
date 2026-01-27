//! Test Reporter - Verbose logging with step-by-step progress
//!
//! Provides shell-script-like output for E2E tests, showing all operations,
//! responses, and progress in a human-readable format.
//!
//! # Output Buffering
//!
//! To prevent interleaved output when tests run in parallel, all output is
//! buffered by default and printed atomically when the test completes (in `print_footer`).
//! Set `DETRIX_E2E_STREAM=1` env var to print output immediately (useful for debugging).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Test step status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Running,
    Success,
    Failed,
    Skipped,
}

/// A single test step
#[derive(Debug, Clone)]
pub struct TestStep {
    pub number: usize,
    pub name: String,
    pub description: String,
    pub status: StepStatus,
    pub duration_ms: Option<u64>,
    pub details: Option<String>,
}

impl TestStep {
    pub fn new(number: usize, name: &str, description: &str) -> Self {
        Self {
            number,
            name: name.to_string(),
            description: description.to_string(),
            status: StepStatus::Running,
            duration_ms: None,
            details: None,
        }
    }
}

/// Test reporter for verbose output
pub struct TestReporter {
    verbose: AtomicBool,
    /// If true, output immediately; if false, buffer until print_footer
    streaming: bool,
    step_counter: AtomicUsize,
    test_name: String,
    api_type: String,
    start_time: Instant,
    // Note: Using std::sync::Mutex is acceptable here because:
    // 1. Lock duration is very short (no await while holding)
    // 2. This is test-only code
    // 3. The struct is not used in async contexts that would block the executor
    steps: std::sync::Mutex<Vec<TestStep>>,
    /// Buffered output lines (used when streaming=false)
    output_buffer: std::sync::Mutex<Vec<String>>,
}

impl TestReporter {
    pub fn new(test_name: &str, api_type: &str) -> Arc<Self> {
        // Check env var for streaming mode (useful for debugging)
        let streaming = std::env::var(detrix_config::constants::ENV_DETRIX_E2E_STREAM)
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        Arc::new(Self {
            verbose: AtomicBool::new(true),
            streaming,
            step_counter: AtomicUsize::new(0),
            test_name: test_name.to_string(),
            api_type: api_type.to_string(),
            start_time: Instant::now(),
            steps: std::sync::Mutex::new(Vec::new()),
            output_buffer: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Output a line (either immediately or buffered)
    fn output(&self, line: String) {
        if self.streaming {
            println!("{}", line);
        } else {
            self.output_buffer.lock().unwrap().push(line);
        }
    }

    /// Flush all buffered output (prints atomically) and clear the buffer
    fn flush_buffer(&self) {
        if !self.streaming {
            let mut buffer = self.output_buffer.lock().unwrap();
            if !buffer.is_empty() {
                // Print all lines atomically (single print block)
                let output = buffer.join("\n");
                println!("{}", output);
                // Clear buffer to prevent double-printing in Drop
                buffer.clear();
            }
        }
    }

    /// Set verbose mode (default: true)
    pub fn set_verbose(&self, verbose: bool) {
        self.verbose.store(verbose, Ordering::SeqCst);
    }

    /// Check if verbose mode is enabled
    pub fn is_verbose(&self) -> bool {
        self.verbose.load(Ordering::SeqCst)
    }

    /// Print test header
    pub fn print_header(&self) {
        if !self.is_verbose() {
            return;
        }
        self.output(String::new());
        self.output("==========================================================".to_string());
        self.output(format!("  {} ({})", self.test_name, self.api_type));
        self.output("==========================================================".to_string());
        self.output(String::new());
    }

    /// Print test footer with summary
    pub fn print_footer(&self, success: bool) {
        if !self.is_verbose() {
            return;
        }

        let steps = self.steps.lock().unwrap();
        let passed = steps
            .iter()
            .filter(|s| s.status == StepStatus::Success)
            .count();
        let failed = steps
            .iter()
            .filter(|s| s.status == StepStatus::Failed)
            .count();
        let skipped = steps
            .iter()
            .filter(|s| s.status == StepStatus::Skipped)
            .count();
        let total_ms = self.start_time.elapsed().as_millis();
        drop(steps); // Release lock before output

        self.output(String::new());
        self.output("==========================================================".to_string());
        if success {
            self.output("  TEST PASSED".to_string());
        } else {
            self.output("  TEST FAILED".to_string());
        }
        self.output("----------------------------------------------------------".to_string());
        self.output(format!(
            "  Steps: {} passed, {} failed, {} skipped",
            passed, failed, skipped
        ));
        self.output(format!("  Duration: {}ms", total_ms));
        self.output("==========================================================".to_string());
        self.output(String::new());

        // Flush buffered output atomically
        self.flush_buffer();
    }

    /// Start a new step (returns step number)
    pub fn step_start(&self, name: &str, description: &str) -> usize {
        let step_num = self.step_counter.fetch_add(1, Ordering::SeqCst) + 1;

        let step = TestStep::new(step_num, name, description);
        self.steps.lock().unwrap().push(step);

        if self.is_verbose() {
            self.output(String::new());
            self.output(format!("[STEP {}] {}", step_num, name));
            self.output(format!("   {}", description));
        }

        step_num
    }

    /// Log progress within a step
    pub fn step_progress(&self, message: &str) {
        if self.is_verbose() {
            self.output(format!("   {}", message));
        }
    }

    /// Log a request being sent
    pub fn step_request(&self, method: &str, details: Option<&str>) {
        if self.is_verbose() {
            if let Some(d) = details {
                self.output(format!("   -> {} {}", method, d));
            } else {
                self.output(format!("   -> {}", method));
            }
        }
    }

    /// Log a response received
    pub fn step_response(&self, status: &str, body: Option<&str>) {
        if self.is_verbose() {
            self.output(format!("   <- {}", status));
            if let Some(b) = body {
                // Pretty print JSON or limit length
                let truncated = if b.len() > 500 {
                    format!("{}...(truncated)", &b[..500])
                } else {
                    b.to_string()
                };
                // Indent response body
                for line in truncated.lines() {
                    self.output(format!("      {}", line));
                }
            }
        }
    }

    /// Mark step as successful
    pub fn step_success(&self, step_num: usize, details: Option<&str>) {
        let mut steps = self.steps.lock().unwrap();
        if let Some(step) = steps.iter_mut().find(|s| s.number == step_num) {
            step.status = StepStatus::Success;
            step.details = details.map(String::from);
        }
        drop(steps); // Release lock before output

        if self.is_verbose() {
            if let Some(d) = details {
                self.output(format!("   OK: {}", d));
            } else {
                self.output("   OK".to_string());
            }
        }
    }

    /// Mark step as failed
    pub fn step_failed(&self, step_num: usize, error: &str) {
        let mut steps = self.steps.lock().unwrap();
        if let Some(step) = steps.iter_mut().find(|s| s.number == step_num) {
            step.status = StepStatus::Failed;
            step.details = Some(error.to_string());
        }
        drop(steps); // Release lock before output

        if self.is_verbose() {
            self.output(format!("   FAILED: {}", error));
        }
    }

    /// Mark step as skipped
    pub fn step_skipped(&self, step_num: usize, reason: &str) {
        let mut steps = self.steps.lock().unwrap();
        if let Some(step) = steps.iter_mut().find(|s| s.number == step_num) {
            step.status = StepStatus::Skipped;
            step.details = Some(reason.to_string());
        }
        drop(steps); // Release lock before output

        if self.is_verbose() {
            self.output(format!("   SKIPPED: {}", reason));
        }
    }

    /// Log a section header within a step
    pub fn section(&self, title: &str) {
        if self.is_verbose() {
            self.output(String::new());
            self.output(format!("   --- {} ---", title));
        }
    }

    /// Log a key-value pair
    pub fn log_kv(&self, key: &str, value: &str) {
        if self.is_verbose() {
            self.output(format!("   {}: {}", key, value));
        }
    }

    /// Log an info message
    pub fn info(&self, message: &str) {
        if self.is_verbose() {
            self.output(format!("   {}", message));
        }
    }

    /// Log a warning
    pub fn warn(&self, message: &str) {
        if self.is_verbose() {
            self.output(format!("   WARNING: {}", message));
        }
    }

    /// Log an error
    pub fn error(&self, message: &str) {
        if self.is_verbose() {
            self.output(format!("   ERROR: {}", message));
        }
    }

    /// Log raw data (e.g., for debugging)
    pub fn raw(&self, label: &str, data: &str) {
        if self.is_verbose() {
            self.output(String::new());
            self.output(format!("   === {} ===", label));
            for line in data.lines() {
                self.output(format!("   {}", line));
            }
            self.output("   ============".to_string());
        }
    }

    /// Get all steps
    pub fn get_steps(&self) -> Vec<TestStep> {
        self.steps.lock().unwrap().clone()
    }

    /// Check if any step failed
    pub fn has_failures(&self) -> bool {
        self.steps
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.status == StepStatus::Failed)
    }
}

impl Drop for TestReporter {
    /// Flush any remaining buffered output on drop (e.g., during panic)
    fn drop(&mut self) {
        // If there's unflushed output (buffer not empty), flush it
        // This ensures logs are visible even when tests panic
        if let Ok(buffer) = self.output_buffer.lock() {
            if !buffer.is_empty() {
                let output = buffer.join("\n");
                eprintln!("{}", output);
            }
        }
    }
}
