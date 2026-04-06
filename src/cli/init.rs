//! `kew init` — project setup command.
//!
//! Creates .kew/ directory, scaffolds kew_config.yaml, updates .gitignore,
//! and optionally injects MCP config into Claude Code settings.

use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;

#[derive(Args)]
pub struct InitArgs {
    /// Skip MCP injection into Claude settings
    #[arg(long)]
    pub no_mcp: bool,

    /// Skip .gitignore modification
    #[arg(long)]
    pub no_gitignore: bool,

    /// Set default model in kew_config.yaml
    #[arg(long, default_value = "gemma3:27b")]
    pub model: String,

    /// Overwrite existing kew_config.yaml
    #[arg(long)]
    pub force: bool,
}

const KEW_CONFIG_TEMPLATE: &str = r#"# kew_config.yaml — project-level kew configuration
# See: https://github.com/futur/kew

defaults:
  model: {model}
  provider: ollama
  workers: 4
  timeout: 5m

ollama:
  url: http://localhost:11434
  embedding_model: nomic-embed-text

# Model aliases
aliases:
  fast: gemma3:27b
  # smart: claude-sonnet-4-20250514
  # code: codellama

# Agent type overrides (override built-in defaults)
# agents:
#   coder:
#     model: codellama
#     system_prompt_append: |
#       This project uses Rust with tokio for async.
"#;

/// Execute the `kew init` command.
pub fn execute(args: &InitArgs) -> Result<()> {
    let project_dir = std::env::current_dir().context("cannot determine current directory")?;

    // 1. Create .kew/ directory
    let kew_dir = project_dir.join(".kew");
    if !kew_dir.exists() {
        std::fs::create_dir_all(&kew_dir).context("creating .kew/ directory")?;
        println!("\u{2713} Created .kew/ directory");
    } else {
        println!("  .kew/ already exists");
    }

    // 2. Create .kew/agents/ for project-specific agents
    let agents_dir = kew_dir.join("agents");
    if !agents_dir.exists() {
        std::fs::create_dir_all(&agents_dir).context("creating .kew/agents/ directory")?;
        println!("\u{2713} Created .kew/agents/ directory");
    }

    // 3. Initialize the database
    let db_path = kew_dir.join("kew.db");
    crate::db::Database::open(&db_path).context("initializing database")?;
    println!("\u{2713} Initialized .kew/kew.db");

    // 4. Scaffold kew_config.yaml
    let config_path = project_dir.join("kew_config.yaml");
    if !config_path.exists() || args.force {
        let config = KEW_CONFIG_TEMPLATE.replace("{model}", &args.model);
        std::fs::write(&config_path, config).context("writing kew_config.yaml")?;
        println!("\u{2713} Generated kew_config.yaml");
    } else {
        println!("  kew_config.yaml already exists (use --force to overwrite)");
    }

    // 5. Append .kew/ to .gitignore
    if !args.no_gitignore {
        append_gitignore(&project_dir)?;
    }

    // 6. Inject MCP config (Phase 8 — placeholder for now)
    if !args.no_mcp {
        inject_mcp_config(&project_dir)?;
    }

    println!();
    println!("Ready. Try: kew run -m {} -w \"Say hello\"", args.model);

    Ok(())
}

/// Append .kew/ to .gitignore if not already present.
fn append_gitignore(project_dir: &Path) -> Result<()> {
    let gitignore_path = project_dir.join(".gitignore");
    let entry = ".kew/";

    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path)?;
        if content.lines().any(|line| line.trim() == entry) {
            println!("  .gitignore already contains .kew/");
            return Ok(());
        }
        // Append
        let mut new_content = content;
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(entry);
        new_content.push('\n');
        std::fs::write(&gitignore_path, new_content)?;
    } else {
        std::fs::write(&gitignore_path, format!("{entry}\n"))?;
    }

    println!("\u{2713} Added .kew/ to .gitignore");
    Ok(())
}

/// Inject kew MCP server config into Claude Code's project-local settings.
fn inject_mcp_config(project_dir: &Path) -> Result<()> {
    let claude_dir = project_dir.join(".claude");
    let settings_path = claude_dir.join("settings.local.json");

    // Read existing settings or start fresh
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        if !claude_dir.exists() {
            std::fs::create_dir_all(&claude_dir)?;
        }
        serde_json::json!({})
    };

    // Ensure mcpServers object exists
    if settings.get("mcpServers").is_none() {
        settings["mcpServers"] = serde_json::json!({});
    }

    // Add/update kew entry (don't clobber other servers)
    settings["mcpServers"]["kew"] = serde_json::json!({
        "command": "kew",
        "args": ["mcp", "serve", "--db", ".kew/kew.db"]
    });

    // Write back
    let json = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, format!("{json}\n"))?;
    println!("\u{2713} Injected kew MCP server into .claude/settings.local.json");

    Ok(())
}
