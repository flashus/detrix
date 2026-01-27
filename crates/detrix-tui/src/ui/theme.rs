//! Theme system for TUI styling

use detrix_config::{ThemeColors, ThemeConfig};
use ratatui::style::{Color, Modifier, Style};

/// Application theme containing all styles
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,

    // General
    pub header: Style,
    pub normal: Style,
    pub selected: Style,
    pub border: Style,

    // Tabs
    pub tab_active: Style,
    pub tab_inactive: Style,

    // Status indicators
    pub connected: Style,
    pub connecting: Style,
    pub disconnected: Style,
    pub error: Style,
    pub warning: Style,

    // Data display
    pub metric_name: Style,
    pub value: Style,
    pub timestamp: Style,
}

impl Theme {
    /// Create a dark theme (default)
    pub fn dark() -> Self {
        Self {
            name: "dark".to_string(),

            header: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            normal: Style::default().fg(Color::White),
            selected: Style::default()
                .bg(Color::Rgb(60, 60, 80))
                .add_modifier(Modifier::BOLD),
            border: Style::default().fg(Color::Gray),

            tab_active: Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            tab_inactive: Style::default().fg(Color::Gray),

            connected: Style::default().fg(Color::LightGreen),
            connecting: Style::default().fg(Color::Yellow),
            disconnected: Style::default().fg(Color::Gray),
            error: Style::default().fg(Color::LightRed),
            warning: Style::default().fg(Color::Yellow),

            metric_name: Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
            value: Style::default().fg(Color::Cyan),
            timestamp: Style::default().fg(Color::Gray),
        }
    }

    /// Create a light theme
    pub fn light() -> Self {
        Self {
            name: "light".to_string(),

            header: Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            normal: Style::default().fg(Color::Black),
            selected: Style::default()
                .bg(Color::Rgb(200, 200, 220))
                .add_modifier(Modifier::BOLD),
            border: Style::default().fg(Color::Gray),

            tab_active: Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            tab_inactive: Style::default().fg(Color::Gray),

            connected: Style::default().fg(Color::Green),
            connecting: Style::default().fg(Color::Yellow),
            disconnected: Style::default().fg(Color::Gray),
            error: Style::default().fg(Color::Red),
            warning: Style::default().fg(Color::Rgb(200, 150, 0)),

            metric_name: Style::default()
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            value: Style::default().fg(Color::Blue),
            timestamp: Style::default().fg(Color::Gray),
        }
    }

    /// Create a theme from configuration
    pub fn from_config(config: &ThemeConfig) -> Self {
        let base = match config.name.as_str() {
            "light" => Self::light(),
            _ => Self::dark(),
        };

        // Apply color overrides if present
        if let Some(ref colors) = config.colors {
            base.with_overrides(colors)
        } else {
            base
        }
    }

    /// Apply color overrides
    fn with_overrides(mut self, colors: &ThemeColors) -> Self {
        if let Some(ref c) = colors.header {
            if let Some(color) = parse_color(c) {
                self.header = self.header.fg(color);
            }
        }
        if let Some(ref c) = colors.foreground {
            if let Some(color) = parse_color(c) {
                self.normal = self.normal.fg(color);
            }
        }
        if let Some(ref c) = colors.selection {
            if let Some(color) = parse_color(c) {
                self.selected = self.selected.bg(color);
            }
        }
        if let Some(ref c) = colors.border {
            if let Some(color) = parse_color(c) {
                self.border = self.border.fg(color);
            }
        }
        if let Some(ref c) = colors.connected {
            if let Some(color) = parse_color(c) {
                self.connected = self.connected.fg(color);
            }
        }
        if let Some(ref c) = colors.disconnected {
            if let Some(color) = parse_color(c) {
                self.disconnected = self.disconnected.fg(color);
            }
        }
        if let Some(ref c) = colors.error {
            if let Some(color) = parse_color(c) {
                self.error = self.error.fg(color);
            }
        }
        if let Some(ref c) = colors.warning {
            if let Some(color) = parse_color(c) {
                self.warning = self.warning.fg(color);
            }
        }
        if let Some(ref c) = colors.metric_name {
            if let Some(color) = parse_color(c) {
                self.metric_name = self.metric_name.fg(color);
            }
        }
        if let Some(ref c) = colors.value {
            if let Some(color) = parse_color(c) {
                self.value = self.value.fg(color);
            }
        }
        if let Some(ref c) = colors.timestamp {
            if let Some(color) = parse_color(c) {
                self.timestamp = self.timestamp.fg(color);
            }
        }

        self
    }
}

/// Parse a color string into a ratatui Color
fn parse_color(s: &str) -> Option<Color> {
    // Handle hex colors
    if s.starts_with('#') && s.len() == 7 {
        let r = u8::from_str_radix(&s[1..3], 16).ok()?;
        let g = u8::from_str_radix(&s[3..5], 16).ok()?;
        let b = u8::from_str_radix(&s[5..7], 16).ok()?;
        return Some(Color::Rgb(r, g, b));
    }

    // Handle named colors
    match s.to_lowercase().as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "lightred" => Some(Color::LightRed),
        "lightgreen" => Some(Color::LightGreen),
        "lightyellow" => Some(Color::LightYellow),
        "lightblue" => Some(Color::LightBlue),
        "lightmagenta" => Some(Color::LightMagenta),
        "lightcyan" => Some(Color::LightCyan),
        _ => None,
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_color() {
        assert_eq!(parse_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_color("#00ff00"), Some(Color::Rgb(0, 255, 0)));
        assert_eq!(parse_color("#0000ff"), Some(Color::Rgb(0, 0, 255)));
        assert_eq!(parse_color("#1a1b26"), Some(Color::Rgb(26, 27, 38)));
    }

    #[test]
    fn test_parse_named_color() {
        assert_eq!(parse_color("red"), Some(Color::Red));
        assert_eq!(parse_color("Blue"), Some(Color::Blue));
        assert_eq!(parse_color("CYAN"), Some(Color::Cyan));
    }

    #[test]
    fn test_parse_invalid_color() {
        assert_eq!(parse_color("invalid"), None);
        assert_eq!(parse_color("#fff"), None); // Too short
        assert_eq!(parse_color("#gggggg"), None); // Invalid hex
    }
}
