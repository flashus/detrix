//! JSON output formatting

use crate::grpc_client::{ConnectionInfo, EventInfo, GroupInfo, MetricInfo, StatusInfo};
use serde::Serialize;

/// Helper to print any serializable value as JSON
fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{}", json),
        Err(e) => eprintln!("Error serializing to JSON: {}", e),
    }
}

/// Print metrics as JSON array
pub fn print_metrics(metrics: &[MetricInfo]) {
    print_json(&metrics);
}

/// Print single metric as JSON
pub fn print_metric(metric: &MetricInfo) {
    print_json(&metric);
}

/// Print connections as JSON array
pub fn print_connections(connections: &[ConnectionInfo]) {
    print_json(&connections);
}

/// Print single connection as JSON
pub fn print_connection(connection: &ConnectionInfo) {
    print_json(&connection);
}

/// Print events as JSON array
pub fn print_events(events: &[EventInfo]) {
    print_json(&events);
}

/// Print groups as JSON array
pub fn print_groups(groups: &[GroupInfo]) {
    print_json(&groups);
}

/// Print status as JSON
pub fn print_status(status: &StatusInfo) {
    print_json(&status);
}
