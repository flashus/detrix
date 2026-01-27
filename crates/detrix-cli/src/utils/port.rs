//! Port availability checking and automatic port selection
//!
//! Re-exports from `detrix_config::ports` for backward compatibility.
//! All port-related functionality is now centralized in the config crate.

// Re-export port utilities (used by tests and potentially external consumers)
#[allow(unused_imports)]
pub use detrix_config::{find_available_port, find_available_port_default, is_port_available};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_port_available() {
        // Port 0 should always work (OS assigns available port)
        assert!(is_port_available(0));

        // Very high ports should usually be available
        assert!(is_port_available(60000));
    }

    #[test]
    fn test_find_available_port() {
        // Should find an available port in a large range
        let port = find_available_port(50000, 50100);
        assert!(port.is_some());

        // Verify the returned port is in the expected range
        if let Some(p) = port {
            assert!(p >= 50000);
            assert!(p <= 50100);
        }
    }

    #[test]
    fn test_find_available_port_default() {
        let port = find_available_port_default(50000);
        assert!(port.is_some());

        if let Some(p) = port {
            assert!(p >= 50000);
            assert!(p <= 50100);
        }
    }
}
