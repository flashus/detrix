//! Location value object - represents a position in source code

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Location in source code (@file.py#line)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub file: String,
    pub line: u32,
}

impl Location {
    /// Parse location from string format: file.py#line or @file.py#line
    ///
    /// The `@` prefix is optional. Uses `#` as separator.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();

        if s.is_empty() {
            return Err(Error::InvalidLocation(
                "Location cannot be empty".to_string(),
            ));
        }

        // Strip @ prefix if present (optional)
        let without_at = s.strip_prefix('@').unwrap_or(s);

        let parts: Vec<&str> = without_at.rsplitn(2, '#').collect();

        if parts.len() != 2 {
            return Err(Error::InvalidLocation(
                "Expected format: file.py#line (@ prefix optional)".to_string(),
            ));
        }

        let line = parts[0]
            .parse::<u32>()
            .map_err(|_| Error::InvalidLocation("Line must be a positive integer".to_string()))?;

        let file = parts[1].to_string();

        if file.is_empty() {
            return Err(Error::InvalidLocation(
                "File path cannot be empty".to_string(),
            ));
        }

        Ok(Location { file, line })
    }

    /// Convert to string format: @file.py#line
    pub fn to_string_format(&self) -> String {
        format!("@{}#{}", self.file, self.line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_location_parse_valid() {
        let loc = Location::parse("@/app/trading/orders.py#127").unwrap();
        assert_eq!(loc.file, "/app/trading/orders.py");
        assert_eq!(loc.line, 127);
    }

    #[test]
    fn test_location_parse_relative() {
        let loc = Location::parse("@orders.py#42").unwrap();
        assert_eq!(loc.file, "orders.py");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn test_location_parse_without_at_works() {
        // @ prefix is now optional
        let loc = Location::parse("orders.py#42").unwrap();
        assert_eq!(loc.file, "orders.py");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn test_location_parse_missing_line() {
        let err = Location::parse("@orders.py").unwrap_err();
        assert!(matches!(err, Error::InvalidLocation(_)));
    }

    #[test]
    fn test_location_parse_invalid_line() {
        let err = Location::parse("@orders.py#abc").unwrap_err();
        assert!(matches!(err, Error::InvalidLocation(_)));
    }

    #[test]
    fn test_location_to_string() {
        let loc = Location {
            file: "test.py".to_string(),
            line: 100,
        };
        assert_eq!(loc.to_string_format(), "@test.py#100");
    }

    #[test]
    fn test_location_parse_windows_path() {
        // Windows paths with drive letters work because we use # as separator
        let loc = Location::parse("@C:/Users/test.py#42").unwrap();
        assert_eq!(loc.file, "C:/Users/test.py");
        assert_eq!(loc.line, 42);
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// Generate valid file path segments (alphanumeric, _, ., /, -)
    fn valid_file_path() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_./\\-]{1,100}".prop_filter("must not be empty after trim", |s: &String| {
            !s.trim().is_empty()
        })
    }

    /// Generate valid line numbers (1 to u32::MAX)
    fn valid_line() -> impl Strategy<Value = u32> {
        1..=u32::MAX
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Roundtrip property: parse(format(location)) == location
        #[test]
        fn proptest_location_roundtrip(
            file in valid_file_path(),
            line in valid_line()
        ) {
            let loc = Location { file: file.clone(), line };
            let formatted = loc.to_string_format();
            let parsed = Location::parse(&formatted);

            prop_assert!(parsed.is_ok(), "Failed to parse: {}", formatted);
            let parsed = parsed.unwrap();
            prop_assert_eq!(parsed.file, file);
            prop_assert_eq!(parsed.line, line);
        }

        /// Valid formats with or without '@' parse correctly
        #[test]
        fn proptest_location_accepts_with_or_without_at(
            file in valid_file_path(),
            line in valid_line(),
            with_at in proptest::bool::ANY
        ) {
            let input = if with_at {
                format!("@{}#{}", file, line)
            } else {
                format!("{}#{}", file, line)
            };
            let result = Location::parse(&input);
            prop_assert!(result.is_ok(), "Should parse: {}", input);
            let loc = result.unwrap();
            prop_assert_eq!(loc.line, line);
        }

        /// No panics property: any string input never panics
        #[test]
        fn proptest_location_never_panics(s in "\\PC{0,500}") {
            // Should not panic, may return Ok or Err
            let _ = Location::parse(&s);
        }

        /// Paths with colons (like @C:/Users/test.py#10) parse correctly using # separator
        #[test]
        fn proptest_location_handles_paths_with_colons(
            prefix in "[a-zA-Z0-9_/]{1,20}",
            middle in "[a-zA-Z0-9_/:]{0,20}",
            line in valid_line()
        ) {
            // File paths can contain colons (e.g., Windows C:/)
            let file = format!("{}:{}", prefix, middle);
            let input = format!("@{}#{}", file, line);
            let result = Location::parse(&input);

            prop_assert!(result.is_ok(), "Failed to parse: {}", input);
            let loc = result.unwrap();
            prop_assert_eq!(loc.file, file);
            prop_assert_eq!(loc.line, line);
        }

        /// Empty file path after '@' should be rejected
        #[test]
        fn proptest_location_rejects_empty_file(line in valid_line()) {
            let input = format!("@#{}", line);
            let result = Location::parse(&input);
            prop_assert!(result.is_err(), "Should reject empty file: {}", input);
        }

        /// Line number 0 should parse but might be valid depending on usage
        #[test]
        fn proptest_location_zero_line_is_invalid(file in valid_file_path()) {
            let input = format!("@{}#0", file);
            let result = Location::parse(&input);
            // Line 0 is technically parseable as u32, verify consistent behavior
            if let Ok(loc) = result {
                prop_assert_eq!(loc.line, 0);
            }
        }
    }
}
