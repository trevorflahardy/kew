//! `kew agent` — list, inspect, and manage agent configurations.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::agents;

#[derive(Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommands,
}

#[derive(Subcommand)]
pub enum AgentCommands {
    /// List all available agents (built-in and custom)
    List,

    /// Show an agent's full configuration including system prompt
    Show {
        /// Agent name
        name: String,
    },

    /// Export a built-in agent to .kew/agents/ for local customization
    Export {
        /// Agent name to export
        name: String,
        /// Output directory (default: .kew/agents/)
        #[arg(long, default_value = ".kew/agents")]
        dir: String,
    },
}

pub fn execute(args: &AgentArgs) -> Result<()> {
    match &args.command {
        AgentCommands::List => list_agents(),
        AgentCommands::Show { name } => show_agent(name),
        AgentCommands::Export { name, dir } => export_agent(name, dir),
    }
}

fn list_agents() -> Result<()> {
    let project_dir = std::env::current_dir().ok();
    let agents = agents::list_agents(project_dir.as_deref());

    println!("Available agents:\n");
    println!("  {:<15} {:<10} {}", "NAME", "SOURCE", "DESCRIPTION");
    println!("  {}", "-".repeat(72));

    for entry in &agents {
        let source_tag = match entry.source.as_str() {
            "project" => "\x1b[32mproject\x1b[0m",
            "user" => "\x1b[33muser\x1b[0m   ",
            _ => "builtin",
        };
        println!("  {:<15} {}  {}", entry.name, source_tag, entry.description);
    }

    println!();
    println!("Use with:  kew run --agent <name> -w \"your prompt\"");
    println!("Customize: kew agent export <name>  (copies to .kew/agents/)");
    Ok(())
}

fn show_agent(name: &str) -> Result<()> {
    let project_dir = std::env::current_dir().ok();
    let cfg = agents::load_agent(name, project_dir.as_deref())?;

    println!("Agent: {}", cfg.name);
    println!("Description: {}", cfg.description);
    if let Some(ref model) = cfg.model {
        println!("Preferred model: {model}");
    }
    println!();
    println!("--- System Prompt ---");
    println!("{}", cfg.system_prompt.trim_end());
    Ok(())
}

fn export_agent(name: &str, dir: &str) -> Result<()> {
    let project_dir = std::env::current_dir().ok();
    let cfg = agents::load_agent(name, project_dir.as_deref())?;

    let out_dir = std::path::Path::new(dir);
    std::fs::create_dir_all(out_dir)?;

    let out_path = out_dir.join(format!("{name}.yaml"));
    if out_path.exists() {
        anyhow::bail!("{out_path:?} already exists. Delete it first or edit it directly.");
    }

    let yaml = format!(
        "name: {}\ndescription: {}\n{}system_prompt: |\n{}\n",
        cfg.name,
        cfg.description,
        cfg.model
            .as_ref()
            .map(|m| format!("model: {m}\n"))
            .unwrap_or_default(),
        cfg.system_prompt
            .lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    std::fs::write(&out_path, yaml)?;
    println!("Exported '{name}' to {out_path:?}");
    println!("Edit it freely — project-local agents override built-ins.");
    Ok(())
}
