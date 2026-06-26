//! Minimal MCP (Model Context Protocol) client.
//!
//! Ox is **not** a general-purpose MCP host. This module is a deliberately small
//! stdio JSON-RPC client whose only job is to spawn a single MCP server (today:
//! GitNexus) as a child process and talk to it: `initialize` handshake +
//! `tools/list` + `tools/call`. Higher-level lifecycle (detection, auto-index,
//! restart, action routing) is layered on top of this client elsewhere.

pub mod client;
pub mod detect;
pub mod gitnexus;

pub use client::{McpClient, McpServerSpec};
pub use detect::{GitNexusAvailability, detect};
pub use gitnexus::{GitNexusService, GitNexusStatus, GraphResult};
