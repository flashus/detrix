//! System log tab rendering

use super::formatters::{format_timestamp_short, truncate};
use crate::app::{App, LogLevel};
use detrix_config::constants::{
    TUI_CONNECTION_ID_TRUNCATE_LEN, TUI_LOGS_AGE_WIDTH, TUI_LOGS_LEVEL_WIDTH,
    TUI_LOGS_MESSAGE_MIN_WIDTH, TUI_MCP_ACTIVITY_WIDTH, TUI_MCP_CLIENT_ID_TRUNCATE_LEN,
    TUI_MCP_CLIENT_ID_WIDTH, TUI_MCP_DURATION_WIDTH, TUI_MCP_PARENT_WIDTH,
    TUI_SYSEVENT_CONNECTION_WIDTH, TUI_SYSEVENT_DETAILS_MIN_WIDTH, TUI_SYSEVENT_TIME_WIDTH,
    TUI_SYSEVENT_TYPE_WIDTH, TUI_SYSTEM_DAEMON_HEIGHT_NO_MCP, TUI_SYSTEM_DAEMON_HEIGHT_WITH_MCP,
    TUI_SYSTEM_EVENTS_PERCENT, TUI_SYSTEM_LOGS_PERCENT, TUI_TIMESTAMP_TRUNCATE_LEN,
};
use detrix_core::{format_uptime, SystemEventType};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use std::time::Instant;

/// Render the System Log tab
pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    // Split area: daemon info + mcp clients, system events, TUI logs
    let has_mcp_clients = !app.daemon_status.mcp_clients.is_empty();
    let daemon_height = if has_mcp_clients {
        TUI_SYSTEM_DAEMON_HEIGHT_WITH_MCP
    } else {
        TUI_SYSTEM_DAEMON_HEIGHT_NO_MCP
    };

    let chunks = Layout::vertical([
        Constraint::Length(daemon_height),
        Constraint::Percentage(TUI_SYSTEM_EVENTS_PERCENT),
        Constraint::Percentage(TUI_SYSTEM_LOGS_PERCENT),
    ])
    .split(area);

    render_daemon_info(frame, app, chunks[0]);
    render_system_events(frame, app, chunks[1]);
    render_tui_logs(frame, app, chunks[2]);
}

/// Render daemon info and MCP clients
fn render_daemon_info(frame: &mut Frame, app: &App, area: Rect) {
    let status = &app.daemon_status;

    // If we have MCP clients, show a table
    if !status.mcp_clients.is_empty() {
        let header = Row::new(vec!["Parent", "Duration", "Activity", "Client ID"])
            .style(app.theme.header)
            .height(1);

        let rows: Vec<Row> = status
            .mcp_clients
            .iter()
            .map(|client| {
                let duration = format_uptime(client.connected_duration_secs);
                let last_activity = if client.last_activity_ago_secs == 0 {
                    "now".to_string()
                } else if client.last_activity_ago_secs < 60 {
                    format!("{}s", client.last_activity_ago_secs)
                } else {
                    format!("{}m", client.last_activity_ago_secs / 60)
                };

                // Format parent process: "windsurf->detrix(pid)" or "unknown"
                let parent = if let Some(ref pp) = client.parent_process {
                    format!("{}->detrix({})", pp.name, pp.bridge_pid)
                } else {
                    "unknown".to_string()
                };

                Row::new(vec![
                    Cell::from(parent).style(app.theme.metric_name),
                    Cell::from(duration).style(app.theme.normal),
                    Cell::from(last_activity).style(app.theme.normal),
                    Cell::from(truncate(&client.id, TUI_MCP_CLIENT_ID_TRUNCATE_LEN))
                        .style(app.theme.timestamp),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(TUI_MCP_PARENT_WIDTH),
            Constraint::Length(TUI_MCP_DURATION_WIDTH),
            Constraint::Length(TUI_MCP_ACTIVITY_WIDTH),
            Constraint::Length(TUI_MCP_CLIENT_ID_WIDTH),
        ];

        let title = format!(
            "MCP Clients ({}) │ Daemon PID: {} │ Started: {}",
            status.mcp_clients.len(),
            status.daemon_pid,
            truncate(&status.started_at, TUI_TIMESTAMP_TRUNCATE_LEN)
        );
        let table = Table::new(rows, widths).header(header).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        );

        frame.render_widget(table, area);
    } else {
        // No MCP clients - show simple daemon info
        let info = format!(
            "Daemon PID: {} │ PPID: {} │ Status: {} │ Started: {}",
            status.daemon_pid,
            status.daemon_ppid,
            status.status,
            truncate(&status.started_at, 19)
        );
        let widget = ratatui::widgets::Paragraph::new(info)
            .style(app.theme.normal)
            .block(
                Block::default()
                    .title("Daemon Info")
                    .borders(Borders::ALL)
                    .border_style(app.theme.border),
            );
        frame.render_widget(widget, area);
    }
}

/// Render system events from daemon
fn render_system_events(frame: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["Time", "Type", "Connection", "Details"])
        .style(app.theme.header)
        .height(1);

    let rows: Vec<Row> = app
        .system_events
        .iter()
        .map(|event| {
            let timestamp = format_timestamp_short(event.timestamp);
            let (type_text, type_style) = format_event_type(&event.event_type, app);
            let connection = event
                .connection_id
                .as_ref()
                .map(|c| c.0.clone())
                .unwrap_or_else(|| "-".to_string());
            let details = format_details(event);

            Row::new(vec![
                Cell::from(timestamp).style(app.theme.timestamp),
                Cell::from(type_text).style(type_style),
                Cell::from(truncate(&connection, TUI_CONNECTION_ID_TRUNCATE_LEN))
                    .style(app.theme.normal),
                Cell::from(details).style(app.theme.normal),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(TUI_SYSEVENT_TIME_WIDTH),
        Constraint::Length(TUI_SYSEVENT_TYPE_WIDTH),
        Constraint::Length(TUI_SYSEVENT_CONNECTION_WIDTH),
        Constraint::Min(TUI_SYSEVENT_DETAILS_MIN_WIDTH),
    ];

    let title = format!("Daemon Events ({})", app.system_events.len());
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .row_highlight_style(app.theme.selected)
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(table, area, &mut app.system_state);
}

/// Render TUI internal logs
fn render_tui_logs(frame: &mut Frame, app: &App, area: Rect) {
    let start_time = Instant::now();

    let header = Row::new(vec!["Age", "Level", "Message"])
        .style(app.theme.header)
        .height(1);

    let rows: Vec<Row> = app
        .tui_logs
        .iter()
        .map(|entry| {
            let age = format_age(start_time, entry.timestamp);
            let (level_text, level_style) = format_log_level(entry.level, app);

            Row::new(vec![
                Cell::from(age).style(app.theme.timestamp),
                Cell::from(level_text).style(level_style),
                Cell::from(entry.message.clone()).style(app.theme.normal),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(TUI_LOGS_AGE_WIDTH),
        Constraint::Length(TUI_LOGS_LEVEL_WIDTH),
        Constraint::Min(TUI_LOGS_MESSAGE_MIN_WIDTH),
    ];

    let title = format!("TUI Logs ({})", app.tui_logs.len());
    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(app.theme.border),
    );

    frame.render_widget(table, area);
}

/// Format log level with style
fn format_log_level(level: LogLevel, app: &App) -> (String, Style) {
    match level {
        LogLevel::Info => ("INFO".to_string(), app.theme.connected),
        LogLevel::Warn => ("WARN".to_string(), app.theme.warning),
        LogLevel::Error => ("ERR".to_string(), app.theme.error),
    }
}

/// Format age (time since entry)
fn format_age(now: Instant, timestamp: Instant) -> String {
    use detrix_core::{SECS_PER_HOUR, SECS_PER_MINUTE};

    let elapsed = now.duration_since(timestamp);
    let secs = elapsed.as_secs();
    if secs < SECS_PER_MINUTE {
        format!("{}s ago", secs)
    } else if secs < SECS_PER_HOUR {
        format!("{}m ago", secs / SECS_PER_MINUTE)
    } else {
        format!("{}h ago", secs / SECS_PER_HOUR)
    }
}

/// Format event type with appropriate style and icon
fn format_event_type(event_type: &SystemEventType, app: &App) -> (String, Style) {
    match event_type {
        SystemEventType::MetricAdded => ("● metric_added".to_string(), app.theme.connected),
        SystemEventType::MetricRemoved => ("○ metric_removed".to_string(), app.theme.disconnected),
        SystemEventType::MetricToggled => ("◐ metric_toggled".to_string(), app.theme.normal),
        SystemEventType::MetricRelocated => ("⇄ metric_relocated".to_string(), app.theme.warning),
        SystemEventType::MetricOrphaned => ("⚠ metric_orphaned".to_string(), app.theme.error),
        SystemEventType::ConnectionCreated => {
            ("● connection_created".to_string(), app.theme.connected)
        }
        SystemEventType::ConnectionClosed => {
            ("○ connection_closed".to_string(), app.theme.disconnected)
        }
        SystemEventType::ConnectionLost => ("⚠ connection_lost".to_string(), app.theme.warning),
        SystemEventType::ConnectionRestored => {
            ("● connection_restored".to_string(), app.theme.connected)
        }
        SystemEventType::DebuggerCrash => ("✗ debugger_crash".to_string(), app.theme.error),
        // Audit events
        SystemEventType::ConfigUpdated => ("⚙ config_updated".to_string(), app.theme.normal),
        SystemEventType::ConfigValidationFailed => {
            ("⚠ config_validation_failed".to_string(), app.theme.warning)
        }
        SystemEventType::ApiCallExecuted => ("→ api_call".to_string(), app.theme.connected),
        SystemEventType::ApiCallFailed => ("✗ api_call_failed".to_string(), app.theme.error),
        SystemEventType::AuthenticationFailed => ("⛔ auth_failed".to_string(), app.theme.error),
        SystemEventType::ExpressionValidated => ("✓ expr_validated".to_string(), app.theme.normal),
    }
}

/// Format event details
fn format_details(event: &detrix_core::SystemEvent) -> String {
    let mut parts = Vec::new();

    if let Some(ref name) = event.metric_name {
        parts.push(format!("metric={}", name));
    }

    if let Some(ref json) = event.details_json {
        // Try to extract key fields from JSON
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
            if let Some(obj) = v.as_object() {
                for (k, v) in obj.iter().take(2) {
                    let val_str = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        serde_json::Value::Number(n) => n.to_string(),
                        _ => continue,
                    };
                    parts.push(format!("{}={}", k, truncate(&val_str, 15)));
                }
            }
        }
    }

    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(", ")
    }
}
