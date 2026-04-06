//! Kew — Real local agent orchestration.
//!
//! A single binary that spawns real LLM agents (Ollama, Claude API),
//! coordinates them via SQLite, and learns from past work.

pub mod cli;
pub mod db;
pub mod llm;
pub mod mcp;
pub mod tui;
pub mod worker;

use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Set up logging
    let filter = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_target(false)
        .init();

    // Resolve DB path: CLI flag > .kew/kew.db in current dir
    let db_path = cli.db.unwrap_or_else(|| {
        let local = std::path::Path::new(".kew/kew.db");
        if local.exists() {
            return local.to_string_lossy().to_string();
        }
        // Fallback to XDG-style global path
        dirs_default_db()
    });

    let result = match cli.command {
        Commands::Run(ref args) => cli::run::execute(args, &db_path, &cli.ollama_url, cli.claude_key.as_deref()).await,
        Commands::Init(ref args) => {
            // Init is synchronous
            cli::init::execute(args)
        }
        Commands::Chain(ref args) => cli::chain::execute(args, &db_path, &cli.ollama_url, cli.claude_key.as_deref()).await,
        Commands::Context(ref args) => cli::context::execute(args, &db_path, &cli.ollama_url).await,
        Commands::Mcp(ref args) => cli::mcp::execute(args, &db_path, &cli.ollama_url).await,
        Commands::Status(ref args) => cli::status::execute(args, &db_path),
        Commands::Doctor(_) => cli::doctor::execute(&cli.ollama_url, &db_path).await,
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Default database path: ~/.local/share/kew/kew.db
fn dirs_default_db() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let path = std::path::PathBuf::from(home)
            .join(".local/share/kew/kew.db");
        return path.to_string_lossy().to_string();
    }
    ".kew/kew.db".to_string()
}
