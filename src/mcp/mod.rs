pub mod client;
pub mod config;

pub use client::{
    McpConnection, McpServerStatus, McpTool, McpToolBackend, SharedMcpBackends,
    connect_mcp_servers, create_placeholder_statuses,
};
