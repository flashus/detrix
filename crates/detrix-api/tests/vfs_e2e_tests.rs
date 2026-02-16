//! VFS transparent file fetching E2E tests
//!
//! Tests the full file source chain integration:
//! - FileSourceChain orchestration with real sources
//! - MockVfs cache behavior
//! - ControlPlaneSource with wiremock
//! - BridgeSource with wiremock
//! - DiskSource fallback
//! - FileInspectionService reading fetched files

use detrix_api::file_sources::{BridgeSource, ControlPlaneSource, DiskSource};
use detrix_application::{
    FileInspectionRequest, FileInspectionService, FileSourceChain, FileSourceRef, VfsRef,
    VirtualFileSystem,
};
use detrix_core::{ConnectionIdentity, SourceLanguage};
use detrix_testing::MockVfs;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Python file content for inspection tests
const PYTHON_CONTENT: &str = r#"import os

def greet(name):
    message = f"Hello, {name}!"
    print(message)
    return message

if __name__ == "__main__":
    greet("world")
"#;

fn test_connection_with_cp(cp_url: Option<&str>) -> detrix_core::Connection {
    let identity = ConnectionIdentity::new(
        "test-app",
        SourceLanguage::Python,
        "/workspace",
        "test-host",
    );
    let mut conn =
        detrix_core::Connection::new_with_identity(identity, "127.0.0.1".into(), 5678).unwrap();
    conn.control_plane_url = cp_url.map(|s| s.to_string());
    conn
}

fn test_connection() -> detrix_core::Connection {
    test_connection_with_cp(None)
}

/// Test: observe/inspect on a file NOT in VFS → auto-fetched from control plane
#[tokio::test]
async fn test_observe_fetches_file_from_control_plane() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/detrix/files/read"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PYTHON_CONTENT))
        .expect(1)
        .mount(&mock_server)
        .await;

    let vfs = Arc::new(MockVfs::new());
    let cp_source = Arc::new(ControlPlaneSource::new(
        Duration::from_secs(5),
        10 * 1024 * 1024,
        None,
    )) as FileSourceRef;

    let chain = FileSourceChain::new(
        vfs.clone() as VfsRef,
        vec![cp_source],
        &["control_plane".to_string()],
    );

    let conn = test_connection_with_cp(Some(&mock_server.uri()));

    // File not in VFS yet
    assert!(!vfs.exists("/app/main.py").unwrap());

    // ensure_available fetches from control plane
    chain.ensure_available(&conn, "/app/main.py").await.unwrap();

    // File should now be in VFS
    assert!(vfs.exists("/app/main.py").unwrap());

    // FileInspectionService should be able to inspect it
    let inspection = FileInspectionService::new(vfs.clone() as VfsRef);
    let (lang, _result) = inspection
        .inspect(FileInspectionRequest {
            file_path: "/app/main.py".to_string(),
            line: Some(4),
            find_variable: None,
            workspace_root: None,
        })
        .unwrap();

    assert_eq!(lang, SourceLanguage::Python);
}

/// Test: pre-populated VFS cache → no HTTP call to control plane
#[tokio::test]
async fn test_observe_uses_vfs_cache() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/detrix/files/read"))
        .respond_with(ResponseTemplate::new(200).set_body_string("should not be called"))
        .expect(0) // Expect NO calls
        .mount(&mock_server)
        .await;

    let vfs = Arc::new(MockVfs::new());
    // Pre-populate VFS
    vfs.store("test-conn", "/app/cached.py", PYTHON_CONTENT.to_string());

    let cp_source = Arc::new(ControlPlaneSource::new(
        Duration::from_secs(5),
        10 * 1024 * 1024,
        None,
    )) as FileSourceRef;

    let chain = FileSourceChain::new(
        vfs.clone() as VfsRef,
        vec![cp_source],
        &["control_plane".to_string()],
    );

    let conn = test_connection_with_cp(Some(&mock_server.uri()));

    // ensure_available should be a cache hit
    chain
        .ensure_available(&conn, "/app/cached.py")
        .await
        .unwrap();

    // FileInspectionService should read from cache
    let inspection = FileInspectionService::new(vfs as VfsRef);
    let (lang, _result) = inspection
        .inspect(FileInspectionRequest {
            file_path: "/app/cached.py".to_string(),
            line: Some(3),
            find_variable: None,
            workspace_root: None,
        })
        .unwrap();

    assert_eq!(lang, SourceLanguage::Python);
    // wiremock's expect(0) will panic on drop if it was called
}

/// Test: inspect_file on a remote file → auto-fetched via chain
#[tokio::test]
async fn test_inspect_file_fetches_transparently() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/detrix/files/read"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PYTHON_CONTENT))
        .mount(&mock_server)
        .await;

    let vfs = Arc::new(MockVfs::new());
    let cp_source = Arc::new(ControlPlaneSource::new(
        Duration::from_secs(5),
        10 * 1024 * 1024,
        None,
    )) as FileSourceRef;

    let chain = FileSourceChain::new(
        vfs.clone() as VfsRef,
        vec![cp_source],
        &["control_plane".to_string()],
    );

    let conn = test_connection_with_cp(Some(&mock_server.uri()));

    // Fetch the file first
    chain
        .ensure_available(&conn, "/remote/service.py")
        .await
        .unwrap();

    // Then inspect — should work because file is now in VFS
    let inspection = FileInspectionService::new(vfs as VfsRef);
    let result = inspection.inspect(FileInspectionRequest {
        file_path: "/remote/service.py".to_string(),
        line: None,
        find_variable: Some("greet".to_string()),
        workspace_root: None,
    });

    assert!(result.is_ok(), "Inspect should succeed for fetched file");
}

/// Test: control plane returns 404 → falls through to DiskSource (temp file)
#[tokio::test]
async fn test_fallthrough_to_disk() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/detrix/files/read"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    // Create a real temp file for DiskSource to find
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("local.py");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        write!(f, "{}", PYTHON_CONTENT).unwrap();
    }
    let file_path_str = file_path.to_str().unwrap().to_string();

    let vfs = Arc::new(MockVfs::new());
    let cp_source = Arc::new(ControlPlaneSource::new(
        Duration::from_secs(5),
        10 * 1024 * 1024,
        None,
    )) as FileSourceRef;
    let disk_source = Arc::new(DiskSource) as FileSourceRef;

    let chain = FileSourceChain::new(
        vfs.clone() as VfsRef,
        vec![cp_source, disk_source],
        &["control_plane".to_string(), "disk".to_string()],
    );

    let conn = test_connection_with_cp(Some(&mock_server.uri()));

    // CP returns 404, DiskSource reads from temp file
    chain.ensure_available(&conn, &file_path_str).await.unwrap();

    // File should be in VFS now (fetched from disk)
    assert!(vfs.exists(&file_path_str).unwrap());
    let content = vfs.read_to_string(&file_path_str).unwrap();
    assert_eq!(content, PYTHON_CONTENT);
}

/// Test: bridge source serves file via set_bridge_url
#[tokio::test]
async fn test_bridge_source_via_header() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/detrix/files/read"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PYTHON_CONTENT))
        .expect(1)
        .mount(&mock_server)
        .await;

    let vfs = Arc::new(MockVfs::new());
    let bridge_source = Arc::new(BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024));
    bridge_source.set_bridge_url(Some(mock_server.uri()));

    let chain = FileSourceChain::new(
        vfs.clone() as VfsRef,
        vec![bridge_source as FileSourceRef],
        &["bridge".to_string()],
    );

    let conn = test_connection();

    chain
        .ensure_available(&conn, "/bridge/app.py")
        .await
        .unwrap();

    assert!(vfs.exists("/bridge/app.py").unwrap());
    assert_eq!(
        vfs.read_to_string("/bridge/app.py").unwrap(),
        PYTHON_CONTENT
    );
}

/// Test: disk has higher priority → checked first, no HTTP call
#[tokio::test]
async fn test_priority_ordering_e2e() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/detrix/files/read"))
        .respond_with(ResponseTemplate::new(200).set_body_string("from control plane"))
        .expect(0) // Should NOT be called — disk finds it first
        .mount(&mock_server)
        .await;

    // Create a real temp file for DiskSource
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("priority.py");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        write!(f, "{}", PYTHON_CONTENT).unwrap();
    }
    let file_path_str = file_path.to_str().unwrap().to_string();

    let vfs = Arc::new(MockVfs::new());
    let disk_source = Arc::new(DiskSource) as FileSourceRef;
    let cp_source = Arc::new(ControlPlaneSource::new(
        Duration::from_secs(5),
        10 * 1024 * 1024,
        None,
    )) as FileSourceRef;

    // Priority: disk first, then control_plane
    let chain = FileSourceChain::new(
        vfs.clone() as VfsRef,
        vec![disk_source, cp_source],
        &["disk".to_string(), "control_plane".to_string()],
    );

    let conn = test_connection_with_cp(Some(&mock_server.uri()));

    chain.ensure_available(&conn, &file_path_str).await.unwrap();

    // File should be in VFS, fetched from disk
    assert!(vfs.exists(&file_path_str).unwrap());
    assert_eq!(vfs.read_to_string(&file_path_str).unwrap(), PYTHON_CONTENT);
    // wiremock's expect(0) will verify no HTTP call was made
}
