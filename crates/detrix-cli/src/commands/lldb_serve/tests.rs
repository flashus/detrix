//! Tests for lldb-serve

use super::protocol::{
    create_launch_request, get_rust_type_formatters, read_dap_message, write_dap_message,
};

#[test]
fn test_create_launch_request() {
    let request = create_launch_request("/path/to/binary", &["arg1".to_string()], true, None, None)
        .expect("Failed to create launch request");
    let parsed: serde_json::Value = serde_json::from_str(&request).expect("Failed to parse JSON");

    assert_eq!(parsed["command"], "launch");
    assert_eq!(parsed["arguments"]["program"], "/path/to/binary");
    assert_eq!(parsed["arguments"]["stopOnEntry"], true);

    // Verify init commands for type formatters are included
    let init_commands = parsed["arguments"]["initCommands"]
        .as_array()
        .expect("initCommands should be an array");
    assert!(
        !init_commands.is_empty(),
        "initCommands should contain type formatters"
    );

    // Check for String formatter
    let has_string_formatter = init_commands.iter().any(|cmd| {
        cmd.as_str()
            .map(|s| s.contains("alloc::") && s.contains("String"))
            .unwrap_or(false)
    });
    assert!(has_string_formatter, "Should have String type formatter");

    // Verify stdio is null when no path provided (to prevent program output from corrupting DAP stream)
    assert!(
        parsed["arguments"]["stdio"].is_null(),
        "stdio should be null when no path provided"
    );
}

#[test]
fn test_create_launch_request_with_stdio_path() {
    let request = create_launch_request(
        "/path/to/binary",
        &[],
        false,
        None,
        Some("/tmp/program_output.log"),
    )
    .expect("Failed to create launch request");
    let parsed: serde_json::Value = serde_json::from_str(&request).expect("Failed to parse JSON");

    // stdio should be array format [null, path, path] for better Windows compatibility
    let stdio = &parsed["arguments"]["stdio"];
    assert!(stdio.is_array(), "stdio should be an array");
    let stdio_arr = stdio.as_array().unwrap();
    assert_eq!(
        stdio_arr.len(),
        3,
        "stdio should have 3 elements [stdin, stdout, stderr]"
    );
    assert!(stdio_arr[0].is_null(), "stdin should be null");
    assert_eq!(
        stdio_arr[1], "/tmp/program_output.log",
        "stdout should be the log path"
    );
    assert_eq!(
        stdio_arr[2], "/tmp/program_output.log",
        "stderr should be the log path"
    );
}

#[test]
fn test_rust_type_formatters() {
    let formatters = get_rust_type_formatters();
    assert!(!formatters.is_empty(), "Should have type formatters");

    // Verify key formatters are present
    let formatters_str = formatters.join("\n");
    assert!(
        formatters_str.contains("String"),
        "Should have String formatter"
    );
    assert!(formatters_str.contains("Vec"), "Should have Vec formatter");
}

#[test]
fn test_dap_message_roundtrip() {
    let message = r#"{"seq":1,"type":"request","command":"initialize"}"#;
    let mut buf = Vec::new();
    write_dap_message(&mut buf, message).unwrap();

    let mut reader = std::io::BufReader::new(buf.as_slice());
    let read_message = read_dap_message(&mut reader).unwrap();

    assert_eq!(message, read_message);
}

/// Test that the DAP parser handles program output leaked before DAP messages
/// This is the exact scenario that was failing on Windows with CodeLLDB
#[test]
fn test_dap_message_with_program_output_leaked_before() {
    // Simulate the Windows CodeLLDB issue: program output appears before DAP message
    // From the debug log: "Add metrics with Detrix to observe values!" appeared before JSON
    let leaked_output = "Add metrics with Detrix to observe values!\n";
    let dap_message = r#"{"seq":15,"type":"event","event":"module"}"#;

    let mut buf = Vec::new();
    // Write leaked program output first
    buf.extend_from_slice(leaked_output.as_bytes());
    // Then write proper DAP message
    write_dap_message(&mut buf, dap_message).unwrap();

    let mut reader = std::io::BufReader::new(buf.as_slice());
    let read_message = read_dap_message(&mut reader).unwrap();

    // Should successfully parse the DAP message despite the leaked output
    assert_eq!(dap_message, read_message);
}

/// Test multiple lines of program output before DAP message
#[test]
fn test_dap_message_with_multiple_leaked_lines() {
    let leaked_output = "Line 1: Program starting...\nLine 2: Initializing...\nLine 3: Ready!\n";
    let dap_message = r#"{"seq":1,"type":"response","command":"initialize"}"#;

    let mut buf = Vec::new();
    buf.extend_from_slice(leaked_output.as_bytes());
    write_dap_message(&mut buf, dap_message).unwrap();

    let mut reader = std::io::BufReader::new(buf.as_slice());
    let read_message = read_dap_message(&mut reader).unwrap();

    assert_eq!(dap_message, read_message);
}

/// Test empty lines mixed with program output before DAP message
#[test]
fn test_dap_message_with_empty_lines_in_leaked_output() {
    let leaked_output = "Some output\n\nAnother line\n\n";
    let dap_message = r#"{"seq":42,"type":"event","event":"output"}"#;

    let mut buf = Vec::new();
    buf.extend_from_slice(leaked_output.as_bytes());
    write_dap_message(&mut buf, dap_message).unwrap();

    let mut reader = std::io::BufReader::new(buf.as_slice());
    let read_message = read_dap_message(&mut reader).unwrap();

    assert_eq!(dap_message, read_message);
}

/// Test that normal DAP messages (without leaked output) still work
#[test]
fn test_dap_message_normal_without_leakage() {
    let dap_message = r#"{"seq":1,"type":"request","command":"launch"}"#;

    let mut buf = Vec::new();
    write_dap_message(&mut buf, dap_message).unwrap();

    let mut reader = std::io::BufReader::new(buf.as_slice());
    let read_message = read_dap_message(&mut reader).unwrap();

    assert_eq!(dap_message, read_message);
}

/// Test sequential DAP messages with leaked output between them
#[test]
fn test_sequential_dap_messages_with_leaked_output_between() {
    let dap_message1 = r#"{"seq":1,"type":"request"}"#;
    let leaked_output = "Debug: processing...\n";
    let dap_message2 = r#"{"seq":2,"type":"response"}"#;

    let mut buf = Vec::new();
    write_dap_message(&mut buf, dap_message1).unwrap();
    buf.extend_from_slice(leaked_output.as_bytes());
    write_dap_message(&mut buf, dap_message2).unwrap();

    let mut reader = std::io::BufReader::new(buf.as_slice());

    // First message should parse correctly
    let read_message1 = read_dap_message(&mut reader).unwrap();
    assert_eq!(dap_message1, read_message1);

    // Second message should also parse correctly (despite leaked output before it)
    let read_message2 = read_dap_message(&mut reader).unwrap();
    assert_eq!(dap_message2, read_message2);
}

/// Test recovery from corrupted body (program output mixed into JSON body)
/// This is the exact scenario seen in Windows logs where output appears inside the body
#[test]
fn test_dap_message_body_corruption_recovery() {
    // Simulate: Content-Length matches but body starts with program output then JSON
    // Content-Length: 60 (calculated for the corrupted content)
    // Body: "Program output\n{"seq":1,"type":"event"}"
    let corrupted_body = "Program output\n{\"seq\":1,\"type\":\"event\"}";
    let content = format!(
        "Content-Length: {}\r\n\r\n{}",
        corrupted_body.len(),
        corrupted_body
    );

    let mut reader = std::io::BufReader::new(content.as_bytes());
    let result = read_dap_message(&mut reader);

    // Should recover by extracting just the JSON part
    assert!(result.is_some(), "Should recover JSON from corrupted body");
    let message = result.unwrap();
    assert!(
        message.starts_with('{'),
        "Recovered message should start with JSON: {}",
        message
    );
    assert!(
        message.contains("\"seq\":1"),
        "Recovered message should contain the JSON content"
    );
}

/// Test that truncated JSON is detected as invalid
/// This is the scenario that was causing cascade failures on Windows:
/// CodeLLDB sends a module event but the JSON is truncated mid-message.
/// The proxy should detect this and skip forwarding rather than sending
/// invalid JSON to the daemon.
#[test]
fn test_truncated_json_detection() {
    // Simulate a truncated module event (like what CodeLLDB was sending on Windows)
    // This is a real message that was cut off at "symbo" instead of completing
    let truncated_message = r#"{"seq":15,"type":"event","event":"module","body":{"module":{"addressRange":"7FFBCA640000","id":"7FFBCA640000","name":"ntdll.dll","path":"C:\\Windows\\System32\\ntdll.dll","symbolFilePath":"C:\\Windows\\System32\\ntdll.dll","symbo"#;

    // This should fail to parse as valid JSON
    let result = serde_json::from_str::<serde_json::Value>(truncated_message);
    assert!(
        result.is_err(),
        "Truncated JSON should fail to parse: {:?}",
        result
    );

    // The complete message should parse successfully
    let complete_message = r#"{"seq":15,"type":"event","event":"module","body":{"module":{"addressRange":"7FFBCA640000","id":"7FFBCA640000","name":"ntdll.dll","path":"C:\\Windows\\System32\\ntdll.dll","symbolFilePath":"C:\\Windows\\System32\\ntdll.dll","symbolStatus":"Symbols loaded."},"reason":"new"}}"#;

    let result = serde_json::from_str::<serde_json::Value>(complete_message);
    assert!(
        result.is_ok(),
        "Complete JSON should parse successfully: {:?}",
        result
    );
}
