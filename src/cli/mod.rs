//! CLI command definitions using clap derive macros.

pub mod chain;
pub mod context;
pub mod doctor;
pub mod init;
pub mod mcp;
pub mod run;
pub mod status;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "kew", version, about = "Real local agent orchestration")]
#[command(long_about = "Kew spawns real LLM agents that do real work. No theater.")]
pub struct Cli {
    /// Path to SQLite database
    #[arg(long, global = true, env = "KEW_DB")]
    pub db: Option<String>,

    /// Ollama API URL
    #[arg(long, global = true, default_value = "http://localhost:11434", env = "KEW_OLLAMA_URL")]
    pub ollama_url: String,

    /// Anthropic API key (for Claude models)
    #[arg(long, global = true, env = "ANTHROPIC_API_KEY")]
    pub claude_key: Option<String>,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Execute a task through an LLM agent
    Run(run::RunArgs),

    /// Initialize kew for this project
    Init(init::InitArgs),

    /// Execute a sequential chain of LLM tasks
    Chain(chain::ChainArgs),

    /// Manage shared context entries
    Context(context::ContextArgs),

    /// MCP server commands
    Mcp(mcp::McpArgs),

    /// Show system status (TUI dashboard)
    Status(status::StatusArgs),

    /// Check system health
    Doctor(doctor::DoctorArgs),
}
