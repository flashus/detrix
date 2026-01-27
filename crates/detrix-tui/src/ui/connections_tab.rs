//! Connections tab rendering

use super::formatters::truncate;
use crate::app::App;
use chrono::Utc;
use detrix_config::constants::{
    TUI_CONN_ADDRESS_WIDTH, TUI_CONN_ID_WIDTH, TUI_CONN_LANGUAGE_WIDTH,
    TUI_CONN_LAST_ACTIVE_MIN_WIDTH, TUI_CONN_METRICS_WIDTH, TUI_CONN_STATUS_ERROR_TRUNCATE_LEN,
    TUI_CONN_STATUS_WIDTH,
};
use detrix_core::ConnectionStatus;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

/// Render the Connections tab
pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec![
        "ID",
        "Address",
        "Language",
        "Status",
        "Metrics",
        "Last Active",
    ])
    .style(app.theme.header)
    .height(1);

    let rows: Vec<Row> = app
        .connections
        .iter()
        .map(|conn| {
            let id = conn.id.0.clone();
            let address = format!("{}:{}", conn.host, conn.port);
            let language = conn.language.display_name().to_string();
            let (status_text, status_style) = format_status(&conn.status, app);
            let metrics = app
                .metrics
                .iter()
                .filter(|m| m.connection_id == conn.id)
                .count()
                .to_string();
            let last_active = format_relative_time(conn.last_active);

            Row::new(vec![
                Cell::from(truncate(&id, TUI_CONN_ID_WIDTH as usize)).style(app.theme.normal),
                Cell::from(address).style(app.theme.value),
                Cell::from(language).style(app.theme.normal),
                Cell::from(status_text).style(status_style),
                Cell::from(metrics).style(app.theme.value),
                Cell::from(last_active).style(app.theme.timestamp),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(TUI_CONN_ID_WIDTH),
        Constraint::Length(TUI_CONN_ADDRESS_WIDTH),
        Constraint::Length(TUI_CONN_LANGUAGE_WIDTH),
        Constraint::Length(TUI_CONN_STATUS_WIDTH),
        Constraint::Length(TUI_CONN_METRICS_WIDTH),
        Constraint::Min(TUI_CONN_LAST_ACTIVE_MIN_WIDTH),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title("Connections")
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .row_highlight_style(app.theme.selected)
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(table, area, &mut app.connections_state);
}

/// Format connection status with appropriate style
fn format_status(status: &ConnectionStatus, app: &App) -> (String, Style) {
    match status {
        ConnectionStatus::Connected => ("● Connected".to_string(), app.theme.connected),
        ConnectionStatus::Connecting => ("◐ Connecting".to_string(), app.theme.connecting),
        ConnectionStatus::Reconnecting => ("◐ Reconnecting".to_string(), app.theme.warning),
        ConnectionStatus::Disconnected => ("○ Disconnected".to_string(), app.theme.disconnected),
        ConnectionStatus::Failed(msg) => {
            let short_msg = if msg.chars().count() > TUI_CONN_STATUS_ERROR_TRUNCATE_LEN {
                let truncated: String = msg
                    .chars()
                    .take(TUI_CONN_STATUS_ERROR_TRUNCATE_LEN)
                    .collect();
                format!("⊗ {}…", truncated)
            } else {
                format!("⊗ {}", msg)
            };
            (short_msg, app.theme.error)
        }
    }
}

/// Format timestamp as relative time
fn format_relative_time(micros: i64) -> String {
    use detrix_core::{SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MINUTE};

    let now = Utc::now().timestamp_micros();
    let diff_secs = (now - micros) / 1_000_000;

    if diff_secs < 0 {
        "future".to_string()
    } else if diff_secs < SECS_PER_MINUTE as i64 {
        format!("{}s ago", diff_secs)
    } else if diff_secs < SECS_PER_HOUR as i64 {
        format!("{}m ago", diff_secs / SECS_PER_MINUTE as i64)
    } else if diff_secs < SECS_PER_DAY as i64 {
        format!("{}h ago", diff_secs / SECS_PER_HOUR as i64)
    } else {
        format!("{}d ago", diff_secs / SECS_PER_DAY as i64)
    }
}
