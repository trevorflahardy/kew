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

const CLAUDE_MD_TEMPLATE: &str = r#"# kew — Local Agent Orchestration

kew runs local LLM agents alongside Claude Code. Delegate background research, code generation, testing, and doc tasks to kew rather than doing everything inline.

## MCP Tools (prefer these over CLI)

kew is registered as an MCP server at project init. Call these directly:

| Tool | What it does |
|------|-------------|
| `kew_run` | Run a prompt through a specialist agent; blocks and returns the result |
| `kew_list_agents` | List all agents with trigger keywords |
| `kew_context_set` | Store text under a key for later tasks to load |
| `kew_context_get` | Retrieve stored text by key |
| `kew_context_search` | Semantic search over stored context (vector similarity) |
| `kew_status` | Pending/running/done task counts and DB stats |
| `kew_doctor` | Check Ollama connectivity and available models |

## Spawning Agents

Pass `agent` explicitly, or let keyword routing pick one automatically:

```jsonc
// explicit
{ "prompt": "Refactor auth.rs to use the new session type", "agent": "developer" }

// auto-routed — 'debug' triggers the debugger agent
{ "prompt": "Debug why the lock is deadlocking in pool.rs" }
```

**Auto-routing keywords** (omit `agent` and these phrases trigger the right specialist):

| Agent | Trigger keywords |
|-------|-----------------|
| `developer` | implement, build this, write code, add feature, refactor, create a function/struct/class |
| `debugger` | debug, broken, not working, crash, root cause, diagnose, fix the bug, why is |
| `docs-writer` | document, write docs, add docs, explain this, write readme |
| `security` | security, vulnerability, exploit, injection, auth bypass, cve |
| `doc-audit` | doc audit, documentation gap, documentation quality, missing docs, audit doc |
| `tester` | write test, add test, unit test, test coverage, test suite, write specs |
| `watcher` | watch, track progress, summarize progress, what's happening, status report, observe |
| `error-finder` | find error, potential bug, what could go wrong, pre-emptive, review for bug, find bug |

Run `kew agent list` or call `kew_list_agents` to see all agents including project-local overrides.

## CLI Patterns

```bash
# Run and wait — stdout goes directly to Claude
kew run --agent developer --wait "Implement a retry wrapper for the HTTP client"

# Fire-and-forget — returns task ID immediately
kew run --agent tester "Add tests for the auth module"

# Sequential chain — each step's output becomes context for the next
kew chain \
  --step "Analyze error handling in src/worker/" \
  --step "Write tests that cover the gaps found above"

# Prompt from file
kew run --agent docs-writer --wait --file src/db/tasks.rs

# Store result for later tasks
kew run --agent developer --wait --share-as auth-refactor "Refactor auth.rs"

# Load stored context into a task
kew run --agent tester --wait --context auth-refactor "Write tests for the refactored auth module"
```

## Context — Shared Memory Between Tasks

```bash
kew context set   <key> "content"   # store
kew context get   <key>             # retrieve
kew context search "semantic query" # vector similarity search
kew context list                    # list all entries
kew context delete <key>
```

Results stored with `--share-as` are automatically retrievable via `--context` or `kew_context_get`.

## Streaming Multiple Requests into kew

When asked for several things at once, map each to the right kew pattern:

| Request type | kew pattern |
|-------------|-------------|
| Independent parallel work | Multiple `kew_run` calls with different agents (fire in parallel) |
| Sequential pipeline | `kew chain --step ... --step ...` |
| Research → implement | `watcher`/`error-finder` → `share_as` key → `developer` with `context: [key]` |
| Write code → test it → check docs | `kew chain`: `developer` → `tester` → `doc-audit` |
| Answer a question about the codebase | `kew_run` with `agent: watcher`, read the result |
| Fix a bug | `kew_run` with `agent: debugger` → review output → apply with Edit |
| Update docs | `kew_run` with `agent: docs-writer`, `share_as` → review → write to file |

**Example: "add a feature and write tests"**
1. `kew_run { prompt: "...", agent: "developer", share_as: "feat" }`
2. Review output before writing to disk.
3. `kew_run { prompt: "Write tests for the feature", agent: "tester", context: ["feat"] }`
4. Review and apply.

## Claude's Role When Using kew

- **Delegate** open-ended LLM work (exploration, generation, auditing) to kew.
- **Verify** all kew output before applying it — agents can hallucinate.
- **Review code** from the `developer` agent before writing it to disk.
- **Own the final decision** on what gets committed; kew is a sub-contractor, not an authority.
- **Don't re-do** work kew already completed — retrieve it with `kew_context_get`.
- **Prefer `--wait`/blocking MCP calls** when you need the result in the same turn.

## Model Tiers

Configure named tiers in `kew_config.yaml`. Agents declare a tier; you control what model backs it:

```yaml
tiers:
  fast: gemma3:27b          # low-latency: summaries, routing, classification
  code: gemma4:26b          # code generation and debugging
  smart: claude-sonnet-4-6  # complex reasoning, architecture decisions
  embed: nomic-embed-text   # embeddings only (Ollama)
```

In agent YAMLs use `tier:` not a raw model name so swapping models only requires editing config:

```yaml
name: developer
tier: code   # resolved to model at runtime via kew_config.yaml tiers
```

Claude does not auto-select models at runtime — agents declare their tier, you map tiers to models.

## Subteams — Departments & Employees

For tasks with 3+ independent workstreams, spawn a **department lead** Claude subagent per category.
Each lead bulk-spawns kew workers. Results bubble up through shared context keys.

```
Claude (orchestrator)
├── engineering lead (Claude subagent)  →  developer × N, tester × 1, debugger × 1
├── docs lead (Claude subagent)         →  docs-writer × N, doc-audit × 1
└── security lead (Claude subagent)     →  security × 1, error-finder × 1
```

**Spawning a department lead:**

```jsonc
// Claude spawns a subagent with this prompt:
{
  "subagent_type": "general-purpose",
  "prompt": "You are the engineering lead for this task. Spawn these kew workers in parallel and collect their results:\n1. kew_run { agent: 'developer', prompt: '...', share_as: 'eng/feature' }\n2. kew_run { agent: 'tester', prompt: '...', share_as: 'eng/tests' }\nOnce done, retrieve with kew_context_get and return a combined summary."
}
```

**Context key namespacing** — use dot-prefixed department paths to avoid collisions:

| Department  | Key pattern     | Example              |
|-------------|----------------|----------------------|
| engineering | `eng/<task>`   | `eng/auth-refactor`  |
| docs        | `docs/<topic>` | `docs/api-guide`     |
| security    | `sec/<area>`   | `sec/auth-audit`     |
| qa          | `qa/<target>`  | `qa/worker-pool`     |

**When to use subteams:** 3+ independent workstreams that can run in parallel. For 1-2, prefer `kew chain` or direct parallel `kew_run` calls.

## Custom Agents

Drop a YAML file in `.kew/agents/` to override a built-in or add a new specialist:

```yaml
name: my-agent
description: Short description shown in `kew agent list`
tier: code   # use a tier name from kew_config.yaml (preferred over raw model)
system_prompt: |
  You are a ...
```

Project-local agents take precedence over built-ins with the same name.

## Health & Status

```bash
kew doctor          # Ollama reachability + available models + DB check
kew status --brief  # task queue snapshot
kew status          # full TUI dashboard
```
"#;

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

    // 5. Write CLAUDE.md
    let claude_md_path = project_dir.join("CLAUDE.md");
    if !claude_md_path.exists() || args.force {
        std::fs::write(&claude_md_path, CLAUDE_MD_TEMPLATE).context("writing CLAUDE.md")?;
        println!("\u{2713} Generated CLAUDE.md");
    } else {
        println!("  CLAUDE.md already exists (use --force to overwrite)");
    }

    // 7. Append .kew/ to .gitignore
    if !args.no_gitignore {
        append_gitignore(&project_dir)?;
    }

    // 8. Inject MCP config
    if !args.no_mcp {
        inject_mcp_config(&project_dir)?;
    }

    // 9. Inject status line into Claude Code settings
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

        let content =
            std::fs::read_to_string(dir.path().join(".claude/settings.local.json")).unwrap();
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
        let content =
            std::fs::read_to_string(dir.path().join(".claude/settings.local.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["statusLine"]["type"], "command");
        assert!(json["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("kew-statusline.sh"));
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

# Format a token count as a compact string: 0→"0", 1500→"1.5k", 1000000→"1.0M"
_fmt_tokens() {
  n="${1:-0}"
  if [ "$n" -ge 1000000 ]; then
    printf "%d.%dM" "$((n / 1000000))" "$(( (n % 1000000) / 100000 ))"
  elif [ "$n" -ge 1000 ]; then
    printf "%d.%dk" "$((n / 1000))" "$(( (n % 1000) / 100 ))"
  else
    printf "%d" "$n"
  fi
}

running=$(_get running)
pending=$(_get pending)
done_count=$(_get done)
failed=$(_get failed)
context=$(_get context)
embeddings=$(_get embeddings)
db_size=$(_get db)
prompt_tokens=$(_get prompt_tokens)
completion_tokens=$(_get completion_tokens)
agents_raw=$(_get agents)
total_tokens=$(( ${prompt_tokens:-0} + ${completion_tokens:-0} ))

parts=""
if [ "${running:-0}" -gt 0 ]; then
  if [ -n "$agents_raw" ]; then
    # Show individual agent names separated by spaces
    agent_list=$(printf '%s' "$agents_raw" | tr ',' ' ')
    parts="${parts}▶ ${agent_list} "
  else
    parts="${parts}▶ ${running} "
  fi
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
if [ "$total_tokens" -gt 0 ] 2>/dev/null; then
  parts="${parts} tok:$(_fmt_tokens "$total_tokens")"
fi
if [ -n "$db_size" ] && [ "$db_size" != "0KB" ]; then
  parts="${parts} ${db_size}"
fi

printf "◆ kew  %s" "$parts"
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

    // Add/update kew entry — use absolute path so the MCP server can be spawned
    // from any working directory (Claude Code doesn't cd into the project first).
    let db_path = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf())
        .join(".kew/kew.db");
    let db_path_str = db_path.to_string_lossy();
    settings["mcpServers"]["kew"] = serde_json::json!({
        "command": "kew",
        "args": ["mcp", "serve", "--db", db_path_str]
    });

    // Write back
    let json = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, format!("{json}\n"))?;
    println!("\u{2713} Injected kew MCP server into .claude/settings.local.json");

    Ok(())
}
