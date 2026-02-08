//! MCP Client Implementation
//!
//! Implements the ApiClient trait for the MCP (Model Context Protocol) API.

use super::client::{
    AddMetricRequest, ApiClient, ApiError, ApiResponse, ApiResult, ConnectionInfo, DiffMetricAdded,
    DiffMetricFailed, DiffMetricNeedsReview, EnableFromDiffRequest, EnableFromDiffResponse,
    EventInfo, GroupInfo, McpUsageInfo, MetricInfo, ObserveRequest, ObserveResponse, StatusInfo,
    SystemEventInfo, ValidationResult,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

/// MCP API client (JSON-RPC over HTTP)
pub struct McpClient {
    base_url: String,
    client: reqwest::Client,
}

impl McpClient {
    /// Create a new MCP client
    pub fn new(http_port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", http_port),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Call an MCP tool (JSON-RPC over HTTP)
    async fn call(&self, tool_name: &str, arguments: Value) -> Result<Value, ApiError> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            },
            "id": 1
        });

        let response = self
            .client
            .post(format!("{}/mcp", self.base_url))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("HTTP error: {}", e)))?;

        let raw_text = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Read error: {}", e)))?;

        let json: Value = serde_json::from_str(&raw_text).map_err(|e| {
            ApiError::new(format!("JSON parse error: {}", e)).with_raw(raw_text.clone())
        })?;

        // Check for JSON-RPC error
        if let Some(error) = json.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(ApiError::new(message).with_raw(raw_text));
        }

        // Check for isError in result
        if let Some(result) = json.get("result") {
            if result.get("isError") == Some(&Value::Bool(true)) {
                let content = result
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("MCP error");
                return Err(ApiError::new(content).with_raw(raw_text));
            }
        }

        Ok(json)
    }

    /// Extract text content from MCP response
    fn extract_text(&self, json: &Value) -> Option<String> {
        json.get("result")?
            .get("content")?
            .as_array()?
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
            .into()
    }

    /// Parse connections from MCP response (handles TOON format)
    fn parse_connections(&self, json: &Value) -> Vec<ConnectionInfo> {
        // Try to parse from content
        let text = match self.extract_text(json) {
            Some(t) => t,
            None => return vec![],
        };

        // For now, just create basic connection info from text
        // The actual MCP response uses TOON format which we'd need to parse properly
        // This is a simplified implementation
        let mut connections = vec![];

        // Parse TOON format: [N]{fields}: line1 line2 ...
        for line in text.lines() {
            if line.contains(',') && !line.starts_with('[') && !line.starts_with("Found") {
                // Simple CSV-like parsing for TOON data lines
                let parts: Vec<&str> = line.trim().split(',').collect();
                if parts.len() >= 5 {
                    let port_str = parts[2].trim();
                    let port = port_str.parse().unwrap_or_else(|_| {
                        eprintln!(
                            "[WARN] Failed to parse port '{}' in connection response, defaulting to 0",
                            port_str
                        );
                        0
                    });
                    connections.push(ConnectionInfo {
                        connection_id: parts[0].trim().to_string(),
                        host: parts[1].trim().trim_matches('"').to_string(),
                        port,
                        language: parts[3].trim().to_string(),
                        status: parts[4].trim().to_string(),
                        safe_mode: false, // MCP text output doesn't include safe_mode
                    });
                }
            }
        }

        connections
    }

    /// Parse metrics from MCP response
    ///
    /// The MCP server returns metrics in TOON format by default (for LLM efficiency).
    /// This function tries multiple parsing strategies:
    /// 1. TOON format (default from server)
    /// 2. JSON format (fallback)
    fn parse_metrics_response(&self, json: &Value) -> Vec<MetricInfo> {
        let text = match self.extract_text(json) {
            Some(t) => t,
            None => return vec![],
        };

        // Skip the summary line ("Found X metrics...") to get the data content
        let content = text
            .lines()
            .skip_while(|l| l.starts_with("Found"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();

        if content.is_empty() {
            return vec![];
        }

        // Handle empty arrays (both TOON "[0]:" and JSON "[]" formats)
        if content == "[]" || content == "[0]:" {
            return vec![];
        }

        // Try parsing as TOON format first (default from server)
        let toon_options = toon_format::DecodeOptions::new().with_strict(false);

        // First try direct TOON decode to MetricInfo vec
        if let Ok(parsed_metrics) = toon_format::decode::<Vec<MetricInfo>>(&content, &toon_options)
        {
            return parsed_metrics;
        }

        // Try TOON decode to serde_json::Value first, then convert
        if let Ok(json_value) = toon_format::decode::<serde_json::Value>(&content, &toon_options) {
            if let Ok(parsed_metrics) = serde_json::from_value::<Vec<MetricInfo>>(json_value) {
                return parsed_metrics;
            }
        }

        // Look for JSON array in the response text
        // The format is: "Found N metrics | connection status\n[{...}, ...]"
        if let Some(json_start) = text.find('[') {
            let json_text = &text[json_start..];
            if let Ok(arr) = serde_json::from_str::<Vec<Value>>(json_text) {
                return arr
                    .iter()
                    .filter_map(|m| {
                        Some(MetricInfo {
                            name: m.get("name")?.as_str()?.to_string(),
                            location: m
                                .get("location")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            expressions: if let Some(arr) =
                                m.get("expressions").and_then(|v| v.as_array())
                            {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            } else if let Some(expr) = m.get("expression").and_then(|v| v.as_str())
                            {
                                vec![expr.to_string()]
                            } else {
                                vec![]
                            },
                            group: m
                                .get("group")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            enabled: m.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
                            mode: m
                                .get("mode")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                        })
                    })
                    .collect();
            }
        }

        // Try parsing as direct JSON array
        if let Ok(parsed_metrics) = serde_json::from_str::<Vec<MetricInfo>>(&content) {
            return parsed_metrics;
        }

        vec![]
    }

    /// Parse events from MCP response
    ///
    /// The MCP server returns events in TOON format by default (for LLM efficiency).
    /// This function tries multiple parsing strategies:
    /// 1. TOON format (default from server)
    /// 2. JSON format (fallback)
    fn parse_events(&self, json: &Value) -> Vec<EventInfo> {
        let text = match self.extract_text(json) {
            Some(t) => t,
            None => return vec![],
        };

        // Skip the summary line ("Found X events...") to get the data content
        let content = text
            .lines()
            .skip_while(|l| l.starts_with("Found"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();

        if content.is_empty() {
            return vec![];
        }

        // Handle empty arrays (both TOON "[0]:" and JSON "[]" formats)
        if content == "[]" || content == "[0]:" {
            return vec![];
        }

        // Try parsing as TOON format first (default from server)
        // Use non-strict mode to handle array length mismatches and other edge cases
        let toon_options = toon_format::DecodeOptions::new().with_strict(false);

        // First try direct TOON decode to EventInfo vec
        if let Ok(parsed_events) = toon_format::decode::<Vec<EventInfo>>(&content, &toon_options) {
            return parsed_events;
        }

        // Try TOON decode to serde_json::Value first, then convert
        // This works around issues with nested tabular arrays and flatten attributes
        if let Ok(json_value) = toon_format::decode::<serde_json::Value>(&content, &toon_options) {
            if let Ok(parsed_events) = serde_json::from_value::<Vec<EventInfo>>(json_value) {
                return parsed_events;
            }
        }

        // Try parsing as JSON array (server may have fallen back to JSON)
        if let Ok(parsed_events) = serde_json::from_str::<Vec<EventInfo>>(&content) {
            return parsed_events;
        }

        // Try JSON from full text
        if let Ok(parsed_events) = serde_json::from_str::<Vec<EventInfo>>(&text) {
            return parsed_events;
        }

        // Try parsing each line as a separate JSON object
        let mut events = vec![];
        let mut json_buffer = String::new();
        let mut in_json = false;
        let mut brace_count = 0;

        for line in text.lines() {
            let trimmed = line.trim();

            // Skip summary lines
            if trimmed.starts_with("Found") || trimmed.is_empty() {
                continue;
            }

            // Track JSON object boundaries
            if trimmed.starts_with('{') || in_json {
                in_json = true;
                json_buffer.push_str(line);
                json_buffer.push('\n');
                brace_count += trimmed.chars().filter(|&c| c == '{').count() as i32;
                brace_count -= trimmed.chars().filter(|&c| c == '}').count() as i32;

                if brace_count <= 0 {
                    // Try to parse the accumulated JSON
                    if let Ok(event) = serde_json::from_str::<EventInfo>(&json_buffer) {
                        events.push(event);
                    }
                    json_buffer.clear();
                    in_json = false;
                    brace_count = 0;
                }
            }
        }

        if events.is_empty() {
            eprintln!("[DEBUG parse_events] No events parsed from any format");
        }

        events
    }

    /// Parse metric count from response text like "✅ Enabled 2 metrics in group 'test'"
    fn parse_metric_count(&self, text: &str) -> usize {
        // Look for patterns like "Enabled 2 metrics" or "Disabled 3 metrics"
        for word in text.split_whitespace() {
            if let Ok(n) = word.parse::<usize>() {
                return n;
            }
        }
        0
    }

    /// Parse system events from MCP response
    fn parse_system_events(&self, json: &Value) -> Vec<SystemEventInfo> {
        let text = match self.extract_text(json) {
            Some(t) => t,
            None => return vec![],
        };

        // Skip summary lines like "Found X system events..."
        let content = text
            .lines()
            .skip_while(|l| l.starts_with("Found") || l.starts_with("⚠️") || l.starts_with("✅"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();

        if content.is_empty() || content == "[]" {
            return vec![];
        }

        // Check for "[N]:" count prefix (e.g., "[3]:", "[0]:")
        // If present, strip it and parse the remaining YAML-style content
        let content = if content.starts_with('[') && content.contains("]:") {
            // Find the end of the count prefix
            if let Some(idx) = content.find("]:") {
                let after_prefix = content[idx + 2..].trim_start_matches('\n').trim_end();
                // Check for empty result
                if after_prefix.is_empty() {
                    return vec![];
                }
                // Dedent by finding minimum indentation and removing it
                let lines: Vec<&str> = after_prefix.lines().collect();
                let min_indent = lines
                    .iter()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| l.len() - l.trim_start().len())
                    .min()
                    .unwrap_or(0);
                lines
                    .iter()
                    .map(|l| {
                        if l.len() >= min_indent {
                            &l[min_indent..]
                        } else {
                            l
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                content
            }
        } else {
            content
        };

        // Try parsing as JSON array first
        if let Ok(events) = serde_json::from_str::<Vec<SystemEventInfo>>(&content) {
            return events;
        }

        // Try TOON format (supports YAML-style list)
        let toon_options = toon_format::DecodeOptions::new().with_strict(false);
        if let Ok(events) = toon_format::decode::<Vec<SystemEventInfo>>(&content, &toon_options) {
            return events;
        }

        // Try TOON to Value then to SystemEventInfo
        if let Ok(json_value) = toon_format::decode::<serde_json::Value>(&content, &toon_options) {
            if let Ok(events) = serde_json::from_value::<Vec<SystemEventInfo>>(json_value) {
                return events;
            }
        }

        // Try full text as JSON
        if let Ok(events) = serde_json::from_str::<Vec<SystemEventInfo>>(&text) {
            return events;
        }

        vec![]
    }

    /// Parse groups from MCP response
    fn parse_groups(&self, json: &Value) -> Vec<GroupInfo> {
        let text = match self.extract_text(json) {
            Some(t) => t,
            None => return vec![],
        };

        let mut groups = vec![];

        // Try to parse JSON array from the response text
        // The server returns both a summary line and a JSON array
        for line in text.lines() {
            // Try parsing as JSON array
            if line.starts_with('[') {
                if let Ok(arr) = serde_json::from_str::<Vec<Value>>(line) {
                    for item in arr {
                        if let (Some(name), Some(metric_count), Some(enabled_count)) = (
                            item.get("name").and_then(|v| v.as_str()),
                            item.get("metric_count").and_then(|v| v.as_i64()),
                            item.get("enabled_count").and_then(|v| v.as_i64()),
                        ) {
                            groups.push(GroupInfo {
                                name: name.to_string(),
                                metric_count: metric_count as u32,
                                enabled_count: enabled_count as u32,
                            });
                        }
                    }
                }
            }
        }

        groups
    }

    /// Parse observe response from MCP output
    ///
    /// The MCP server returns nested JSON:
    /// ```json
    /// {
    ///   "success": true,
    ///   "metric": { "id": 1, "name": "...", "location": "@file:line", "expression": "..." },
    ///   "context": { "line_content": "...", "line_source": "...", "connection_id": "...", "alternatives": [...] },
    ///   "warnings": [...]
    /// }
    /// ```
    fn parse_observe_response(
        &self,
        text: &str,
        _json: &Value,
    ) -> Result<ObserveResponse, ApiError> {
        // Try to find and parse nested JSON structure (single line)
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('{') && trimmed.contains("\"metric\"") {
                if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
                    return self.parse_observe_json(&json);
                }
            }
        }

        // Try multiline JSON: find the JSON block in the text
        // The JSON starts with a line that is just "{" and ends with "}"
        if let Some(json_start) = text.find("\n{") {
            let json_text = &text[json_start + 1..]; // Skip the newline
            if let Ok(json) = serde_json::from_str::<Value>(json_text) {
                if json.get("metric").is_some() {
                    return self.parse_observe_json(&json);
                }
            }
        }

        // Try: the text might start with {
        if text.trim().starts_with('{') {
            if let Ok(json) = serde_json::from_str::<Value>(text.trim()) {
                if json.get("metric").is_some() {
                    return self.parse_observe_json(&json);
                }
            }
        }

        // Fallback: Parse from structured text output
        self.parse_observe_text(text)
    }

    /// Parse nested JSON observe response
    fn parse_observe_json(&self, json: &Value) -> Result<ObserveResponse, ApiError> {
        let success = json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let metric = json.get("metric").unwrap_or(&Value::Null);
        let context = json.get("context").unwrap_or(&Value::Null);

        let metric_id = metric.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let metric_name = metric
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let expression = metric
            .get("expression")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Parse location "@file:line"
        let location = metric
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("@unknown#0");
        let location = location.strip_prefix('@').unwrap_or(location);
        let (file, line) = if let Some((f, l)) = location.rsplit_once('#') {
            (f.to_string(), l.parse::<u32>().unwrap_or(0))
        } else {
            (location.to_string(), 0)
        };

        let line_source = context
            .get("line_source")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let connection_id = context
            .get("connection_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Parse alternatives array
        let alternatives = context
            .get("alternatives")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let line = item.get("line").and_then(|v| v.as_u64())? as u32;
                        let content = item.get("content").and_then(|v| v.as_str())?;
                        Some((line, content.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Parse warnings array
        let warnings = json
            .get("warnings")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(ObserveResponse {
            success,
            metric_id,
            metric_name,
            file,
            line,
            expression,
            line_source,
            connection_id,
            alternatives,
            warnings,
        })
    }

    /// Parse enable_from_diff response from MCP output
    ///
    /// The MCP server returns JSON with structured data:
    /// ```json
    /// {
    ///   "success": true,
    ///   "added": [{"name": "...", "file": "...", "line": N, "id": N}],
    ///   "failed": [],
    ///   "needs_review": [],
    ///   "connection_id": "..."
    /// }
    /// ```
    fn parse_enable_from_diff_response(
        &self,
        text: &str,
    ) -> Result<EnableFromDiffResponse, ApiError> {
        // Try to find and parse nested JSON structure
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('{') && trimmed.contains("\"added\"") {
                if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
                    return self.parse_enable_from_diff_json(&json);
                }
            }
        }

        // Try multiline JSON
        if let Some(json_start) = text.find("\n{") {
            let json_text = &text[json_start + 1..];
            if let Ok(json) = serde_json::from_str::<Value>(json_text) {
                if json.get("added").is_some() {
                    return self.parse_enable_from_diff_json(&json);
                }
            }
        }

        // Try: the text might start with {
        if text.trim().starts_with('{') {
            if let Ok(json) = serde_json::from_str::<Value>(text.trim()) {
                if json.get("added").is_some() {
                    return self.parse_enable_from_diff_json(&json);
                }
            }
        }

        // Fallback: return empty response with success=false if nothing parsed
        Err(ApiError::new("Failed to parse enable_from_diff response"))
    }

    /// Parse enable_from_diff JSON response
    fn parse_enable_from_diff_json(
        &self,
        json: &Value,
    ) -> Result<EnableFromDiffResponse, ApiError> {
        let success = json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let connection_id = json
            .get("connection_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Parse added metrics
        let added = json
            .get("added")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        Some(DiffMetricAdded {
                            name: item.get("name")?.as_str()?.to_string(),
                            file: item.get("file")?.as_str()?.to_string(),
                            line: item.get("line")?.as_u64()? as u32,
                            id: item.get("id")?.as_i64()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Parse failed metrics
        let failed = json
            .get("failed")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        Some(DiffMetricFailed {
                            file: item.get("file")?.as_str()?.to_string(),
                            line: item.get("line")?.as_u64()? as u32,
                            statement: item.get("statement")?.as_str()?.to_string(),
                            error: item.get("error")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Parse needs_review metrics
        let needs_review = json
            .get("needs_review")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        Some(DiffMetricNeedsReview {
                            file: item.get("file")?.as_str()?.to_string(),
                            line: item.get("line")?.as_u64()? as u32,
                            statement: item.get("statement")?.as_str()?.to_string(),
                            suggested_expression: item
                                .get("suggested_expression")?
                                .as_str()?
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(EnableFromDiffResponse {
            success,
            added,
            failed,
            needs_review,
            connection_id,
        })
    }

    /// Fallback text parsing for observe response
    fn parse_observe_text(&self, text: &str) -> Result<ObserveResponse, ApiError> {
        let metric_name = text
            .find("metric: ")
            .map(|pos| {
                let start = pos + "metric: ".len();
                text[start..]
                    .find([',', ')'])
                    .map(|end| text[start..start + end].trim().to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| "unknown".to_string());

        let connection_id = text
            .find("connection: ")
            .map(|pos| {
                let start = pos + "connection: ".len();
                text[start..]
                    .find(')')
                    .map(|end| text[start..start + end].trim().to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| "unknown".to_string());

        let metric_id = text
            .find("ID: ")
            .and_then(|pos| {
                let start = pos + "ID: ".len();
                text[start..]
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or(0);

        let (file, line) = text
            .find(" at ")
            .map(|pos| {
                let after_at = &text[pos + 4..];
                let end = after_at.find(' ').unwrap_or(after_at.len());
                let location = &after_at[..end];
                if let Some((f, l)) = location.rsplit_once('#') {
                    (f.to_string(), l.parse::<u32>().unwrap_or(0))
                } else {
                    (location.to_string(), 0)
                }
            })
            .unwrap_or_else(|| ("unknown".to_string(), 0));

        let expression = text
            .find('\'')
            .and_then(|start| {
                let rest = &text[start + 1..];
                rest.find('\'').map(|end| rest[..end].to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        let line_source = if text.contains("explicit") || text.contains("specified") {
            "explicit".to_string()
        } else if text.contains("auto") || text.contains("found") {
            "auto_found".to_string()
        } else {
            "unknown".to_string()
        };

        Ok(ObserveResponse {
            success: !text.contains("Error") && !text.contains("error:"),
            metric_id,
            metric_name,
            file,
            line,
            expression,
            line_source,
            connection_id,
            alternatives: vec![],
            warnings: vec![],
        })
    }
}

#[async_trait]
impl ApiClient for McpClient {
    fn api_type(&self) -> &'static str {
        "MCP"
    }

    // ========================================================================
    // Health & Status
    // ========================================================================

    async fn health(&self) -> Result<bool, ApiError> {
        self.client
            .get(format!("{}/health", self.base_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map(|r| r.status().is_success())
            .map_err(|e| ApiError::new(format!("Health check failed: {}", e)))
    }

    async fn get_status(&self) -> ApiResult<StatusInfo> {
        let json = self.call("get_status", json!({})).await?;
        let text = self.extract_text(&json).unwrap_or_default();

        // Parse from response text (simplified)
        Ok(ApiResponse::new(StatusInfo {
            mode: if text.contains("active") {
                "active".to_string()
            } else {
                "sleeping".to_string()
            },
            uptime_seconds: 0,
            active_connections: 0,
            total_metrics: 0,
            enabled_metrics: 0,
        })
        .with_raw(json.to_string()))
    }

    async fn wake(&self) -> ApiResult<String> {
        let json = self.call("wake", json!({})).await?;
        let text = self.extract_text(&json).unwrap_or_default();
        Ok(ApiResponse::new(text).with_raw(json.to_string()))
    }

    async fn sleep(&self) -> ApiResult<String> {
        let json = self.call("sleep", json!({})).await?;
        let text = self.extract_text(&json).unwrap_or_default();
        Ok(ApiResponse::new(text).with_raw(json.to_string()))
    }

    // ========================================================================
    // Connection Management
    // ========================================================================

    async fn create_connection(
        &self,
        host: &str,
        port: u16,
        language: &str,
    ) -> ApiResult<ConnectionInfo> {
        let json = self
            .call(
                "create_connection",
                json!({
                    "host": host,
                    "port": port, // JSON number, MCP accepts i32
                    "language": language
                }),
            )
            .await?;

        // Parse connection_id from response
        // Response has two formats:
        // 1. Text: "✅ Connected to debugpy at host:port (connection_id: {id})"
        // 2. JSON: {"connection_id": "...", ...}
        let text = self.extract_text(&json).unwrap_or_default();
        let connection_id = text
            // Try to extract from "(connection_id: {id})" pattern
            .find("connection_id: ")
            .map(|pos| {
                let start = pos + "connection_id: ".len();
                text[start..]
                    .find(')')
                    .map(|end| text[start..start + end].trim().to_string())
                    .unwrap_or_else(|| text[start..].trim().to_string())
            })
            // Or try JSON "connection_id": "{id}" pattern
            .or_else(|| {
                text.find("\"connection_id\": \"").map(|pos| {
                    let start = pos + "\"connection_id\": \"".len();
                    text[start..]
                        .find('"')
                        .map(|end| text[start..start + end].to_string())
                        .unwrap_or_default()
                })
            })
            .unwrap_or_else(|| format!("{}_{}", host.replace('.', "_"), port));

        Ok(ApiResponse::new(ConnectionInfo {
            connection_id,
            host: host.to_string(),
            port,
            language: language.to_string(),
            status: detrix_core::ConnectionStatus::Connected.to_status_string(),
            safe_mode: false, // Default to false
        })
        .with_raw(json.to_string()))
    }

    async fn create_connection_with_program(
        &self,
        host: &str,
        port: u16,
        language: &str,
        program: Option<&str>,
    ) -> ApiResult<ConnectionInfo> {
        let mut args = json!({
            "host": host,
            "port": port,
            "language": language
        });

        if let Some(prog) = program {
            args["program"] = json!(prog);
        }

        let json = self.call("create_connection", args).await?;

        // Parse connection_id from response (same logic as create_connection)
        let text = self.extract_text(&json).unwrap_or_default();
        let connection_id = text
            .find("connection_id: ")
            .map(|pos| {
                let start = pos + "connection_id: ".len();
                text[start..]
                    .find(')')
                    .map(|end| text[start..start + end].trim().to_string())
                    .unwrap_or_else(|| text[start..].trim().to_string())
            })
            .or_else(|| {
                text.find("\"connection_id\": \"").map(|pos| {
                    let start = pos + "\"connection_id\": \"".len();
                    text[start..]
                        .find('"')
                        .map(|end| text[start..start + end].to_string())
                        .unwrap_or_default()
                })
            })
            .unwrap_or_else(|| format!("{}_{}", host.replace('.', "_"), port));

        Ok(ApiResponse::new(ConnectionInfo {
            connection_id,
            host: host.to_string(),
            port,
            language: language.to_string(),
            status: detrix_core::ConnectionStatus::Connected.to_status_string(),
            safe_mode: false, // Default to false
        })
        .with_raw(json.to_string()))
    }

    async fn close_connection(&self, connection_id: &str) -> ApiResult<()> {
        self.call(
            "close_connection",
            json!({ "connection_id": connection_id }),
        )
        .await?;
        Ok(ApiResponse::new(()))
    }

    async fn get_connection(&self, connection_id: &str) -> ApiResult<ConnectionInfo> {
        let json = self
            .call("get_connection", json!({ "connection_id": connection_id }))
            .await?;

        // Parse connection from response (simplified)
        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: connection_id.to_string(),
            host: "127.0.0.1".to_string(),
            port: 5678,
            language: "python".to_string(),
            status: detrix_core::ConnectionStatus::Connected.to_status_string(),
            safe_mode: false, // Default to false
        })
        .with_raw(json.to_string()))
    }

    async fn list_connections(&self) -> ApiResult<Vec<ConnectionInfo>> {
        let json = self.call("list_connections", json!({})).await?;
        let connections = self.parse_connections(&json);
        Ok(ApiResponse::new(connections).with_raw(json.to_string()))
    }

    // ========================================================================
    // Metric Management
    // ========================================================================

    async fn add_metric(&self, request: AddMetricRequest) -> ApiResult<String> {
        let mut args = json!({
            "name": request.name,
            "location": request.location,
            "expressions": request.expressions,
            "connection_id": request.connection_id
        });

        if let Some(group) = &request.group {
            args["group"] = json!(group);
        }
        if let Some(mode) = &request.mode {
            args["mode"] = json!(mode);
        }
        if let Some(enabled) = request.enabled {
            args["enabled"] = json!(enabled);
        }
        // Introspection options
        if let Some(capture_stack_trace) = request.capture_stack_trace {
            args["capture_stack_trace"] = json!(capture_stack_trace);
        }
        if let Some(stack_trace_ttl) = request.stack_trace_ttl {
            args["stack_trace_ttl"] = json!(stack_trace_ttl);
        }
        if let Some(stack_trace_full) = request.stack_trace_full {
            args["stack_trace_full"] = json!(stack_trace_full);
        }
        if let Some(stack_trace_head) = request.stack_trace_head {
            args["stack_trace_head"] = json!(stack_trace_head);
        }
        if let Some(stack_trace_tail) = request.stack_trace_tail {
            args["stack_trace_tail"] = json!(stack_trace_tail);
        }
        if let Some(capture_memory_snapshot) = request.capture_memory_snapshot {
            args["capture_memory_snapshot"] = json!(capture_memory_snapshot);
        }
        if let Some(snapshot_scope) = &request.snapshot_scope {
            args["snapshot_scope"] = json!(snapshot_scope);
        }
        if let Some(snapshot_ttl) = request.snapshot_ttl {
            args["snapshot_ttl"] = json!(snapshot_ttl);
        }

        let json = self.call("add_metric", args).await?;
        let text = self.extract_text(&json).unwrap_or_default();

        // Extract metric_id from response
        let metric_id = text
            .lines()
            .find(|l| l.contains("ID"))
            .and_then(|l| l.split_whitespace().find(|w| w.parse::<i64>().is_ok()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "0".to_string());

        Ok(ApiResponse::new(metric_id).with_raw(json.to_string()))
    }

    async fn remove_metric(&self, name: &str) -> ApiResult<()> {
        self.call("remove_metric", json!({ "name": name })).await?;
        Ok(ApiResponse::new(()))
    }

    async fn toggle_metric(&self, name: &str, enabled: bool) -> ApiResult<()> {
        self.call("toggle_metric", json!({ "name": name, "enabled": enabled }))
            .await?;
        Ok(ApiResponse::new(()))
    }

    async fn get_metric(&self, name: &str) -> ApiResult<MetricInfo> {
        let json = self.call("get_metric", json!({ "name": name })).await?;

        // Parse metric from response (simplified)
        Ok(ApiResponse::new(MetricInfo {
            name: name.to_string(),
            location: String::new(),
            expressions: vec![],
            group: None,
            enabled: true,
            mode: None,
        })
        .with_raw(json.to_string()))
    }

    async fn list_metrics(&self) -> ApiResult<Vec<MetricInfo>> {
        // Request JSON format for reliable parsing
        let json = self
            .call("list_metrics", json!({ "format": "json" }))
            .await?;
        let metrics = self.parse_metrics_response(&json);
        Ok(ApiResponse::new(metrics).with_raw(json.to_string()))
    }

    async fn query_events(&self, metric_name: &str, limit: u32) -> ApiResult<Vec<EventInfo>> {
        // Request JSON format for better compatibility with complex nested structures
        // like stack traces and memory snapshots (TOON has issues with nested tabular arrays)
        let json = self
            .call(
                "query_metrics",
                json!({
                    "name": metric_name,
                    "limit": limit,
                    "format": "json"
                }),
            )
            .await?;
        let events = self.parse_events(&json);
        Ok(ApiResponse::new(events).with_raw(json.to_string()))
    }

    // ========================================================================
    // Group Management
    // ========================================================================

    async fn enable_group(&self, group: &str) -> ApiResult<usize> {
        let json = self.call("enable_group", json!({ "group": group })).await?;
        // Parse count from response like "✅ Enabled 2 metrics in group 'test'"
        let text = self.extract_text(&json).unwrap_or_default();
        let count = self.parse_metric_count(&text);
        Ok(ApiResponse::new(count).with_raw(json.to_string()))
    }

    async fn disable_group(&self, group: &str) -> ApiResult<usize> {
        let json = self
            .call("disable_group", json!({ "group": group }))
            .await?;
        // Parse count from response like "✅ Disabled 2 metrics in group 'test'"
        let text = self.extract_text(&json).unwrap_or_default();
        let count = self.parse_metric_count(&text);
        Ok(ApiResponse::new(count).with_raw(json.to_string()))
    }

    async fn list_groups(&self) -> ApiResult<Vec<GroupInfo>> {
        let json = self.call("list_groups", json!({})).await?;
        // Parse groups from JSON response
        let groups = self.parse_groups(&json);
        Ok(ApiResponse::new(groups).with_raw(json.to_string()))
    }

    // ========================================================================
    // Validation
    // ========================================================================

    async fn validate_expression(
        &self,
        expression: &str,
        language: &str,
    ) -> ApiResult<ValidationResult> {
        let json = self
            .call(
                "validate_expression",
                json!({
                    "expression": expression,
                    "language": language
                }),
            )
            .await?;

        let text = self.extract_text(&json).unwrap_or_default();
        let is_safe = text.contains("SAFE") && !text.contains("UNSAFE");

        Ok(ApiResponse::new(ValidationResult {
            is_safe,
            issues: vec![],
        })
        .with_raw(json.to_string()))
    }

    async fn inspect_file(
        &self,
        file_path: &str,
        line: Option<u32>,
        find_variable: Option<&str>,
    ) -> ApiResult<String> {
        let mut args = json!({ "file_path": file_path });
        if let Some(l) = line {
            args["line"] = json!(l);
        }
        if let Some(v) = find_variable {
            args["find_variable"] = json!(v);
        }

        let json = self.call("inspect_file", args).await?;
        let text = self.extract_text(&json).unwrap_or_default();
        Ok(ApiResponse::new(text).with_raw(json.to_string()))
    }

    // ========================================================================
    // Configuration
    // ========================================================================

    async fn get_config(&self) -> ApiResult<String> {
        let json = self.call("get_config", json!({})).await?;
        let text = self.extract_text(&json).unwrap_or_default();
        Ok(ApiResponse::new(text).with_raw(json.to_string()))
    }

    async fn update_config(&self, config_toml: &str, persist: bool) -> ApiResult<()> {
        self.call(
            "update_config",
            json!({
                "config_toml": config_toml,
                "persist": persist
            }),
        )
        .await?;
        Ok(ApiResponse::new(()))
    }

    async fn validate_config(&self, config_toml: &str) -> ApiResult<bool> {
        let json = self
            .call("validate_config", json!({ "config_toml": config_toml }))
            .await?;

        let text = self.extract_text(&json).unwrap_or_default();
        let is_valid = text.contains("valid");

        Ok(ApiResponse::new(is_valid).with_raw(json.to_string()))
    }

    async fn reload_config(&self) -> ApiResult<()> {
        self.call("reload_config", json!({})).await?;
        Ok(ApiResponse::new(()))
    }

    // ========================================================================
    // System Events
    // ========================================================================

    async fn query_system_events(
        &self,
        limit: u32,
        unacknowledged_only: bool,
        event_types: Option<Vec<&str>>,
    ) -> ApiResult<Vec<SystemEventInfo>> {
        let mut args = json!({
            "limit": limit,
            "unacknowledged_only": unacknowledged_only,
            "format": "json"  // Use JSON for reliable parsing
        });

        if let Some(types) = event_types {
            args["event_types"] = json!(types);
        }

        let json = self.call("query_system_events", args).await?;
        let events = self.parse_system_events(&json);
        Ok(ApiResponse::new(events).with_raw(json.to_string()))
    }

    async fn acknowledge_system_events(&self, event_ids: Vec<i64>) -> ApiResult<u64> {
        let json = self
            .call("acknowledge_events", json!({ "event_ids": event_ids }))
            .await?;

        // Parse count from response like "✅ Acknowledged 3 events"
        let text = self.extract_text(&json).unwrap_or_default();
        let count = text
            .split_whitespace()
            .find_map(|w| w.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(ApiResponse::new(count).with_raw(json.to_string()))
    }

    async fn get_mcp_usage(&self) -> ApiResult<McpUsageInfo> {
        let json = self.call("get_mcp_usage", json!({})).await?;

        // The MCP server returns both human-readable text and JSON in result.content
        // Look for JSON in the content array
        if let Some(content) = json
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.as_array())
        {
            // Find the JSON content (second element typically)
            for item in content {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    // Try to parse as JSON (skip the human-readable emoji summary)
                    if let Ok(usage) = serde_json::from_str::<McpUsageInfo>(text) {
                        return Ok(ApiResponse::new(usage).with_raw(json.to_string()));
                    }
                }
            }
        }

        // Fallback: create default usage info if parsing fails
        Err(ApiError::new("Failed to parse MCP usage response").with_raw(json.to_string()))
    }

    // ========================================================================
    // Observe Tool (MCP-only convenience workflow)
    // ========================================================================

    async fn observe(&self, request: ObserveRequest) -> ApiResult<ObserveResponse> {
        let mut args = json!({
            "file": request.file,
            "expressions": request.expressions
        });

        // Add optional parameters
        if let Some(line) = request.line {
            args["line"] = json!(line);
        }
        if let Some(conn_id) = &request.connection_id {
            args["connection_id"] = json!(conn_id);
        }
        if let Some(name) = &request.name {
            args["name"] = json!(name);
        }
        if let Some(find_var) = &request.find_variable {
            args["find_variable"] = json!(find_var);
        }
        if let Some(group) = &request.group {
            args["group"] = json!(group);
        }
        if let Some(capture_stack) = request.capture_stack_trace {
            args["capture_stack_trace"] = json!(capture_stack);
        }
        if let Some(capture_mem) = request.capture_memory_snapshot {
            args["capture_memory_snapshot"] = json!(capture_mem);
        }
        if let Some(ttl) = request.ttl_seconds {
            args["ttl_seconds"] = json!(ttl);
        }

        let json = self.call("observe", args).await?;
        let text = self.extract_text(&json).unwrap_or_default();

        // Parse observe response from MCP tool output
        // The response contains JSON with structured data
        let response = self.parse_observe_response(&text, &json)?;
        Ok(ApiResponse::new(response).with_raw(json.to_string()))
    }

    async fn enable_from_diff(
        &self,
        request: EnableFromDiffRequest,
    ) -> ApiResult<EnableFromDiffResponse> {
        let mut args = json!({
            "diff": request.diff
        });

        if let Some(conn_id) = &request.connection_id {
            args["connection_id"] = json!(conn_id);
        }
        if let Some(group) = &request.group {
            args["group"] = json!(group);
        }
        if let Some(ttl) = request.ttl_seconds {
            args["ttl_seconds"] = json!(ttl);
        }

        let json = self.call("enable_from_diff", args).await?;
        let text = self.extract_text(&json).unwrap_or_default();

        // Parse enable_from_diff response
        let response = self.parse_enable_from_diff_response(&text)?;
        Ok(ApiResponse::new(response).with_raw(json.to_string()))
    }
}
