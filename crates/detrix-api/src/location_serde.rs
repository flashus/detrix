//! Custom deserialization for Location that accepts both object and string formats.
//!
//! Supports:
//! - Object: `{"file": "path.py", "line": 42}`
//! - String: `"@path.py#42"`, `"path.py#42"`, `"./path.py#42"`
//!
//! String parsing delegates to [`crate::common::parse_location_str`] -
//! the same core parser used by MCP tools.

use serde::Deserialize;

/// Intermediate type for flexible Location deserialization.
///
/// Uses `#[serde(untagged)]` to accept both JSON object and string formats.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum LocationInput {
    /// Standard object format: `{"file": "path.py", "line": 42}`
    Object { file: String, line: u32 },
    /// String format: `"@path.py#42"` or `"path.py#42"`
    String(String),
}

impl TryFrom<LocationInput> for crate::generated::detrix::v1::Location {
    type Error = String;

    fn try_from(input: LocationInput) -> Result<Self, Self::Error> {
        match input {
            LocationInput::Object { file, line } => Ok(Self { file, line }),
            LocationInput::String(s) => {
                let loc = crate::common::parsing::parse_location_str(&s)?;
                Ok(Self {
                    file: loc.file,
                    line: loc.line,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::generated::detrix::v1::Location;

    #[test]
    fn test_deserialize_object_format() {
        let json = r#"{"file": "main.py", "line": 42}"#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.file, "main.py");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn test_deserialize_string_with_at() {
        let json = r#""@main.py#42""#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.file, "main.py");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn test_deserialize_string_without_at() {
        let json = r#""main.py#42""#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.file, "main.py");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn test_deserialize_absolute_path() {
        let json = r#""@/home/user/project/main.py#100""#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.file, "/home/user/project/main.py");
        assert_eq!(loc.line, 100);
    }

    #[test]
    fn test_deserialize_relative_path() {
        let json = r#""./src/main.go#55""#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.file, "src/main.go");
        assert_eq!(loc.line, 55);
    }

    #[test]
    fn test_deserialize_windows_path() {
        let json = r#""C:/Users/dev/main.rs#10""#;
        let loc: Location = serde_json::from_str(json).unwrap();
        assert_eq!(loc.file, "C:/Users/dev/main.rs");
        assert_eq!(loc.line, 10);
    }

    #[test]
    fn test_deserialize_invalid_no_line() {
        let json = r#""main.py""#;
        let result: Result<Location, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_invalid_bad_line() {
        let json = r#""main.py#abc""#;
        let result: Result<Location, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_always_object() {
        let loc = Location {
            file: "main.py".to_string(),
            line: 42,
        };
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["file"], "main.py");
        assert_eq!(json["line"], 42);
    }
}
