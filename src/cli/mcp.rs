//! `kew mcp serve` — start the MCP server on stdio.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::db::Database;

#[derive(Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommands,
}

#[derive(Subcommand)]
pub enum McpCommands {
    /// Start the MCP server (stdio transport)
    Serve,
}

#[cfg(feature = "mcp")]
pub async fn execute(args: &McpArgs, db_path: &str, ollama_url: &str) -> Result<()> {
    match args.command {
        McpCommands::Serve => {
            let db =
                Database::open(std::path::Path::new(db_path)).context("failed to open database")?;
            crate::mcp::server::serve(db, ollama_url).await
        }
    }
}

#[cfg(not(feature = "mcp"))]
pub async fn execute(_args: &McpArgs, _db_path: &str, _ollama_url: &str) -> Result<()> {
    anyhow::bail!("MCP support not compiled. Rebuild with: cargo build --features mcp");
}
