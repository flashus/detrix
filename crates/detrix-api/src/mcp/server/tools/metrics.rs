//! Metric creation tools: observe, add_metric, enable_from_diff
//!
//! These are the core metric creation tools - the "write" side of metrics.

use crate::common::{parse_diff, parse_location_flexible, Confidence};
use crate::grpc::conversions::add_request_to_metric;
use crate::mcp::error::ToMcpResult;
use crate::mcp::helpers;
use crate::mcp::params::{AddMetricParams, EnableFromDiffParams, ObserveParams};
use crate::mcp::proto_adapters::mcp_params_to_add_metric_request;
use crate::state::ApiState;
use detrix_core::{ConnectionId, MetricId};
use rmcp::ErrorData as McpError;
use std::sync::Arc;

use super::common::{build_observe_response, MetricBuilder, ObserveResponseParams};

/// Result of observe operation - contains all data needed for response building
pub struct ObserveResult {
    pub metric_id: MetricId,
    pub metric_name: String,
    pub file: String,
    pub line: u32,
    pub expression: String,
    pub connection_id: String,
    pub line_content: String,
    pub line_source: &'static str,
    pub alternatives: Vec<(u32, String)>,
    pub warnings: Vec<String>,
}

impl ObserveResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let mut message = format!(
            "✓ Observing '{}' at {}:{}\n\
             Metric: {} (ID: {})",
            self.expression, self.file, self.line, self.metric_name, self.metric_id.0
        );

        if !self.line_content.is_empty() {
            message.push_str(&format!("\nLine: {}", self.line_content.trim()));
        }

        if !self.alternatives.is_empty() {
            message.push_str(&format!(
                "\n\n💡 {} alternative location(s) found. Use line= to change.",
                self.alternatives.len()
            ));
        }

        for warning in &self.warnings {
            message.push_str(&format!("\n⚠️ {}", warning));
        }

        message
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        build_observe_response(ObserveResponseParams {
            metric_id: self.metric_id.0,
            metric_name: &self.metric_name,
            file: &self.file,
            line: self.line,
            expression: &self.expression,
            connection_id: &self.connection_id,
            line_content: &self.line_content,
            line_source: self.line_source,
            alternatives: &self.alternatives,
            warnings: &self.warnings,
        })
    }
}

/// Core observe implementation - extracts logic from MCP tool wrapper
///
/// This function handles:
/// 1. Connection resolution (auto-select if only one)
/// 2. Line number determination (auto-find or user-provided)
/// 3. Metric name generation
/// 4. Metric creation via service
///
/// The MCP wrapper handles instrumentation (timing, error tracking).
pub async fn observe_impl(
    state: &Arc<ApiState>,
    params: ObserveParams,
    resolved_connection: Option<ConnectionId>,
) -> Result<ObserveResult, McpError> {
    let ObserveParams {
        file,
        expression,
        line,
        connection_id,
        name,
        find_variable,
        group,
        capture_stack_trace,
        capture_memory_snapshot,
        ttl_seconds,
    } = params;

    // Step 1: Resolve connection (use provided or auto-select)
    let conn_id = match resolved_connection {
        Some(id) => id,
        None => helpers::connection::resolve_connection(state, connection_id).await?,
    };

    // Get connection details for language
    let connection = state
        .context
        .connection_service
        .get_connection(&conn_id)
        .await
        .mcp_context("Failed to get connection")?
        .ok_or_else(|| McpError::internal_error("Connection disappeared after validation", None))?;

    // Step 2: Determine line number
    let (final_line, line_content, alternatives, line_source) = if let Some(l) = line {
        // User provided explicit line
        (l, String::new(), Vec::new(), "user_provided")
    } else {
        // Auto-find best line
        let (l, content, alts) =
            helpers::metrics::find_best_line(&file, &expression, find_variable.as_deref())?;
        (l, content, alts, "auto_found")
    };

    // Step 3: Generate metric name if not provided
    let metric_name = name
        .unwrap_or_else(|| helpers::metrics::generate_metric_name(&expression, &file, final_line));

    // Step 4: Get defaults from config and build metric
    let config = state.config_service.get_config().await;
    let metric = MetricBuilder::new(
        metric_name.clone(),
        conn_id.clone(),
        file.clone(),
        final_line,
        expression.clone(),
        connection.language,
    )
    .with_defaults(&config.defaults)
    .with_group(group)
    .with_stack_trace(capture_stack_trace.unwrap_or(false))
    .with_memory_snapshot(capture_memory_snapshot.unwrap_or(false))
    .with_ttl(ttl_seconds.map(|t| t as i64))
    .build();

    let outcome = state
        .context
        .metric_service
        .add_metric(metric, false) // Don't replace by default
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to add metric: {}", e), None))?;

    let warnings: Vec<String> = outcome.warnings.iter().map(|w| w.to_string()).collect();

    Ok(ObserveResult {
        metric_id: outcome.value,
        metric_name,
        file,
        line: final_line,
        expression,
        connection_id: conn_id.to_string(),
        line_content,
        line_source,
        alternatives,
        warnings,
    })
}

// ============================================================================
// add_metric Implementation
// ============================================================================

/// Result of add_metric operation
pub struct AddMetricResult {
    pub metric_id: MetricId,
    pub name: String,
    pub file: String,
    pub line: u32,
    pub expression: String,
    pub connection_id: String,
    pub warnings: Vec<String>,
}

impl AddMetricResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let mut message = format!(
            "Metric '{}' added at {}:{}\nExpression: {}",
            self.name, self.file, self.line, self.expression
        );

        for warning in &self.warnings {
            message.push_str(&format!("\n⚠️ {}", warning));
        }

        message
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "success": true,
            "metric_id": self.metric_id.0,
            "name": self.name,
            "parsed": {
                "file": self.file,
                "line": self.line,
                "expression": self.expression,
                "connection_id": self.connection_id,
            },
            "location": format!("@{}#{}", self.file, self.line),
            "warnings": self.warnings,
        })
    }
}

/// Core add_metric implementation - extracts logic from MCP tool wrapper
pub async fn add_metric_impl(
    state: &Arc<ApiState>,
    params: AddMetricParams,
) -> Result<AddMetricResult, McpError> {
    // Keep copies for response
    let metric_name = params.name.clone();
    let metric_expression = params.expression.clone();
    let connection_id_str = params.connection_id.clone();
    let replace_flag = params.replace.unwrap_or(false);

    // Parse location with flexible format
    let loc = parse_location_flexible(&params.location, params.line)?;
    let parsed_file = loc.file.clone();
    let parsed_line = loc.line;

    // Validate connection_id is provided
    if connection_id_str.is_empty() {
        return Err(McpError::invalid_params(
            "connection_id is required. Get it from create_connection response or list_connections."
                .to_string(),
            None,
        ));
    }

    let connection_id = ConnectionId::new(&connection_id_str);

    // Verify connection exists and is connected
    let connection = state
        .context
        .connection_service
        .get_connection(&connection_id)
        .await
        .mcp_context("Failed to get connection")?
        .ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "Connection '{}' not found. Use list_connections to see available connections.",
                    connection_id_str
                ),
                None,
            )
        })?;

    // Check connection has running adapter
    if !state
        .context
        .adapter_lifecycle_manager
        .has_adapter(&connection_id)
        .await
    {
        return Err(McpError::invalid_params(
            format!(
                "Connection '{}' is not connected (status: {:?}). Reconnect or use a different connection.",
                connection_id_str, connection.status
            ),
            None,
        ));
    }

    // Get default safety level from config
    let config = state.config_service.get_config().await;
    let safety_level = config.safety.default_safety_level().as_str();

    // Convert MCP params → Proto → Core Metric
    let mut proto_request = mcp_params_to_add_metric_request(
        &params,
        parsed_file.clone(),
        parsed_line,
        connection_id_str.clone(),
        safety_level,
    );
    proto_request.language = Some(connection.language.to_string());

    let metric = add_request_to_metric(&proto_request)
        .map_err(|e| McpError::invalid_params(e.to_string(), None))?;

    // Check workflow
    state.context.mcp_usage.check_workflow_for_add_metric();

    // Add the metric
    let outcome = state
        .context
        .metric_service
        .add_metric(metric, replace_flag)
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to add metric: {}", e), None))?;

    let warnings: Vec<String> = outcome.warnings.iter().map(|w| w.to_string()).collect();

    Ok(AddMetricResult {
        metric_id: outcome.value,
        name: metric_name,
        file: parsed_file,
        line: parsed_line,
        expression: metric_expression,
        connection_id: connection_id_str,
        warnings,
    })
}

// ============================================================================
// enable_from_diff Implementation
// ============================================================================

/// Result of enable_from_diff operation
pub struct EnableFromDiffResult {
    pub added_metrics: Vec<AddedMetric>,
    pub failed_metrics: Vec<FailedMetric>,
    pub suggestions: Vec<Suggestion>,
    pub connection_id: String,
}

/// A metric that was successfully added
pub struct AddedMetric {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub id: u64,
}

/// A metric that failed to be added
pub struct FailedMetric {
    pub file: String,
    pub line: u32,
    pub statement: String,
    pub error: String,
}

/// A suggestion for manual review
pub struct Suggestion {
    pub file: String,
    pub line: u32,
    pub statement: String,
    pub expression: String,
}

impl EnableFromDiffResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let mut message = String::new();

        if !self.added_metrics.is_empty() {
            if self.suggestions.is_empty() && self.failed_metrics.is_empty() {
                message.push_str(&format!(
                    "✓ Added {} metric(s) from diff:\n",
                    self.added_metrics.len()
                ));
            } else {
                message.push_str(&format!(
                    "Partial success - added {} metric(s):\n",
                    self.added_metrics.len()
                ));
            }

            for m in &self.added_metrics {
                message.push_str(&format!(
                    "  - {} @{}#{} (ID: {})\n",
                    m.name, m.file, m.line, m.id
                ));
            }
        }

        if !self.failed_metrics.is_empty() {
            message.push_str(&format!(
                "\n⚠️ Failed to add {} metric(s):\n",
                self.failed_metrics.len()
            ));
            for f in &self.failed_metrics {
                message.push_str(&format!("  - {}:{}: {}\n", f.file, f.line, f.error));
            }
        }

        if !self.suggestions.is_empty() {
            message.push_str(&format!(
                "\n💡 {} expression(s) need manual review (use 'observe' tool):\n",
                self.suggestions.len()
            ));
            for s in &self.suggestions {
                message.push_str(&format!("  - {}:{}: {}\n", s.file, s.line, s.statement));
                if s.expression != "???" {
                    message.push_str(&format!(
                        "    → observe(file=\"{}\", line={}, expression=\"{}\")\n",
                        s.file, s.line, s.expression
                    ));
                } else {
                    message.push_str(&format!(
                        "    → observe(file=\"{}\", line={}, expression=\"<extract manually>\")\n",
                        s.file, s.line
                    ));
                }
            }
        }

        message
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "success": !self.added_metrics.is_empty(),
            "added": self.added_metrics.iter().map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "file": m.file,
                    "line": m.line,
                    "id": m.id,
                })
            }).collect::<Vec<_>>(),
            "failed": self.failed_metrics.iter().map(|f| {
                serde_json::json!({
                    "file": f.file,
                    "line": f.line,
                    "statement": f.statement,
                    "error": f.error,
                })
            }).collect::<Vec<_>>(),
            "needs_review": self.suggestions.iter().map(|s| {
                serde_json::json!({
                    "file": s.file,
                    "line": s.line,
                    "statement": s.statement,
                    "suggested_expression": s.expression,
                })
            }).collect::<Vec<_>>(),
            "connection_id": self.connection_id,
        })
    }
}

/// Represents "no debug statements found" - special case
pub struct NoDebugStatementsFound;

/// Core enable_from_diff implementation
pub async fn enable_from_diff_impl(
    state: &Arc<ApiState>,
    params: EnableFromDiffParams,
) -> Result<Result<EnableFromDiffResult, NoDebugStatementsFound>, McpError> {
    let EnableFromDiffParams {
        diff,
        connection_id,
        group,
        ttl_seconds,
    } = params;

    // Validate diff is not empty
    if diff.trim().is_empty() {
        return Err(McpError::invalid_params("No diff content provided", None));
    }

    // Parse the diff
    let parse_result = parse_diff(&diff);

    // Check if any debug statements were found
    if !parse_result.has_debug_statements() {
        return Ok(Err(NoDebugStatementsFound));
    }

    // Resolve connection (auto-select if only one)
    let conn_id = helpers::connection::resolve_connection(state, connection_id).await?;

    // Get connection details for language
    let connection = state
        .context
        .connection_service
        .get_connection(&conn_id)
        .await
        .mcp_context("Failed to get connection")?
        .ok_or_else(|| McpError::internal_error("Connection disappeared after validation", None))?;

    // Track results
    let mut added_metrics: Vec<AddedMetric> = Vec::new();
    let mut failed_metrics: Vec<FailedMetric> = Vec::new();

    // Process high/medium confidence expressions (auto-add)
    for parsed in &parse_result.parsed {
        if parsed.confidence == Confidence::Low {
            continue;
        }

        let metric_name =
            helpers::metrics::generate_metric_name(&parsed.expression, &parsed.file, parsed.line);

        let metric = MetricBuilder::new(
            metric_name.clone(),
            conn_id.clone(),
            parsed.file.clone(),
            parsed.line,
            parsed.expression.clone(),
            connection.language,
        )
        .with_group(group.clone())
        .with_ttl(ttl_seconds.map(|t| t as i64))
        .build();

        match state
            .context
            .metric_service
            .add_metric(metric, true) // Replace existing if any
            .await
        {
            Ok(outcome) => {
                added_metrics.push(AddedMetric {
                    name: metric_name,
                    file: parsed.file.clone(),
                    line: parsed.line,
                    id: outcome.value.0,
                });
            }
            Err(e) => {
                failed_metrics.push(FailedMetric {
                    file: parsed.file.clone(),
                    line: parsed.line,
                    statement: parsed.original_statement.clone(),
                    error: e.to_string(),
                });
            }
        }
    }

    // Collect low confidence and unparseable for suggestions
    let mut suggestions: Vec<Suggestion> = Vec::new();

    for parsed in &parse_result.parsed {
        if parsed.confidence == Confidence::Low {
            suggestions.push(Suggestion {
                file: parsed.file.clone(),
                line: parsed.line,
                statement: parsed.original_statement.clone(),
                expression: parsed.expression.clone(),
            });
        }
    }

    for unparseable in &parse_result.unparseable {
        suggestions.push(Suggestion {
            file: unparseable.file.clone(),
            line: unparseable.line,
            statement: unparseable.original_statement.clone(),
            expression: "???".to_string(),
        });
    }

    Ok(Ok(EnableFromDiffResult {
        added_metrics,
        failed_metrics,
        suggestions,
        connection_id: conn_id.to_string(),
    }))
}
