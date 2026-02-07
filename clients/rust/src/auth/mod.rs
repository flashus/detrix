//! Authentication utilities for the Detrix client.

mod token;

pub use token::{discover_token, is_authorized};
