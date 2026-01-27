//! LSP-based Purity Analysis for Detrix
//!
//! This crate provides Language Server Protocol (LSP) integration for
//! analyzing user-defined functions to determine if they have side effects.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  PythonPurityAnalyzer (implements PurityAnalyzer trait)             │
//! │  - Main entry point for purity analysis                             │
//! │  - Uses LspManager for server lifecycle                             │
//! │  - Uses CallHierarchyTraverser for recursive analysis              │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │  LspManager                                                         │
//! │  - Lazy-starts LSP server on first analysis                        │
//! │  - Manages server lifecycle (start, stop, restart)                 │
//! │  - Handles connection health monitoring                            │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │  LspClient                                                          │
//! │  - JSON-RPC communication over stdio                               │
//! │  - LSP protocol message handling                                   │
//! │  - Request/response correlation                                    │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │  CallHierarchyTraverser                                            │
//! │  - Recursive call hierarchy analysis                               │
//! │  - Purity classification using whitelist/blacklist                │
//! │  - Max depth handling                                              │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_lsp::PythonPurityAnalyzer;
//! use detrix_application::PurityAnalyzer;
//! use std::path::Path;
//!
//! let analyzer = PythonPurityAnalyzer::new(&config)?;
//! analyzer.ensure_ready().await?;
//!
//! let analysis = analyzer.analyze_function("user.get_data", Path::new("api.py")).await?;
//! println!("Purity: {:?}", analysis.level);
//! ```

mod base;
mod cache;
mod client;
pub mod common;
mod error;
mod go;
mod manager;
mod mutation;
mod protocol;
mod python;
mod rust;
#[cfg(test)]
pub mod unified_tests;

pub use base::{BasePurityAnalyzer, LanguageAnalyzer};
pub use cache::{cache_result, compute_file_hash, get_cached, CacheRef};
pub use client::LspClient;
pub use error::{Error, Result};
pub use go::GoPurityAnalyzer;
pub use manager::{LspManager, LspState};
pub use mutation::{GoMutationTarget, MutationTarget, PythonMutationTarget, RustMutationTarget};
pub use protocol::{LspMessage, LspRequest, LspResponse};
pub use python::PythonPurityAnalyzer;
pub use rust::RustPurityAnalyzer;

// Re-export types from application layer for convenience
pub use detrix_application::PurityAnalyzer;
