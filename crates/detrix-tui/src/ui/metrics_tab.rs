//! Metrics tab rendering

use super::formatters::truncate;
use crate::app::App;
use detrix_config::constants::{
    TUI_CONNECTION_ID_TRUNCATE_LEN, TUI_METRICS_CONNECTION_WIDTH, TUI_METRICS_ENABLED_WIDTH,
    TUI_METRICS_LOCATION_MIN_WIDTH, TUI_METRICS_MODE_WIDTH, TUI_METRICS_NAME_WIDTH,
    TUI_METRICS_PATH_TRUNCATE_LEN,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

/// Render the Metrics tab
pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["Name", "Location", "Mode", "Enabled", "Connection"])
        .style(app.theme.header)
        .height(1);

    let rows: Vec<Row> = app
        .metrics
        .iter()
        .map(|metric| {
            let name = metric.name.clone();
            let location = format!(
                "{}:{}",
                truncate_path(&metric.location.file, TUI_METRICS_PATH_TRUNCATE_LEN),
                metric.location.line
            );
            let mode = metric.mode.as_str().to_string();
            let enabled = if metric.enabled { "✓" } else { "✗" };
            let conn = metric.connection_id.0.clone();

            let enabled_style = if metric.enabled {
                app.theme.connected
            } else {
                app.theme.disconnected
            };

            Row::new(vec![
                Cell::from(name).style(app.theme.metric_name),
                Cell::from(location).style(app.theme.normal),
                Cell::from(mode).style(app.theme.value),
                Cell::from(enabled).style(enabled_style),
                Cell::from(truncate(&conn, TUI_CONNECTION_ID_TRUNCATE_LEN)).style(app.theme.normal),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(TUI_METRICS_NAME_WIDTH),
        Constraint::Min(TUI_METRICS_LOCATION_MIN_WIDTH),
        Constraint::Length(TUI_METRICS_MODE_WIDTH),
        Constraint::Length(TUI_METRICS_ENABLED_WIDTH),
        Constraint::Length(TUI_METRICS_CONNECTION_WIDTH),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title("Metrics")
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .row_highlight_style(app.theme.selected)
        .highlight_symbol("▸ ");

    frame.render_stateful_widget(table, area, &mut app.metrics_state);
}

/// Truncate a file path, keeping the filename
fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }

    // Try to keep the filename
    if let Some(pos) = path.rfind('/') {
        let filename = &path[pos + 1..];
        if filename.len() < max_len - 3 {
            return format!("…{}", &path[path.len() - max_len + 1..]);
        }
    }

    format!("{}…", &path[..max_len - 1])
}
