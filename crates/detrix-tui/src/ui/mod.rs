//! UI rendering module

mod connections_tab;
mod events_tab;
pub mod formatters;
mod metrics_tab;
mod popups;
mod system_tab;
pub mod theme;

use crate::app::{App, AppMode, Tab};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Tabs};

/// Render the entire UI
pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1), // Header
        Constraint::Length(1), // Tabs
        Constraint::Min(0),    // Content
        Constraint::Length(1), // Status bar
    ])
    .split(frame.area());

    render_header(frame, app, chunks[0]);
    render_tabs(frame, app, chunks[1]);

    match app.selected_tab {
        Tab::Events => events_tab::render(frame, app, chunks[2]),
        Tab::Metrics => metrics_tab::render(frame, app, chunks[2]),
        Tab::Connections => connections_tab::render(frame, app, chunks[2]),
        Tab::SystemLog => system_tab::render(frame, app, chunks[2]),
    }

    render_status_bar(frame, app, chunks[3]);

    // Render popup if active
    if let AppMode::Popup(popup) = app.mode.clone() {
        popups::render(frame, app, &popup);
    }
}

/// Render the header bar
fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::horizontal([
        Constraint::Min(20),    // Title + status
        Constraint::Length(20), // Uptime
        Constraint::Length(12), // Time
    ])
    .split(area);

    // Title with daemon status
    let status_icon = match app.daemon_status.status.as_str() {
        "active" => "●",
        "idle" => "○",
        _ => "◌",
    };
    let title = Paragraph::new(format!("Detrix Dashboard {}", status_icon)).style(app.theme.header);
    frame.render_widget(title, chunks[0]);

    // Uptime (from daemon status)
    let uptime_text = if !app.daemon_status.uptime_formatted.is_empty() {
        format!("↑{}", app.daemon_status.uptime_formatted)
    } else {
        String::new()
    };
    let uptime = Paragraph::new(uptime_text)
        .style(app.theme.normal)
        .alignment(Alignment::Right);
    frame.render_widget(uptime, chunks[1]);

    // Time
    let now = chrono::Local::now();
    let time = Paragraph::new(now.format("%H:%M:%S").to_string())
        .style(app.theme.timestamp)
        .alignment(Alignment::Right);
    frame.render_widget(time, chunks[2]);
}

/// Render the tab bar
fn render_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<&str> = Tab::all().iter().map(|t| t.name()).collect();

    let tabs = Tabs::new(titles)
        .select(app.selected_tab as usize)
        .style(app.theme.tab_inactive)
        .highlight_style(app.theme.tab_active)
        .divider(" | ");

    frame.render_widget(tabs, area);
}

/// Render the status bar
fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::horizontal([
        Constraint::Percentage(40), // Left: help hints
        Constraint::Percentage(35), // Middle: stats
        Constraint::Percentage(25), // Right: debug info
    ])
    .split(area);

    // Help hints
    let help = match app.selected_tab {
        Tab::Events => "q:Quit ←→:Tabs ↑↓:Nav Space:Pause c:Clear ?:Help",
        Tab::Metrics => "q:Quit ←→:Tabs ↑↓:Nav Enter:Details ?:Help",
        Tab::Connections => "q:Quit ←→:Tabs ↑↓:Nav ?:Help",
        Tab::SystemLog => "q:Quit ←→:Tabs ↑↓:Nav ?:Help",
    };
    let help_widget = Paragraph::new(help).style(app.theme.normal);
    frame.render_widget(help_widget, chunks[0]);

    // Stats
    let stats = match app.selected_tab {
        Tab::Events => {
            let pause_indicator = if app.streaming_paused {
                "⏸ Paused"
            } else {
                "● Streaming"
            };
            format!(
                "Events: {}  Rate: {:.0}/s  {}",
                app.total_events, app.events_per_second, pause_indicator
            )
        }
        Tab::Metrics => {
            let enabled = app.metrics.iter().filter(|m| m.enabled).count();
            format!("Total: {}  Enabled: {}", app.metrics.len(), enabled)
        }
        Tab::Connections => {
            let connected = app
                .connections
                .iter()
                .filter(|c| c.status == detrix_core::ConnectionStatus::Connected)
                .count();
            format!("Total: {}  Connected: {}", app.connections.len(), connected)
        }
        Tab::SystemLog => {
            format!("Events: {}", app.system_events.len())
        }
    };
    let stats_widget = Paragraph::new(stats).style(app.theme.normal);
    frame.render_widget(stats_widget, chunks[1]);

    // Debug info
    let grpc_status = if app.grpc_connected {
        "gRPC:●"
    } else {
        "gRPC:○"
    };
    let debug_info = if let Some(ref err) = app.last_error {
        format!("{} {}", grpc_status, err)
    } else {
        grpc_status.to_string()
    };
    let debug_style = if app.grpc_connected {
        app.theme.connected
    } else {
        app.theme.error
    };
    let debug_widget = Paragraph::new(debug_info)
        .style(debug_style)
        .alignment(Alignment::Right);
    frame.render_widget(debug_widget, chunks[2]);
}
