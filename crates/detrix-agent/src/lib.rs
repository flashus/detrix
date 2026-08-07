//! Detrix Agent — Library Crate
//!
//! A lightweight binary deployed on each observed machine that runs the eBPF
//! stack locally and streams data to a centralized detrix server over gRPC.
//!
//! This crate provides the agent implementation as a library. The CLI entry
//! point is in `detrix-cli` via the `detrix agent` subcommand.

// Error types
pub mod error;

// Protocol conversions
pub mod proto_convert;

// /proc scanner
pub mod scanner;

// Adapter manager
pub mod adapter_manager;

// Metrics HTTP server
pub mod metrics_server;

// Agent main struct and run loop
pub mod agent;
