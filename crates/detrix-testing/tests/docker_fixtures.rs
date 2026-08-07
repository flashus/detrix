//! Centralized Docker fixture paths and service URLs.
//!
//! Single source of truth for container-internal file paths, service URLs,
//! and compose configuration used by Docker-based E2E tests.
//!
//! # Usage
//!
//! ```ignore
//! mod docker_fixtures;
//! use docker_fixtures::*;
//! ```

#![allow(dead_code)]

// ── Docker Compose ──

pub const COMPOSE_FILE: &str = "fixtures/docker/docker-compose.yml";
pub const COMPOSE_PROJECT: &str = "detrix-cloud-test";

// ── Service URLs (Docker-internal, used by daemon to reach apps) ──

pub const PYTHON_APP_URL: &str = "http://test-app-python:8091";
pub const GO_APP_URL: &str = "http://test-app-go:8091";
pub const RUST_APP_URL: &str = "http://test-app-rust:8091";

// ── Container-internal file paths (from DWARF debug info / runtime WORKDIR) ──

pub const PYTHON_FILE: &str = "/app/trade_bot_forever.py";
pub const GO_FILE: &str = "/src/fixtures/go/string_capture/main.go";
pub const RUST_FILE: &str = "/src/fixtures/rust/src/main.rs";

// ── Host-mapped ports ──

pub const DAEMON_HTTP_PORT: u16 = 8095;
pub const DAEMON_GRPC_PORT: u16 = 50065;

// ── Auth ──

pub const DOCKER_AUTH_TOKEN: &str = "docker-test-token";

// ── Advertise ──

pub const ADVERTISE_URL: &str = "http://localhost:8095";
