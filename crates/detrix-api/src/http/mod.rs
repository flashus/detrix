//! HTTP API server module
//!
//! This module provides REST, WebSocket, and MCP HTTP endpoints using axum.
//! All endpoints share the same ApiState and follow Clean Architecture principles:
//! - Handlers do DTO mapping ONLY
//! - All business logic delegated to application services
//!
//! # Route Structure
//!
//! ```text
//! /health                    - Health check
//! /api/v1/metrics            - GET (list), POST (add)
//! /api/v1/metrics/:id        - GET, DELETE
//! /api/v1/events             - GET (query)
//! /ws                        - WebSocket upgrade
//! /mcp                       - MCP JSON-RPC over HTTP
//! /mcp/heartbeat             - MCP client heartbeat (keep-alive)
//! /mcp/disconnect            - MCP client disconnect notification
//! ```

pub mod error;
pub mod handlers;
pub mod mcp;
pub mod middleware;
pub mod paths;
pub mod proto_adapters;
pub mod routes;
pub mod server;
pub mod websocket;

pub use error::{ErrorResponse, HttpError, ToHttpOption, ToHttpResult};
pub use middleware::{auth_middleware, AuthState};
pub use routes::{create_router, RouterConfigError};
pub use server::HttpServer;
pub use websocket::websocket_handler;
