//! Output formatting module for CLI commands
//!
//! Provides unified output formatting with support for:
//! - Table format (default, human-readable)
//! - JSON format (for scripting/automation)
//! - Toon format (LLM-optimized, matches MCP output)
//! - Quiet mode (IDs only)

mod format;
mod json;
mod table;

pub use format::{Formatter, OutputFormat};
pub use table::set_use_utc;
