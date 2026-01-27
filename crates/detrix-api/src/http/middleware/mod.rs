//! HTTP middleware for Detrix API
//!
//! This module provides middleware for request processing:
//! - Authentication

mod auth;

pub use auth::{auth_middleware, AuthState};
