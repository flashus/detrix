//! Popup dialogs for help, filter, and connection switcher

use super::formatters::format_json_pretty;
use crate::app::{App, PopupType};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

/// Render popup overlay
pub fn render(frame: &mut Frame, app: &mut App, popup: &PopupType) {
    match popup {
        PopupType::Help => render_help_popup(frame, app),
        PopupType::ConnectionSwitcher => render_connection_switcher(frame, app),
        PopupType::Filter => render_filter_popup(frame, app),
        PopupType::MetricDetails(id) => render_metric_details(frame, app, *id),
        PopupType::EventDetails(idx) => render_event_details(frame, app, *idx),
        PopupType::ConfirmCloseConnection(conn_id) => {
            render_confirm_close_popup(frame, app, conn_id)
        }
    }
}

/// Calculate centered popup area
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

/// Render help popup
fn render_help_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 70, frame.area());

    // Clear the background
    frame.render_widget(Clear, area);

    let help_text = vec![
        Line::from(vec![Span::styled("Navigation", app.theme.header)]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ↑/k     ", app.theme.value),
            Span::styled("Move up", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  ↓/j     ", app.theme.value),
            Span::styled("Move down", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  PgUp/K  ", app.theme.value),
            Span::styled("Page up", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  PgDn/J  ", app.theme.value),
            Span::styled("Page down", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  Home/g  ", app.theme.value),
            Span::styled("Go to top", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  End/G   ", app.theme.value),
            Span::styled("Go to bottom", app.theme.normal),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("Tabs", app.theme.header)]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ←/h     ", app.theme.value),
            Span::styled("Previous tab", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  →/l     ", app.theme.value),
            Span::styled("Next tab", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  Tab     ", app.theme.value),
            Span::styled("Next tab", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  S-Tab   ", app.theme.value),
            Span::styled("Previous tab", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  F1-F4   ", app.theme.value),
            Span::styled("Jump to tab", app.theme.normal),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("Actions", app.theme.header)]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Space   ", app.theme.value),
            Span::styled("Pause/Resume streaming", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  c       ", app.theme.value),
            Span::styled("Clear events", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  s       ", app.theme.value),
            Span::styled("Switch connection", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  /       ", app.theme.value),
            Span::styled("Filter events", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  Enter   ", app.theme.value),
            Span::styled("Pin/unpin event (Events tab)", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  d       ", app.theme.value),
            Span::styled("Close connection (Connections tab)", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  ?       ", app.theme.value),
            Span::styled("Show this help", app.theme.normal),
        ]),
        Line::from(vec![
            Span::styled("  q/Esc   ", app.theme.value),
            Span::styled("Quit / Close popup", app.theme.normal),
        ]),
    ];

    let paragraph = Paragraph::new(help_text)
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render connection switcher popup
fn render_connection_switcher(frame: &mut Frame, app: &mut App) {
    let area = centered_rect(50, 50, frame.area());

    // Clear the background
    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .connections
        .iter()
        .map(|conn| {
            let (status_icon, style) = match &conn.status {
                detrix_core::ConnectionStatus::Connected => ("●", app.theme.connected),
                detrix_core::ConnectionStatus::Connecting => ("◐", app.theme.connecting),
                detrix_core::ConnectionStatus::Reconnecting => ("◐", app.theme.warning),
                detrix_core::ConnectionStatus::Disconnected => ("○", app.theme.disconnected),
                detrix_core::ConnectionStatus::Failed(_) => ("⊗", app.theme.error),
            };

            let is_selected = app
                .selected_connection
                .as_ref()
                .map(|id| *id == conn.id)
                .unwrap_or(false);

            let prefix = if is_selected { "▸ " } else { "  " };

            ListItem::new(Line::from(vec![
                Span::raw(prefix),
                Span::styled(status_icon, style),
                Span::raw(" "),
                Span::styled(&conn.id.0, app.theme.normal),
                Span::raw(" - "),
                Span::styled(format!("{}:{}", conn.host, conn.port), app.theme.value),
                Span::raw(" ("),
                Span::styled(conn.language.display_name(), app.theme.normal),
                Span::raw(")"),
            ]))
        })
        .collect();

    // Add "All connections" option at the top
    let mut all_items = vec![ListItem::new(Line::from(vec![
        Span::raw(if app.selected_connection.is_none() {
            "▸ "
        } else {
            "  "
        }),
        Span::styled("All Connections", app.theme.header),
    ]))];
    all_items.extend(items);

    let list = List::new(all_items)
        .block(
            Block::default()
                .title(" Select Connection ")
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .highlight_style(app.theme.selected);

    frame.render_stateful_widget(list, area, &mut app.connection_switcher_state);
}

/// Render filter popup
fn render_filter_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 30, frame.area());

    // Clear the background
    frame.render_widget(Clear, area);

    let content = vec![
        Line::from(vec![
            Span::styled("Filter: ", app.theme.normal),
            Span::styled(&app.filter_text, app.theme.value),
            Span::styled("_", app.theme.header), // Cursor
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Type to filter events by metric name",
            app.theme.timestamp,
        )]),
        Line::from(vec![Span::styled(
            "Press Enter to apply, Esc to cancel",
            app.theme.timestamp,
        )]),
    ];

    let paragraph = Paragraph::new(content).block(
        Block::default()
            .title(" Filter Events ")
            .borders(Borders::ALL)
            .border_style(app.theme.border),
    );

    frame.render_widget(paragraph, area);
}

/// Render metric details popup
fn render_metric_details(frame: &mut Frame, app: &App, metric_id: u64) {
    let area = centered_rect(70, 60, frame.area());

    // Clear the background
    frame.render_widget(Clear, area);

    let content = if let Some(metric) = app
        .metrics
        .iter()
        .find(|m| m.id.map(|id| id.0) == Some(metric_id))
    {
        let id_str = metric
            .id
            .map(|id| id.0.to_string())
            .unwrap_or_else(|| "N/A".to_string());
        vec![
            Line::from(vec![
                Span::styled("Name: ", app.theme.normal),
                Span::styled(&metric.name, app.theme.metric_name),
            ]),
            Line::from(vec![
                Span::styled("ID: ", app.theme.normal),
                Span::styled(id_str, app.theme.value),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Location: ", app.theme.normal),
                Span::styled(
                    format!("{}:{}", metric.location.file, metric.location.line),
                    app.theme.value,
                ),
            ]),
            Line::from(vec![
                Span::styled("Expression: ", app.theme.normal),
                Span::styled(metric.expressions.join(", "), app.theme.value),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Mode: ", app.theme.normal),
                Span::styled(metric.mode.as_str(), app.theme.value),
            ]),
            Line::from(vec![
                Span::styled("Enabled: ", app.theme.normal),
                Span::styled(
                    if metric.enabled { "Yes" } else { "No" },
                    if metric.enabled {
                        app.theme.connected
                    } else {
                        app.theme.disconnected
                    },
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Connection: ", app.theme.normal),
                Span::styled(&metric.connection_id.0, app.theme.value),
            ]),
        ]
    } else {
        vec![Line::from(Span::styled(
            "Metric not found",
            app.theme.error,
        ))]
    };

    let paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .title(" Metric Details ")
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render event details popup
fn render_event_details(frame: &mut Frame, app: &App, idx: usize) {
    let area = centered_rect(80, 80, frame.area());

    // Clear the background
    frame.render_widget(Clear, area);

    let content = if let Some(event) = app.events.get(idx) {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Metric: ", app.theme.normal),
                Span::styled(&event.metric_name, app.theme.metric_name),
            ]),
            Line::from(vec![
                Span::styled("Metric ID: ", app.theme.normal),
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
            lines.push(Line::from(Span::styled("Value:", app.theme.header)));
            // Pretty print JSON
            let value_str = format_json_pretty(event.value_json());
            for line in value_str.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), app.theme.value)));
            }
        }

        // Thread info
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

        // Stack trace
        if let Some(ref stack) = event.stack_trace {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "── Stack Trace ──",
                app.theme.header,
            )));
            for frame in &stack.frames {
                let frame_text = format!(
                    "  {}: {} ({}:{})",
                    frame.index,
                    frame.name,
                    frame.file.as_deref().unwrap_or("?"),
                    frame.line.unwrap_or(0)
                );
                lines.push(Line::from(Span::styled(frame_text, app.theme.normal)));
            }
        }

        // Memory snapshot
        if let Some(ref snapshot) = event.memory_snapshot {
            if !snapshot.locals.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "── Local Variables ──",
                    app.theme.header,
                )));
                for var in &snapshot.locals {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {}", var.name), app.theme.metric_name),
                        Span::styled(" = ", app.theme.normal),
                        Span::styled(&var.value, app.theme.value),
                    ]));
                }
            }
        }

        lines
    } else {
        vec![Line::from(Span::styled("Event not found", app.theme.error))]
    };

    let paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .title(" Event Details ")
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render confirm close connection popup
fn render_confirm_close_popup(frame: &mut Frame, app: &App, conn_id: &str) {
    let area = centered_rect(50, 30, frame.area());

    // Clear the background
    frame.render_widget(Clear, area);

    // Find connection details for display
    let conn_info = app
        .connections
        .iter()
        .find(|c| c.id.0 == conn_id)
        .map(|c| format!("{}:{} ({})", c.host, c.port, c.language.display_name()))
        .unwrap_or_else(|| "Unknown".to_string());

    let content = vec![
        Line::from(vec![Span::styled("Close Connection?", app.theme.header)]),
        Line::from(""),
        Line::from(vec![
            Span::styled("ID: ", app.theme.normal),
            Span::styled(conn_id, app.theme.value),
        ]),
        Line::from(vec![
            Span::styled("Address: ", app.theme.normal),
            Span::styled(conn_info, app.theme.value),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "This will disconnect from the debugger.",
            app.theme.warning,
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("y/Enter", app.theme.value),
            Span::styled(" - Confirm   ", app.theme.normal),
            Span::styled("n/Esc", app.theme.value),
            Span::styled(" - Cancel", app.theme.normal),
        ]),
    ];

    let paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .title(" Confirm Close ")
                .borders(Borders::ALL)
                .border_style(app.theme.border),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}
