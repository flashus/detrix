//! Persistent machine-scoped client identity for CLI/TUI.
//!
//! Stored in `~/detrix/machine_id`. Created on first use.
//! Machine identity is used for audit/traceability ("who ran this").
//! MCP bridges use per-session UUIDs instead.

/// Get or create a persistent machine-scoped client ID.
///
/// Reads from `~/detrix/machine_id` if it exists, otherwise generates a new UUID v4
/// and persists it. The value is cached in memory after first resolution.
///
/// Benign race on first-time concurrent creation (last-writer-wins, then stable).
pub fn ensure_machine_id() -> String {
    use std::sync::OnceLock;
    static MACHINE_ID: OnceLock<String> = OnceLock::new();

    MACHINE_ID
        .get_or_init(|| {
            let path = crate::paths::detrix_home().join("machine_id");
            if let Ok(id) = std::fs::read_to_string(&path) {
                let id = id.trim().to_string();
                if !id.is_empty() {
                    return id;
                }
            }
            let id = uuid::Uuid::new_v4().to_string();
            let home = crate::paths::detrix_home();
            if let Err(e) = std::fs::create_dir_all(&home) {
                tracing::warn!("Failed to create detrix home dir {home:?}: {e}");
            }
            if let Err(e) = std::fs::write(&path, &id) {
                tracing::warn!("Failed to persist machine ID to {path:?}: {e} — a new ID will be generated on next restart");
            }
            id
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_machine_id_generates_valid_uuid() {
        let id = ensure_machine_id();
        assert!(!id.is_empty(), "machine_id should not be empty");
        // Should be parseable as UUID
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "machine_id should be valid UUID, got: {}",
            id
        );
    }

    #[test]
    fn test_ensure_machine_id_is_stable() {
        let id1 = ensure_machine_id();
        let id2 = ensure_machine_id();
        assert_eq!(id1, id2, "machine_id should be stable across calls");
    }

    #[test]
    fn test_ensure_machine_id_persists_to_file() {
        let path = crate::paths::detrix_home().join("machine_id");
        let id = ensure_machine_id();
        if path.exists() {
            let file_content = std::fs::read_to_string(&path).unwrap();
            assert_eq!(file_content.trim(), id);
        }
    }
}
