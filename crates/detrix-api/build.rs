use std::io::Result;

fn main() -> Result<()> {
    // Configure tonic-build
    //
    // NOTE: Proto is the single source of truth for DTOs across all APIs:
    // - gRPC: Uses proto types directly
    // - REST: Serializes proto types via serde
    // - MCP: Converts to/from proto types
    // - WebSocket: Serializes proto types via serde
    //
    // When adding new fields, update the proto definition and regenerate.
    // The serde derive ensures JSON compatibility for REST/MCP/WebSocket.
    tonic_prost_build::configure()
        // Generate server code (we're implementing the server)
        .build_server(true)
        // Generate client code (for E2E tests)
        .build_client(true)
        // Output to src/generated directory
        .out_dir("src/generated")
        // Enable serde derive for JSON compatibility (REST/MCP/WebSocket)
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .type_attribute(".", "#[serde(rename_all = \"camelCase\")]")
        // Compile all proto files
        .compile_protos(
            &[
                "proto/common.proto",
                "proto/metrics.proto",
                "proto/streaming.proto",
                "proto/connections.proto",
            ],
            &["proto"],
        )?;

    // Tell Cargo to rerun this build script if proto files change
    println!("cargo:rerun-if-changed=proto/common.proto");
    println!("cargo:rerun-if-changed=proto/metrics.proto");
    println!("cargo:rerun-if-changed=proto/streaming.proto");
    println!("cargo:rerun-if-changed=proto/connections.proto");

    Ok(())
}
