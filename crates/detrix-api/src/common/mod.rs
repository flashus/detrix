//! Common utilities shared across API layers (MCP, REST, gRPC)

pub mod diff_parser;
pub mod expression_extractor;
pub mod parsing;

pub use diff_parser::{parse_diff, DiffParseResult, ParsedDiffLine, UnparseableLine};
pub use expression_extractor::{
    detect_language, get_extractor, Confidence, ExpressionExtractor, ExtractedExpression,
    GoExtractor, PythonExtractor, RustExtractor,
};
pub use parsing::{
    extract_variable_names, generate_metric_name, generate_metric_name_with_prefix, parse_location,
    parse_location_flexible, parse_location_str, parse_location_with_expression,
    ParsedLocationWithExpression,
};
