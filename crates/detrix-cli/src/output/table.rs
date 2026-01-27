//! Table formatting using comfy-table

use crate::grpc_client::{
    format_uptime, ConnectionInfo, EventInfo, GroupInfo, MetricInfo, StatusInfo,
};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Color, ContentArrangement, Table};
use detrix_core::format_timestamp_full;

// Re-export set_use_utc from detrix_core for backward compatibility
pub use detrix_core::set_use_utc;

/// Create a styled table
fn create_table(headers: &[&str], no_color: bool) -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    if no_color {
        table.set_header(headers);
    } else {
        let header_cells: Vec<Cell> = headers
            .iter()
            .map(|h| Cell::new(h).fg(Color::Cyan))
            .collect();
        table.set_header(header_cells);
    }

    table
}

// ============================================================================
// Metrics
// ============================================================================

pub fn print_metrics(metrics: &[MetricInfo], no_color: bool) {
    if metrics.is_empty() {
        println!("No metrics configured");
        return;
    }

    let mut table = create_table(
        &["NAME", "LOCATION", "EXPRESSION", "GROUP", "ENABLED", "HITS"],
        no_color,
    );

    for m in metrics {
        let location = format!("{}:{}", m.location_file, m.location_line);
        let enabled = if m.enabled { "✓" } else { "✗" };
        let group = m.group.as_deref().unwrap_or("-");
        let hits = m.hit_count.to_string();

        table.add_row(vec![
            &m.name,
            &location,
            &m.expression,
            group,
            enabled,
            &hits,
        ]);
    }

    println!("{table}");
}

pub fn print_metrics_toon(metrics: &[MetricInfo]) {
    if metrics.is_empty() {
        println!("No metrics.");
        return;
    }

    println!("Metrics ({}):", metrics.len());
    for m in metrics {
        let status = if m.enabled { "enabled" } else { "disabled" };
        let group = m
            .group
            .as_ref()
            .map(|g| format!(" [{}]", g))
            .unwrap_or_default();
        println!(
            "  {} @ {}:{} → {} ({}, {} hits){}",
            m.name, m.location_file, m.location_line, m.expression, status, m.hit_count, group
        );
    }
}

pub fn print_metric_detail(metric: &MetricInfo, _no_color: bool) {
    println!("Name:       {}", metric.name);
    println!(
        "Location:   {}:{}",
        metric.location_file, metric.location_line
    );
    println!("Expression: {}", metric.expression);
    println!("Language:   {}", metric.language);
    println!("Enabled:    {}", if metric.enabled { "yes" } else { "no" });
    println!("Mode:       {}", metric.mode);
    if let Some(group) = &metric.group {
        println!("Group:      {}", group);
    }
    println!("Hit Count:  {}", metric.hit_count);
    if let Some(last_hit) = metric.last_hit_at {
        println!("Last Hit:   {}", format_timestamp_full(last_hit));
    }
}

// ============================================================================
// Connections
// ============================================================================

pub fn print_connections(connections: &[ConnectionInfo], no_color: bool) {
    if connections.is_empty() {
        println!("No connections");
        return;
    }

    let mut table = create_table(
        &["ID", "HOST", "PORT", "LANGUAGE", "STATUS", "CREATED"],
        no_color,
    );

    for c in connections {
        let created = format_timestamp_full(c.created_at);

        table.add_row(vec![
            &c.connection_id,
            &c.host,
            &c.port.to_string(),
            &c.language,
            &c.status,
            &created,
        ]);
    }

    println!("{table}");
}

pub fn print_connections_toon(connections: &[ConnectionInfo]) {
    if connections.is_empty() {
        println!("No connections.");
        return;
    }

    println!("Connections ({}):", connections.len());
    for c in connections {
        println!(
            "  {} → {}:{} ({}) [{}]",
            c.connection_id, c.host, c.port, c.language, c.status
        );
    }
}

pub fn print_connection_detail(conn: &ConnectionInfo, _no_color: bool) {
    println!("ID:         {}", conn.connection_id);
    println!("Host:       {}", conn.host);
    println!("Port:       {}", conn.port);
    println!("Language:   {}", conn.language);
    println!("Status:     {}", conn.status);
    println!("Created:    {}", format_timestamp_full(conn.created_at));
    if let Some(connected_at) = conn.connected_at {
        println!("Connected:  {}", format_timestamp_full(connected_at));
    }
    if let Some(last_active) = conn.last_active_at {
        println!("Last Active:{}", format_timestamp_full(last_active));
    }
}

// ============================================================================
// Events
// ============================================================================

pub fn print_events(events: &[EventInfo], no_color: bool) {
    if events.is_empty() {
        println!("No events");
        return;
    }

    let mut table = create_table(&["METRIC", "TIMESTAMP", "VALUE", "THREAD"], no_color);

    for e in events {
        let timestamp = format_timestamp_full(e.timestamp);
        let value = e.value_json.as_deref().unwrap_or("-");

        // Store thread string in a variable to extend its lifetime (avoids .leak())
        let thread_id_str: String;
        let thread = match &e.thread_name {
            Some(name) => name.as_str(),
            None => {
                thread_id_str = e
                    .thread_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".to_string());
                &thread_id_str
            }
        };

        table.add_row(vec![&e.metric_name, &timestamp, value, thread]);
    }

    println!("{table}");
}

pub fn print_events_toon(events: &[EventInfo]) {
    if events.is_empty() {
        println!("No events.");
        return;
    }

    println!("Events ({}):", events.len());
    for e in events {
        let timestamp = format_timestamp_full(e.timestamp);
        let value = e.value_json.as_deref().unwrap_or("null");
        println!("  [{}] {} = {}", timestamp, e.metric_name, value);
    }
}

// ============================================================================
// Groups
// ============================================================================

pub fn print_groups(groups: &[GroupInfo], no_color: bool) {
    if groups.is_empty() {
        println!("No groups");
        return;
    }

    let mut table = create_table(&["NAME", "METRICS", "ENABLED"], no_color);

    for g in groups {
        table.add_row(vec![
            &g.name,
            &g.metric_count.to_string(),
            &g.enabled_count.to_string(),
        ]);
    }

    println!("{table}");
}

pub fn print_groups_toon(groups: &[GroupInfo]) {
    if groups.is_empty() {
        println!("No groups.");
        return;
    }

    println!("Groups ({}):", groups.len());
    for g in groups {
        println!(
            "  {} ({} metrics, {} enabled)",
            g.name, g.metric_count, g.enabled_count
        );
    }
}

// ============================================================================
// Status
// ============================================================================

pub fn print_status(status: &StatusInfo, no_color: bool) {
    println!("Detrix Status");
    println!("─────────────────────────────────────");
    println!("Mode:               {}", status.mode);

    // Show config path if available
    if !status.config_path.is_empty() {
        println!("Config:             {}", status.config_path);
    }

    // Display uptime with formatted version if available
    let uptime = if !status.uptime_formatted.is_empty() {
        status.uptime_formatted.clone()
    } else {
        format_uptime(status.uptime_seconds)
    };
    println!("Uptime:             {}", uptime);

    // Show started datetime if available
    if !status.started_at.is_empty() {
        println!("Started:            {}", status.started_at);
    }

    // Show connections: debugger + MCP
    let mcp_count = status.mcp_clients.len();
    println!(
        "Connections:        {} debugger, {} MCP",
        status.active_connections, mcp_count
    );
    println!("Total Metrics:      {}", status.total_metrics);
    println!("Enabled Metrics:    {}", status.enabled_metrics);
    println!("Total Events:       {}", status.total_events);

    // Show daemon info if available
    if let Some(daemon) = &status.daemon {
        println!();
        println!("Daemon Process");
        println!("─────────────────────────────────────");
        println!("PID:                {}", daemon.pid);
        println!("Parent PID:         {}", daemon.ppid);
    }

    // Show MCP clients if any
    if !status.mcp_clients.is_empty() {
        println!();
        println!("MCP Clients ({})", status.mcp_clients.len());
        println!("─────────────────────────────────────");

        let mut table = create_table(&["PARENT", "DURATION", "LAST ACTIVITY", "ID"], no_color);

        for client in &status.mcp_clients {
            let duration = format_uptime(client.connected_duration_secs);
            let last_activity = if client.last_activity_ago_secs == 0 {
                "just now".to_string()
            } else if client.last_activity_ago_secs < 60 {
                format!("{}s ago", client.last_activity_ago_secs)
            } else {
                format!("{}m ago", client.last_activity_ago_secs / 60)
            };

            // Format parent process: "windsurf->detrix(bridge_pid)" or "unknown"
            let parent = if let Some(ref pp) = client.parent_process {
                format!("{}->detrix({})", pp.name, pp.bridge_pid)
            } else {
                "unknown".to_string()
            };

            table.add_row(vec![&parent, &duration, &last_activity, &client.id]);
        }

        println!("{table}");
    }
}
