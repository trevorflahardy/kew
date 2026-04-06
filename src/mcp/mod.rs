//! MCP server: exposes kew functionality as MCP tools.
//!
//! Started via `kew mcp serve` (stdio transport).
//! Claude Code connects to this as an MCP server.

#[cfg(feature = "mcp")]
pub mod server;
