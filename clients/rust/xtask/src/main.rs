//! OpenAPI type generator for Detrix Rust client.
//!
//! This tool generates Rust types from the OpenAPI specification using the `typify` crate.
//!
//! Usage:
//!   cargo run -p xtask -- generate
//!
//! Or via Taskfile:
//!   cd clients && task generate-rust-types

use std::fs;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: xtask <command>");
        eprintln!("Commands:");
        eprintln!("  generate  - Generate Rust types from OpenAPI spec");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "generate" => generate_types(),
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }
}

fn generate_types() {
    // Paths relative to xtask directory
    let spec_path = Path::new("../../specs/client-control-plane.openapi.yaml");
    let output_path = Path::new("../src/generated.rs");

    println!("Reading OpenAPI spec from: {:?}", spec_path);

    let yaml_content = fs::read_to_string(spec_path).expect("Failed to read OpenAPI spec");

    let spec: serde_json::Value =
        serde_yaml::from_str(&yaml_content).expect("Failed to parse YAML");

    // Extract the schemas from components
    let schemas = spec
        .get("components")
        .and_then(|c| c.get("schemas"))
        .expect("No schemas found in OpenAPI spec");

    // Create a JSON Schema document from the OpenAPI schemas
    let schema_doc = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "definitions": schemas,
        "type": "object"
    });

    // Configure typify
    let mut settings = typify::TypeSpaceSettings::default();
    settings.with_derive("Debug".to_string());
    settings.with_derive("Clone".to_string());

    let mut type_space = typify::TypeSpace::new(&settings);

    // Add schema
    type_space
        .add_root_schema(serde_json::from_value(schema_doc).unwrap())
        .expect("Failed to add schema");

    // Generate code
    let generated_tokens = type_space.to_stream();

    // Parse and format with prettyplease
    let syntax_tree: syn::File =
        syn::parse2(generated_tokens).expect("Failed to parse generated code");
    let formatted = prettyplease::unparse(&syntax_tree);

    // Post-process: Replace NonZeroU32 with i32 for API compatibility
    // OpenAPI minimum:1 creates NonZeroU32, but we want i32 for simpler API
    let formatted = formatted.replace("::std::num::NonZeroU32", "i32");

    // Build final output with header
    let contents = format!(
        r#"//! Generated types from OpenAPI specification.
//!
//! This file is auto-generated. Do NOT edit manually.
//!
//! To regenerate: cd clients && task generate-rust-types

#![allow(clippy::all)]
#![allow(unused_imports)]

use serde::{{Deserialize, Serialize}};

{formatted}
"#
    );

    // Write output
    fs::write(output_path, &contents).expect("Failed to write output");

    println!("Generated types to: {:?}", output_path);
    println!("Done!");
}
