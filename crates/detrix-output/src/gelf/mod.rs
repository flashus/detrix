//! GELF (Graylog Extended Log Format) Output
//!
//! Implements event streaming to Graylog via GELF protocol.
//! Supports TCP, UDP, and HTTP transports.

mod http;
mod message;
mod router;
mod tcp;
mod udp;

pub use http::{GelfHttpBuilder, GelfHttpConfig, GelfHttpOutput};
pub use message::{GelfLevel, GelfMessage};
pub use router::{
    GelfMessageExt, RoutedGelfBuilder, RoutedGelfOutput, RoutedGelfTcpOutput, RouterConfig,
    StreamRule,
};
pub use tcp::{GelfTcpBuilder, GelfTcpConfig, GelfTcpOutput};
pub use udp::{GelfUdpBuilder, GelfUdpConfig, GelfUdpOutput};
