//! `kew mcp serve` — start the MCP server on stdio.
//!
//! Loads `kew_config.yaml` from the current directory to resolve runtime
//! settings (e.g. worker pool size) before starting the server. CLI flags
//! always take precedence over config file values.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::config::KewConfig;
use crate::db::Database;

/// Arguments for the `kew mcp` subcommand.
#[derive(Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommands,
}

/// MCP subcommands.
#[derive(Subcommand)]
pub enum McpCommands {
    /// Start the MCP server on stdio for Claude Code integration.
    Serve,
}

#[cfg(feature = "mcp")]
pub async fn execute(args: &McpArgs, db_path: &str, ollama_url: &str) -> Result<()> {
    match args.command {
        McpCommands::Serve => {
            let db =
                Database::open(std::path::Path::new(db_path)).context("failed to open database")?;

            // Load project config — missing file is fine, defaults are used.
            let cfg = KewConfig::load_cwd().unwrap_or_default();
            let workers = cfg.workers(4);

            crate::mcp::server::serve(db, ollama_url, workers).await
        }
    }
}

#[cfg(not(feature = "mcp"))]
pub async fn execute(_args: &McpArgs, _db_path: &str, _ollama_url: &str) -> Result<()> {
    anyhow::bail!("MCP support not compiled. Rebuild with: cargo build --features mcp");
}
