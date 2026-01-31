//! Token discovery and validation.

use std::env;
use std::fs;
use std::net::IpAddr;
use std::path::Path;

use subtle::ConstantTimeEq;

/// Discover the authentication token.
///
/// Priority:
/// 1. DETRIX_TOKEN environment variable
/// 2. ~/detrix/mcp-token file
/// 3. {detrix_home}/mcp-token file (if provided)
pub fn discover_token(detrix_home: Option<&Path>) -> Option<String> {
    // 1. Check environment variable
    if let Ok(token) = env::var("DETRIX_TOKEN") {
        if !token.is_empty() {
            return Some(token);
        }
    }

    // 2. Try ~/detrix/mcp-token
    if let Some(home) = dirs::home_dir() {
        let token_path = home.join("detrix").join("mcp-token");
        if let Ok(token) = fs::read_to_string(&token_path) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }

    // 3. Try custom detrix home
    if let Some(home) = detrix_home {
        let token_path = home.join("mcp-token");
        if let Ok(token) = fs::read_to_string(&token_path) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }

    None
}

/// Check if an address is localhost.
///
/// Returns true for:
/// - 127.0.0.1
/// - ::1
/// - localhost (when resolved)
pub fn is_localhost(addr: &str) -> bool {
    // Handle IPv6 with brackets first (e.g., "[::1]:8080")
    if addr.starts_with('[') {
        if let Some(bracket_end) = addr.find(']') {
            let host = &addr[1..bracket_end];
            return check_localhost_host(host);
        }
    }

    // Handle host:port format for IPv4 only
    // For bare IPv6 like "::1", we can't use rfind(':') as it would split the address
    if let Some(idx) = addr.rfind(':') {
        // Check if this looks like IPv4 with port (no other colons before this one)
        let potential_host = &addr[..idx];
        if !potential_host.contains(':') {
            return check_localhost_host(potential_host);
        }
    }

    // No port suffix, check the whole address
    check_localhost_host(addr)
}

/// Check if a host string is localhost.
fn check_localhost_host(host: &str) -> bool {
    let host_lower = host.to_lowercase();
    if host_lower == "localhost" || host_lower == "127.0.0.1" || host_lower == "::1" {
        return true;
    }

    // Try to parse as IP and check if loopback
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip.is_loopback();
    }

    false
}

/// Check if a request is authorized.
///
/// Localhost requests are always authorized.
/// Remote requests require a valid Bearer token.
pub fn is_authorized(
    remote_addr: &str,
    auth_header: Option<&str>,
    valid_token: Option<&str>,
) -> bool {
    // Localhost bypass
    if is_localhost(remote_addr) {
        return true;
    }

    // No token configured = deny remote requests
    let valid_token = match valid_token {
        Some(t) if !t.is_empty() => t,
        _ => return false,
    };

    // Check Authorization header
    let auth_header = match auth_header {
        Some(h) => h,
        None => return false,
    };

    // Parse Bearer token
    const PREFIX: &str = "Bearer ";
    if !auth_header.starts_with(PREFIX) {
        return false;
    }

    let token = &auth_header[PREFIX.len()..];

    // Constant-time comparison to prevent timing attacks
    token.as_bytes().ct_eq(valid_token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_localhost_ipv4() {
        assert!(is_localhost("127.0.0.1"));
        assert!(is_localhost("127.0.0.1:8080"));
        assert!(!is_localhost("192.168.1.1"));
        assert!(!is_localhost("192.168.1.1:8080"));
    }

    #[test]
    fn test_is_localhost_ipv6() {
        assert!(is_localhost("::1"));
        assert!(is_localhost("[::1]:8080"));
        assert!(!is_localhost("::2"));
    }

    #[test]
    fn test_is_localhost_name() {
        assert!(is_localhost("localhost"));
        assert!(is_localhost("localhost:8080"));
        assert!(is_localhost("LOCALHOST"));
        assert!(!is_localhost("example.com"));
    }

    #[test]
    fn test_is_authorized_localhost() {
        // Localhost always authorized, even without token
        assert!(is_authorized("127.0.0.1:12345", None, None));
        assert!(is_authorized("127.0.0.1:12345", None, Some("secret")));
        assert!(is_authorized("::1", None, None));
        assert!(is_authorized("localhost:8080", None, None));
    }

    #[test]
    fn test_is_authorized_remote_no_token() {
        // Remote without token = denied
        assert!(!is_authorized("192.168.1.1:12345", None, None));
        assert!(!is_authorized("192.168.1.1:12345", None, Some("")));
    }

    #[test]
    fn test_is_authorized_remote_with_token() {
        let token = "secret-token";

        // Valid token
        assert!(is_authorized(
            "192.168.1.1:12345",
            Some("Bearer secret-token"),
            Some(token)
        ));

        // Invalid token
        assert!(!is_authorized(
            "192.168.1.1:12345",
            Some("Bearer wrong-token"),
            Some(token)
        ));

        // Missing Bearer prefix
        assert!(!is_authorized(
            "192.168.1.1:12345",
            Some("secret-token"),
            Some(token)
        ));

        // Missing header
        assert!(!is_authorized("192.168.1.1:12345", None, Some(token)));
    }
}
