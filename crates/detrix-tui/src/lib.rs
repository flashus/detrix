//! Detrix TUI - Terminal User Interface for Detrix metrics dashboard
//!
//! This crate provides a real-time terminal dashboard for monitoring
//! Detrix metrics, events, connections, and system status.
//!
//! # Features
//!
//! - **Live Event Streaming**: Real-time display of metric events via gRPC
//! - **Metrics Management**: View and toggle metrics
//! - **Connection Monitoring**: Track debugger adapter connections
//! - **System Log**: View system events and health status
//! - **Customizable Themes**: Dark/light themes with custom color overrides
//!
//! # Architecture
//!
//! The TUI follows a standard event-driven architecture:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      Main Event Loop                        │
//! │                                                             │
//! │  ┌─────────┐    ┌─────────┐    ┌─────────┐                 │
//! │  │ Keyboard│───▶│   App   │───▶│  Render │                 │
//! │  │  Input  │    │  State  │    │   UI    │                 │
//! │  └─────────┘    └────┬────┘    └─────────┘                 │
//! │                      │                                      │
//! │  ┌─────────┐         │                                      │
//! │  │  gRPC   │─────────┘                                      │
//! │  │ Stream  │                                                │
//! │  └─────────┘                                                │
//! └─────────────────────────────────────────────────────────────┘
//! ```

pub mod app;
pub mod client;
pub mod event;
pub mod input;
pub mod ui;

pub use app::{App, AppMode, Tab};
pub use client::DetrixClient;
pub use event::AppEvent;
pub use ui::theme::Theme;
