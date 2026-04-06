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

    /// Skip status line injection into Claude settings
    #[arg(long)]
    pub no_statusline: bool,

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

    // 6. Inject MCP config
    if !args.no_mcp {
        inject_mcp_config(&project_dir)?;
    }

    // 7. Inject status line into Claude Code settings
    if !args.no_statusline {
        inject_statusline_config(&project_dir)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_append_gitignore_creates_new() {
        let dir = tempdir().unwrap();
        append_gitignore(dir.path()).unwrap();

        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains(".kew/"));
    }

    #[test]
    fn test_append_gitignore_appends_to_existing() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        append_gitignore(dir.path()).unwrap();

        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains("node_modules/"));
        assert!(content.contains(".kew/"));
    }

    #[test]
    fn test_append_gitignore_no_duplicate() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".kew/\n").unwrap();
        append_gitignore(dir.path()).unwrap();

        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(content.matches(".kew/").count(), 1);
    }

    #[test]
    fn test_inject_mcp_config_creates_fresh() {
        let dir = tempdir().unwrap();
        inject_mcp_config(dir.path()).unwrap();

        let content = std::fs::read_to_string(
            dir.path().join(".claude/settings.local.json"),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["mcpServers"]["kew"].is_object());
        assert_eq!(json["mcpServers"]["kew"]["command"], "kew");
    }

    #[test]
    fn test_inject_mcp_config_preserves_existing() {
        let dir = tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{"mcpServers":{"other":{"command":"other"}}}"#,
        )
        .unwrap();

        inject_mcp_config(dir.path()).unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.local.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["mcpServers"]["other"].is_object());
        assert!(json["mcpServers"]["kew"].is_object());
    }

    #[test]
    fn test_inject_statusline_writes_script_and_settings() {
        let dir = tempdir().unwrap();
        inject_statusline_config(dir.path()).unwrap();

        // Script exists and contains the sentinel comment
        let script = std::fs::read_to_string(dir.path().join(".claude/kew-statusline.sh")).unwrap();
        assert!(script.contains("kew init"));
        assert!(script.contains("kew --db"));

        // settings.local.json has the statusLine key
        let content = std::fs::read_to_string(
            dir.path().join(".claude/settings.local.json"),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["statusLine"]["type"], "command");
        assert!(json["statusLine"]["command"].as_str().unwrap().contains("kew-statusline.sh"));
    }

    #[test]
    fn test_inject_statusline_preserves_existing_settings() {
        let dir = tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.local.json"),
            r#"{"mcpServers":{"kew":{"command":"kew"}}}"#,
        )
        .unwrap();

        inject_statusline_config(dir.path()).unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.local.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Existing MCP config preserved
        assert!(json["mcpServers"]["kew"].is_object());
        // New statusLine added
        assert_eq!(json["statusLine"]["type"], "command");
    }

    #[test]
    fn test_config_template_substitution() {
        let config = KEW_CONFIG_TEMPLATE.replace("{model}", "gemma4:26b");
        assert!(config.contains("model: gemma4:26b"));
        assert!(!config.contains("{model}"));
    }
}

/// Shell script written to .claude/kew-statusline.sh on `kew init`.
/// Uses Claude Code's stdin JSON to find the project DB — no hardcoded paths.
const STATUSLINE_SCRIPT: &str = r#"#!/bin/sh
# kew status line for Claude Code — injected by `kew init`
# Reads workspace context from stdin, queries kew, renders a compact status bar.

input=$(cat)

project_dir=$(printf '%s' "$input" | jq -r '.workspace.project_dir // .cwd // empty' 2>/dev/null)
db_path=""
if [ -n "$project_dir" ] && [ -f "$project_dir/.kew/kew.db" ]; then
  db_path="$project_dir/.kew/kew.db"
else
  db_path="$HOME/.local/share/kew/kew.db"
fi

kew_status=""
if command -v kew >/dev/null 2>&1 && [ -f "$db_path" ]; then
  kew_status=$(kew --db "$db_path" status --porcelain 2>/dev/null)
fi

if [ -z "$kew_status" ]; then
  printf "◆ kew  offline"
  exit 0
fi

_get() { printf '%s' "$kew_status" | grep -o "$1=[^ ]*" | cut -d= -f2; }

running=$(_get running)
pending=$(_get pending)
done_count=$(_get done)
failed=$(_get failed)
context=$(_get context)
embeddings=$(_get embeddings)

parts=""
if [ "${running:-0}" -gt 0 ]; then
  parts="${parts}▶ ${running} "
else
  parts="${parts}▷ 0 "
fi
if [ "${pending:-0}" -gt 0 ]; then
  parts="${parts}⏳${pending} "
fi
parts="${parts}✓${done_count:-0}"
if [ "${failed:-0}" -gt 0 ]; then
  parts="${parts} ✗${failed}"
fi
parts="${parts}  ctx:${context:-0} emb:${embeddings:-0}"

if [ -f "$db_path" ]; then
  db_indicator="DB:ok"
else
  db_indicator="DB:?"
fi

printf "◆ kew  %s  %s" "$parts" "$db_indicator"
"#;

/// Write the statusline script and wire it into .claude/settings.local.json.
fn inject_statusline_config(project_dir: &Path) -> Result<()> {
    let claude_dir = project_dir.join(".claude");
    if !claude_dir.exists() {
        std::fs::create_dir_all(&claude_dir)?;
    }

    // Write the script
    let script_path = claude_dir.join("kew-statusline.sh");
    std::fs::write(&script_path, STATUSLINE_SCRIPT)?;

    // Make it executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms)?;
    }

    // Merge statusLine into settings.local.json (relative path — project-local setting)
    let settings_path = claude_dir.join("settings.local.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    settings["statusLine"] = serde_json::json!({
        "type": "command",
        "command": "sh .claude/kew-statusline.sh"
    });

    let json = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, format!("{json}\n"))?;
    println!("\u{2713} Injected kew status line into .claude/settings.local.json");

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
