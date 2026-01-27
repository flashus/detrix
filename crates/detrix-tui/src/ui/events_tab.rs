//! Events tab rendering

use super::formatters::{compact_json, format_json_pretty, format_timestamp, truncate};
use crate::app::App;
use detrix_config::constants::{
    TUI_CONNECTION_ID_TRUNCATE_LEN, TUI_EVENTS_CONNECTION_WIDTH, TUI_EVENTS_DETAILS_PERCENT,
    TUI_EVENTS_LIST_PERCENT, TUI_EVENTS_METRIC_WIDTH, TUI_EVENTS_PIN_WIDTH, TUI_EVENTS_TIME_WIDTH,
    TUI_EVENTS_VALUE_MIN_WIDTH, TUI_EVENTS_VALUE_TRUNCATE_LEN,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};

/// Render the Events tab
pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::horizontal([
        Constraint::Percentage(TUI_EVENTS_LIST_PERCENT),
        Constraint::Percentage(TUI_EVENTS_DETAILS_PERCENT),
    ])
    .split(area);

    render_event_list(frame, app, chunks[0]);
    render_event_details(frame, app, chunks[1]);
}

/// Render the event list
fn render_event_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(vec!["", "Time", "Metric", "Value", "Conn"])
        .style(app.theme.header)
        .height(1);

    // Get the current index of the pinned event (it moves as new events come in)
    let pinned_idx = app.pinned_event_index();
    let has_pin = app.has_pinned_event();

    let rows: Vec<Row> = app
        .events
        .iter()
        .enumerate()
        .map(|(idx, event)| {
            let timestamp = format_timestamp(event.timestamp);
            let name = event.metric_name.clone();
            let value = format_value(event);
            let conn = event.connection_id.0.clone();

            // Show pin marker "▸" for pinned event
            let pin_marker = if Some(idx) == pinned_idx { "▸" } else { " " };

            let style = if event.is_error {
                app.theme.error
            } else {
                app.theme.normal
            };

            Row::new(vec![
                Cell::from(pin_marker).style(app.theme.metric_name),
                Cell::from(timestamp).style(app.theme.timestamp),
                Cell::from(name).style(app.theme.metric_name),
                Cell::from(value).style(app.theme.value),
                Cell::from(truncate(&conn, TUI_CONNECTION_ID_TRUNCATE_LEN)).style(app.theme.normal),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(TUI_EVENTS_PIN_WIDTH),
        Constraint::Length(TUI_EVENTS_TIME_WIDTH),
        Constraint::Length(TUI_EVENTS_METRIC_WIDTH),
        Constraint::Min(TUI_EVENTS_VALUE_MIN_WIDTH),
        Constraint::Length(TUI_EVENTS_CONNECTION_WIDTH),
    ];

    let title = if app.streaming_paused {
        if has_pin {
            "Live Events [PAUSED] [PINNED]"
        } else {
            "Live Events [PAUSED]"
        }
    } else if has_pin {
        "Live Events [PINNED - Enter to unpin]"
    } else {
        "Live Events [Enter to pin]"
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .row_highlight_style(app.theme.selected)
        .highlight_symbol("> "); // Cursor marker (different from pin)

    frame.render_stateful_widget(table, area, &mut app.events_state);
}

/// Render the event details panel
fn render_event_details(frame: &mut Frame, app: &App, area: Rect) {
    let is_pinned = app.has_pinned_event();
    let title = if is_pinned {
        "Event Details [PINNED]"
    } else {
        "Event Details"
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(app.theme.border);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Use pinned event if set, otherwise selected event
    if let Some(event) = app.pinned_event() {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Metric: ", app.theme.normal),
                Span::styled(&event.metric_name, app.theme.metric_name),
            ]),
            Line::from(vec![
                Span::styled("ID: ", app.theme.normal),
                Span::styled(event.metric_id.0.to_string(), app.theme.value),
            ]),
            Line::from(vec![
                Span::styled("Connection: ", app.theme.normal),
                Span::styled(&event.connection_id.0, app.theme.value),
            ]),
            Line::from(""),
        ];

        if event.is_error {
            lines.push(Line::from(vec![
                Span::styled("ERROR: ", app.theme.error),
                Span::styled(
                    event.error_type.as_deref().unwrap_or("Unknown"),
                    app.theme.error,
                ),
            ]));
            if let Some(ref msg) = event.error_message {
                lines.push(Line::from(vec![
                    Span::styled("Message: ", app.theme.normal),
                    Span::styled(msg, app.theme.error),
                ]));
            }
        } else {
            lines.push(Line::from(vec![Span::styled("Value: ", app.theme.normal)]));
            // Pretty print JSON if possible
            let value_display = format_json_pretty(&event.value_json);
            for line in value_display.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), app.theme.value)));
            }
        }

        lines.push(Line::from(""));

        if let Some(ref thread) = event.thread_name {
            lines.push(Line::from(vec![
                Span::styled("Thread: ", app.theme.normal),
                Span::styled(thread, app.theme.value),
            ]));
        }

        if let Some(thread_id) = event.thread_id {
            lines.push(Line::from(vec![
                Span::styled("Thread ID: ", app.theme.normal),
                Span::styled(thread_id.to_string(), app.theme.value),
            ]));
        }

        if let Some(ref req_id) = event.request_id {
            lines.push(Line::from(vec![
                Span::styled("Request ID: ", app.theme.normal),
                Span::styled(req_id, app.theme.value),
            ]));
        }

        // Stack trace if present
        if let Some(ref stack) = event.stack_trace {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "── Stack Trace ──",
                app.theme.header,
            )));
            for frame in &stack.frames {
                let frame_text = format!(
                    "{}: {} ({}:{})",
                    frame.index,
                    frame.name,
                    frame.file.as_deref().unwrap_or("?"),
                    frame.line.unwrap_or(0)
                );
                lines.push(Line::from(Span::styled(frame_text, app.theme.normal)));
            }
        }

        // Memory snapshot if present
        if let Some(ref snapshot) = event.memory_snapshot {
            if !snapshot.locals.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "── Variables ──",
                    app.theme.header,
                )));
                for var in &snapshot.locals {
                    lines.push(Line::from(vec![
                        Span::styled(&var.name, app.theme.metric_name),
                        Span::styled(" = ", app.theme.normal),
                        Span::styled(&var.value, app.theme.value),
                    ]));
                }
            }
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, inner);
    } else {
        let text = Paragraph::new("Select an event to view details")
            .style(app.theme.normal)
            .alignment(Alignment::Center);
        frame.render_widget(text, inner);
    }
}

/// Format event value for display
fn format_value(event: &detrix_core::MetricEvent) -> String {
    if event.is_error {
        event.error_type.as_deref().unwrap_or("Error").to_string()
    } else if event.value_json.is_empty() {
        "(empty)".to_string()
    } else {
        // Try to format as compact JSON
        truncate(
            &compact_json(&event.value_json),
            TUI_EVENTS_VALUE_TRUNCATE_LEN,
        )
    }
}
