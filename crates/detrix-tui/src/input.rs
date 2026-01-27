//! Keyboard input handling

use crate::app::{App, AppMode, PopupType, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Handle a key press event
pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Global keys (work in any mode)
    match key.code {
        KeyCode::Char('q') if key.modifiers.is_empty() => {
            if matches!(app.mode, AppMode::Popup(_)) {
                app.mode = AppMode::Running;
            } else {
                app.mode = AppMode::Quitting;
            }
            return;
        }
        KeyCode::Esc => {
            if matches!(app.mode, AppMode::Popup(_)) {
                app.mode = AppMode::Running;
            } else {
                app.mode = AppMode::Quitting;
            }
            return;
        }
        KeyCode::Char('?') => {
            app.mode = AppMode::Popup(PopupType::Help);
            return;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.mode = AppMode::Quitting;
            return;
        }
        _ => {}
    }

    // If popup is open, handle popup-specific keys
    if let AppMode::Popup(ref popup) = app.mode {
        handle_popup_key(app, key, popup.clone());
        return;
    }

    // Tab switching (global when not in popup)
    match key.code {
        KeyCode::F(1) => {
            app.selected_tab = Tab::Events;
            return;
        }
        KeyCode::F(2) => {
            app.selected_tab = Tab::Metrics;
            return;
        }
        KeyCode::F(3) => {
            app.selected_tab = Tab::Connections;
            return;
        }
        KeyCode::F(4) => {
            app.selected_tab = Tab::SystemLog;
            return;
        }
        KeyCode::Tab => {
            app.next_tab();
            return;
        }
        KeyCode::BackTab => {
            app.prev_tab();
            return;
        }
        _ => {}
    }

    // Tab-specific keys
    match app.selected_tab {
        Tab::Events => handle_events_key(app, key),
        Tab::Metrics => handle_metrics_key(app, key),
        Tab::Connections => handle_connections_key(app, key),
        Tab::SystemLog => handle_system_key(app, key),
    }
}

/// Handle key in Events tab
fn handle_events_key(app: &mut App, key: KeyEvent) {
    // Tab switching works even when list is empty
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => {
            app.prev_tab();
            return;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.next_tab();
            return;
        }
        // Actions that work without items
        KeyCode::Char(' ') => {
            app.streaming_paused = !app.streaming_paused;
            return;
        }
        KeyCode::Char('c') => {
            app.clear_events();
            return;
        }
        KeyCode::Char('s') => {
            app.mode = AppMode::Popup(PopupType::ConnectionSwitcher);
            return;
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::Popup(PopupType::Filter);
            return;
        }
        _ => {}
    }

    let len = app.events.len();
    if len == 0 {
        return;
    }

    match key.code {
        // Navigation (vim + arrows)
        KeyCode::Up | KeyCode::Char('k') => {
            let i = app.events_state.selected().unwrap_or(0);
            app.events_state.select(Some(i.saturating_sub(1)));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = app.events_state.selected().unwrap_or(0);
            app.events_state.select(Some((i + 1).min(len - 1)));
        }
        KeyCode::PageUp | KeyCode::Char('K') => {
            let i = app.events_state.selected().unwrap_or(0);
            app.events_state.select(Some(i.saturating_sub(10)));
        }
        KeyCode::PageDown | KeyCode::Char('J') => {
            let i = app.events_state.selected().unwrap_or(0);
            app.events_state.select(Some((i + 10).min(len - 1)));
        }
        KeyCode::Home | KeyCode::Char('g') => {
            app.events_state.select(Some(0));
        }
        KeyCode::End | KeyCode::Char('G') => {
            app.events_state.select(Some(len - 1));
        }
        KeyCode::Enter => {
            // Toggle pin: if cursor is on pinned event, unpin; otherwise pin cursor
            if app.is_selected_pinned() {
                app.unpin_event();
            } else {
                app.pin_selected_event();
            }
        }
        _ => {}
    }
}

/// Handle key in Metrics tab
fn handle_metrics_key(app: &mut App, key: KeyEvent) {
    // Tab switching works even when list is empty
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => {
            app.prev_tab();
            return;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.next_tab();
            return;
        }
        _ => {}
    }

    let len = app.metrics.len();
    if len == 0 {
        return;
    }

    match key.code {
        // Navigation
        KeyCode::Up | KeyCode::Char('k') => {
            let i = app.metrics_state.selected().unwrap_or(0);
            app.metrics_state.select(Some(i.saturating_sub(1)));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = app.metrics_state.selected().unwrap_or(0);
            app.metrics_state.select(Some((i + 1).min(len - 1)));
        }
        KeyCode::PageUp | KeyCode::Char('K') => {
            let i = app.metrics_state.selected().unwrap_or(0);
            app.metrics_state.select(Some(i.saturating_sub(10)));
        }
        KeyCode::PageDown | KeyCode::Char('J') => {
            let i = app.metrics_state.selected().unwrap_or(0);
            app.metrics_state.select(Some((i + 10).min(len - 1)));
        }
        // Actions
        KeyCode::Enter => {
            if let Some(metric) = app.selected_metric() {
                if let Some(id) = metric.id {
                    app.mode = AppMode::Popup(PopupType::MetricDetails(id.0));
                }
            }
        }
        _ => {}
    }
}

/// Handle key in Connections tab
fn handle_connections_key(app: &mut App, key: KeyEvent) {
    // Tab switching works even when list is empty
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => {
            app.prev_tab();
            return;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.next_tab();
            return;
        }
        _ => {}
    }

    let len = app.connections.len();
    if len == 0 {
        return;
    }

    match key.code {
        // Navigation
        KeyCode::Up | KeyCode::Char('k') => {
            let i = app.connections_state.selected().unwrap_or(0);
            app.connections_state.select(Some(i.saturating_sub(1)));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = app.connections_state.selected().unwrap_or(0);
            app.connections_state.select(Some((i + 1).min(len - 1)));
        }
        // Close connection
        KeyCode::Char('d') => {
            if let Some(conn_id) = app.get_selected_connection_id() {
                app.mode = AppMode::Popup(PopupType::ConfirmCloseConnection(conn_id));
            }
        }
        _ => {}
    }
}

/// Handle key in System Log tab
fn handle_system_key(app: &mut App, key: KeyEvent) {
    // Tab switching works even when list is empty
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => {
            app.prev_tab();
            return;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.next_tab();
            return;
        }
        _ => {}
    }

    let len = app.system_events.len();
    if len == 0 {
        return;
    }

    match key.code {
        // Navigation
        KeyCode::Up | KeyCode::Char('k') => {
            let i = app.system_state.selected().unwrap_or(0);
            app.system_state.select(Some(i.saturating_sub(1)));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let i = app.system_state.selected().unwrap_or(0);
            app.system_state.select(Some((i + 1).min(len - 1)));
        }
        _ => {}
    }
}

/// Handle key when a popup is open
fn handle_popup_key(app: &mut App, key: KeyEvent, popup: PopupType) {
    match popup {
        PopupType::Help => {
            // Any key closes help
            app.mode = AppMode::Running;
        }
        PopupType::ConnectionSwitcher => {
            // Total items: "All Connections" (index 0) + actual connections
            let total = app.connections.len() + 1;
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    // Navigate up in connection list
                    let current = app.connection_switcher_state.selected().unwrap_or(0);
                    let new = if current == 0 {
                        total.saturating_sub(1)
                    } else {
                        current - 1
                    };
                    app.connection_switcher_state.select(Some(new));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    // Navigate down in connection list
                    let current = app.connection_switcher_state.selected().unwrap_or(0);
                    let new = if total == 0 { 0 } else { (current + 1) % total };
                    app.connection_switcher_state.select(Some(new));
                }
                KeyCode::Enter => {
                    // Select connection: index 0 = "All", index 1+ = actual connections
                    if let Some(idx) = app.connection_switcher_state.selected() {
                        app.selected_connection = if idx == 0 {
                            None // "All Connections"
                        } else {
                            app.connections.get(idx - 1).map(|c| c.id.clone())
                        };
                    }
                    app.mode = AppMode::Running;
                }
                KeyCode::Esc => {
                    // Close without selecting
                    app.mode = AppMode::Running;
                }
                _ => {}
            }
        }
        PopupType::Filter => match key.code {
            KeyCode::Char(c) => {
                app.filter_text.push(c);
            }
            KeyCode::Backspace => {
                app.filter_text.pop();
            }
            KeyCode::Enter => {
                app.mode = AppMode::Running;
            }
            _ => {}
        },
        PopupType::EventDetails(_) | PopupType::MetricDetails(_) => {
            // Any key closes details
            app.mode = AppMode::Running;
        }
        PopupType::ConfirmCloseConnection(ref conn_id) => match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                // Close the connection
                let conn_id_owned = conn_id.clone();
                app.close_connection_async(conn_id_owned);
                app.mode = AppMode::Running;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                app.mode = AppMode::Running;
            }
            _ => {}
        },
    }
}
