//! Expression and metric validation for MetricService

use crate::error::OperationWarning;
use crate::services::file_inspection_types::{
    FileInspectionRequest, FileInspectionResult, SourceLanguageExt,
};
use crate::Result;
use detrix_core::{expression_contains_function_call, Metric, SafetyLevel, SourceLanguage};

use super::MetricService;

impl MetricService {
    /// Validate SafeMode constraints for a metric.
    ///
    /// When a connection is in SafeMode, only logpoints (non-blocking observation)
    /// are allowed. Operations that require actual breakpoints are blocked:
    /// - capture_stack_trace: Requires pausing execution to capture stack
    /// - capture_memory_snapshot: Requires pausing execution to inspect memory
    /// - Go function calls: Delve requires breakpoints for function evaluation
    ///
    /// Language-specific behavior:
    /// - Python: Function calls use logpoints (non-blocking) - allowed in SafeMode
    /// - Go: Function calls require breakpoints via `call` prefix - blocked in SafeMode
    /// - Rust: Function calls use logpoints (non-blocking) - allowed in SafeMode
    ///
    /// Uses in-memory lookup from AdapterLifecycleManager (no database query).
    /// Returns Ok(()) if allowed, SafeModeViolation error if blocked.
    pub(super) fn validate_safe_mode(&self, metric: &Metric) -> Result<()> {
        // Determine if this metric requires breakpoint (blocking operation)
        let needs_introspection = metric.capture_stack_trace || metric.capture_memory_snapshot;
        let is_go_function_call = metric.language == SourceLanguage::Go
            && expression_contains_function_call(&metric.expression);

        let requires_breakpoint = needs_introspection || is_go_function_call;

        // Skip SafeMode check if metric doesn't require breakpoint
        if !requires_breakpoint {
            return Ok(());
        }

        // Check if connection is in SafeMode (in-memory lookup, no DB query)
        match self.adapter_manager.is_safe_mode(&metric.connection_id) {
            Some(true) => {
                // SafeMode enabled - block the operation
                let operation = if is_go_function_call && !needs_introspection {
                    "Go function call (requires breakpoint)".to_string()
                } else {
                    let mut ops = Vec::new();
                    if metric.capture_stack_trace {
                        ops.push("capture_stack_trace");
                    }
                    if metric.capture_memory_snapshot {
                        ops.push("capture_memory_snapshot");
                    }
                    if is_go_function_call {
                        ops.push("Go function call");
                    }
                    ops.join(" and ")
                };

                Err(detrix_core::Error::SafeModeViolation {
                    operation,
                    connection_id: metric.connection_id.0.clone(),
                }
                .into())
            }
            Some(false) => {
                // SafeMode disabled - allow the operation
                Ok(())
            }
            None => {
                // Adapter not registered - require adapter to be running
                // for operations that use breakpoints
                let feature = if is_go_function_call && !needs_introspection {
                    "Go function calls (they require breakpoints)"
                } else {
                    "capture_stack_trace or capture_memory_snapshot"
                };
                Err(crate::Error::ConnectionNotFound(format!(
                    "Connection '{}' not active. Start the connection before adding metrics with {}.",
                    metric.connection_id.0, feature
                )))
            }
        }
    }

    /// Validate metric expression for safety using language-specific validator
    ///
    /// Uses the `ValidatorRegistry` to dispatch to the appropriate language validator.
    /// Returns `Ok(true)` if safe, `Ok(false)` if unsafe with violations in the result.
    pub fn validate_expression(
        &self,
        expression: &str,
        language: &str,
        safety_level: SafetyLevel,
    ) -> Result<crate::safety::ValidationResult> {
        self.validators.validate(language, expression, safety_level)
    }

    /// Check if a function is allowed for a specific language
    pub fn is_function_allowed(&self, language: &str, func_name: &str, level: SafetyLevel) -> bool {
        self.validators
            .is_function_allowed(language, func_name, level)
            .unwrap_or(false)
    }

    /// Get list of supported languages for safety validation
    pub fn supported_languages(&self) -> Vec<&str> {
        self.validators.supported_languages()
    }

    /// Validate a metric before saving
    ///
    /// Returns warnings (like missing safety validator) that should be reported to CLI.
    pub(super) fn validate_metric(&self, metric: &Metric) -> Result<Vec<OperationWarning>> {
        let mut warnings = Vec::new();

        // Validate name
        Metric::validate_name(&metric.name)?;

        // Validate expression is not empty
        if metric.expression.trim().is_empty() {
            return Err(detrix_core::Error::InvalidExpression(
                "Expression cannot be empty".to_string(),
            )
            .into());
        }

        // Validate expression length using config
        Metric::validate_expression_length(
            &metric.expression,
            self.limits_config.max_expression_length,
        )?;

        // Check if language has a safety validator
        let language_str = metric.language.as_str();
        if self.validators.supports_language(language_str) {
            // Use language-specific validator for full AST-based safety check
            let result =
                self.validators
                    .validate(language_str, &metric.expression, metric.safety_level)?;
            if !result.is_safe {
                return Err(detrix_core::Error::SafetyViolation {
                    violations: result.errors,
                }
                .into());
            }
        } else {
            // Language not yet supported for safety validation
            // Return warning to be displayed at CLI layer
            warnings.push(OperationWarning::NoSafetyValidator {
                language: language_str.to_string(),
                metric_name: metric.name.clone(),
            });
        }

        // Validate expression variables are in scope (Python only, for now)
        self.validate_expression_scope(metric)?;

        Ok(warnings)
    }

    /// Validate that expression variables are in scope at the metric location.
    ///
    /// For languages with scope validation support (Python, Go), uses AST analysis
    /// to determine available variables.
    /// For other languages, this check is skipped (validated at runtime by debugger).
    ///
    /// File existence is ALWAYS checked, regardless of expression complexity.
    /// Only scope validation (variable existence check) is skipped for complex expressions.
    fn validate_expression_scope(&self, metric: &Metric) -> Result<()> {
        // Check if language supports scope validation using capabilities
        // Use the metric's language directly (already a SourceLanguage)
        if !metric.language.capabilities().has_scope_validation {
            return Ok(());
        }

        // Determine if expression is complex (skip scope validation but NOT file check)
        let expression = metric.expression.trim();
        let is_complex_expression =
            expression.contains('.') || expression.contains('(') || expression.contains('[');

        let request = FileInspectionRequest {
            file_path: metric.location.file.clone(),
            line: Some(metric.location.line),
            find_variable: None,
        };

        // ALWAYS check file existence - this is a fundamental safety check
        // Inspect file for available variables - fail if inspection fails
        // (don't silently skip validation, that defeats the purpose of safety checks)
        let result = match self.file_inspection.inspect(request) {
            Ok((_, result)) => result,
            Err(e) => {
                // Check if this is a "file not found" error vs analyzer error
                let error_str = e.to_string();
                if error_str.contains("not found")
                    || error_str.contains("No such file")
                    || error_str.contains("does not exist")
                {
                    return Err(detrix_core::Error::InvalidLocation(format!(
                        "File '{}' not found at line {}: {}",
                        metric.location.file, metric.location.line, error_str
                    ))
                    .into());
                }
                // For analyzer errors (e.g., analyzer not installed), log warning and continue
                // but only in development - in production we should fail
                tracing::warn!(
                    "Scope validation unavailable for metric '{}': {} (validation skipped)",
                    metric.name,
                    e
                );
                return Ok(());
            }
        };

        // Skip scope validation for complex expressions (only validate simple variable names)
        // Complex expression like `obj.attr`, `func()`, or `arr[0]` - skip scope validation
        // These will be validated at runtime by the debugger
        if is_complex_expression {
            return Ok(());
        }

        // Check if we got line inspection result with available variables
        if let FileInspectionResult::LineInspection(line_info) = result {
            if !line_info
                .available_variables
                .contains(&expression.to_string())
            {
                return Err(detrix_core::Error::InvalidExpression(format!(
                    "Variable '{}' is not in scope at {}:{}. Available variables: {}",
                    expression,
                    metric.location.file,
                    metric.location.line,
                    if line_info.available_variables.is_empty() {
                        "none".to_string()
                    } else {
                        line_info.available_variables.join(", ")
                    }
                ))
                .into());
            }
        }

        Ok(())
    }
}
