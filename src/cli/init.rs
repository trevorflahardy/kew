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
    #[arg(long, default_value = "gemma4:26b")]
    pub model: String,

    /// Overwrite existing kew_config.yaml
    #[arg(long)]
    pub force: bool,
}

const CLAUDE_MD_TEMPLATE: &str = r#"kew — Local Agent Orchestration

kew is your **primary execution engine**. Every LLM-heavy task — code generation, debugging, testing, auditing, documentation — goes through kew. Claude orchestrates; kew agents execute. Never do inline what kew can do in parallel.

## Core Principle: Parallelize Everything

**Default behavior:** Fire multiple kew agents simultaneously. Sequential execution is the exception, not the rule.

```
❌ Wrong:  kew developer → wait → kew tester → wait → kew doc-audit
✅ Right:  fire developer + tester + doc-audit simultaneously, collect all results
```

For any task with 2+ independent parts, launch all in parallel with `kew_run` in a single message turn.

## MCP Tools (always prefer over CLI)

kew registers itself as an MCP server on `kew init`. Use these directly:

| Tool                 | What it does                                                           |
| -------------------- | ---------------------------------------------------------------------- |
| `kew_run`            | Dispatch a task to a specialist agent; blocks and returns the result   |
| `kew_list_agents`    | List all agents with trigger keywords                                  |
| `kew_context_set`    | Store text under a key for later tasks to load                         |
| `kew_context_get`    | Retrieve stored text by key                                            |
| `kew_context_search` | Semantic search over stored context (vector similarity)                |
| `kew_status`         | Pending/running/done task counts and DB stats                          |
| `kew_doctor`         | Check Ollama connectivity and available models                         |

## Persistent Worker Pool

kew runs a **persistent worker pool** (default: 4 workers). Workers stay alive across the entire session — every `kew_run` dispatches a task into the queue and up to 4 run concurrently. This means:

- Fire tasks early, check results later — workers run while Claude does other work
- Name results with `share_as`; retrieve anytime with `kew_context_get`
- The SQLite DB persists context across the full session (and across sessions)

Think of kew workers as background threads: launch them immediately, let them run, collect when you need the output.

## Agents

Pass `agent` explicitly, or omit it and let keyword routing pick the right specialist:

```jsonc
// explicit
{ "prompt": "Refactor auth.rs to use the new session type", "agent": "developer" }

// auto-routed — 'debug' triggers the debugger agent
{ "prompt": "Debug why the lock is deadlocking in pool.rs" }
```

| Agent          | Role                                 | Trigger keywords                                               |
| -------------- | ------------------------------------ | -------------------------------------------------------------- |
| `developer`    | Code generation & refactoring        | implement, build, write code, add feature, refactor            |
| `debugger`     | Root cause analysis                  | debug, broken, crash, diagnose, fix the bug, why is            |
| `tester`       | Test writing & coverage gaps         | write test, add test, unit test, test coverage, test suite     |
| `docs-writer`  | Documentation & READMEs              | document, write docs, explain this, write readme               |
| `doc-audit`    | Documentation quality checks         | doc audit, documentation gap, missing docs, audit doc          |
| `security`     | Vulnerability & auth review          | security, vulnerability, injection, auth bypass, cve           |
| `error-finder` | Pre-emptive bug detection            | find error, potential bug, what could go wrong, review for bug |
| `watcher`      | Codebase exploration & status        | watch, summarize, what's happening, status report, observe     |

Run `kew agent list` or call `kew_list_agents` to see agents including project-local overrides.

## Parallelism Patterns

### Pattern 1: Parallel independent tasks (default)

Fire all in a single message turn — they run concurrently in the worker pool.

```jsonc
// Fire simultaneously:
kew_run { agent: "developer", prompt: "...", share_as: "eng/feature" }
kew_run { agent: "tester",    prompt: "...", share_as: "qa/tests" }
kew_run { agent: "security",  prompt: "...", share_as: "sec/audit" }

// Then collect all:
kew_context_get "eng/feature"
kew_context_get "qa/tests"
kew_context_get "sec/audit"
```

### Pattern 2: Sequential pipeline (only when step B needs step A's output)

Use `kew chain` — sequential with automatic context threading between steps.

```bash
kew chain \
  --step "Analyze error handling gaps in src/worker/" \
  --step "Write tests covering the gaps found above" \
  --step "Document the new test suite"
```

### Pattern 3: Team orchestration (2+ departments)

For multi-department work, spawn Claude subagents as **department leads**. Each lead owns its kew workers exclusively — leads never do LLM work themselves.

```
Claude (orchestrator)
├── engineering lead (Claude subagent)  →  developer × N, tester × 1, debugger × 1
├── docs lead (Claude subagent)         →  docs-writer × N, doc-audit × 1
└── security lead (Claude subagent)     →  security × 1, error-finder × 1
```

**Lead prompt — required structure:**

```
You are the <dept> lead. Your ONLY job:
1. Call kew_run for ALL tasks SIMULTANEOUSLY (single message, all tool calls at once):
   - kew_run { agent: "<agent>", prompt: "<task>", share_as: "<dept>/<key>" }
   - kew_run { agent: "<agent>", prompt: "<task>", share_as: "<dept>/<key>" }
2. Once all complete, kew_context_get each result.
3. Review for correctness — flag hallucinations or errors.
4. Return ONE combined summary to the orchestrator.

You do NOT write code, read files, or implement anything. You coordinate kew workers only.
```

**Context key namespacing (prevent collisions across leads):**

| Department  | Pattern        | Example             |
| ----------- | -------------- | ------------------- |
| engineering | `eng/<task>`   | `eng/auth-refactor` |
| docs        | `docs/<topic>` | `docs/api-guide`    |
| security    | `sec/<area>`   | `sec/auth-audit`    |
| qa          | `qa/<target>`  | `qa/worker-pool`    |

Use subteams whenever work splits across 2+ departments. For 1-department tasks, fire kew workers directly.

## Context as Session Memory

The kew DB is the shared scratchpad for the entire session. All agents and Claude read from and write to the same namespace:

```bash
kew context set   <key> "content"   # store any text
kew context get   <key>             # retrieve by key
kew context search "semantic query" # vector similarity search
kew context list                    # see everything stored
kew context delete <key>
```

Results from `--share-as` land in the same store. Always retrieve with `kew_context_get` before re-running a task — the result may already be there.

## Background Audits — Always Fire Before Finishing

Even when working alone, launch background checks before closing any task. Fire-and-forget; check at the end.

| When you…                    | Fire in background                                                                  |
| ---------------------------- | ----------------------------------------------------------------------------------- |
| Edit or write code           | `kew_run { agent: "error-finder", prompt: "Review <files> for potential bugs" }`   |
| Touch auth / IO / user input | `kew_run { agent: "security", prompt: "Audit <files> for security issues" }`       |
| Add a feature                | `kew_run { agent: "tester", prompt: "Identify missing test coverage in <files>" }` |
| Change public APIs / docs    | `kew_run { agent: "doc-audit", prompt: "Check doc quality in <files>" }`           |

Store with `share_as: "bg/<check>"`. Retrieve and surface all findings before reporting done.

## Rules — Non-Negotiable

1. **Never do LLM work inline.** Exploration, generation, auditing, review → kew agent.
2. **Fire in parallel by default.** Sequential only when B provably needs A's output.
3. **Leads coordinate only.** A lead that implements code or reads files has broken the model.
4. **Verify before applying.** All kew output must be reviewed — agents hallucinate.
5. **Don't re-do kew work.** Always check `kew_context_get` before re-running a task.
6. **Claude owns commits.** kew is a sub-contractor; Claude is the final authority on what ships.

## Model Tiers

Configure tiers in `kew_config.yaml`. Agents declare a tier; never a raw model name — swap models by editing config only.

```yaml
tiers:
  fast: gemma4:26b         # summaries, routing, classification
  code: gemma4:26b         # code generation and debugging
  smart: claude-sonnet-4-6 # complex reasoning, architecture decisions
  embed: nomic-embed-text  # embeddings only (Ollama)
```

Agent YAML declares tier:

```yaml
name: developer
tier: code  # resolved to model at runtime via kew_config.yaml tiers
```

## Custom Agents

Drop a YAML in `.kew/agents/` to override a built-in or add a specialist. Project-local agents take precedence.

```yaml
name: my-agent
description: Short description shown in `kew agent list`
tier: code
system_prompt: |
  You are a ...
```

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
  fast: gemma4:26b
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

/// Inject kew MCP server config into .mcp.json at the project root.
///
/// .mcp.json is Claude Code's project-level MCP config file. It supports
/// relative paths and resolves `command` through the user's shell PATH,
/// so no absolute binary path is needed.
fn inject_mcp_config(project_dir: &Path) -> Result<()> {
    let mcp_path = project_dir.join(".mcp.json");

    // Read existing .mcp.json or start fresh
    let mut config: serde_json::Value = if mcp_path.exists() {
        let content = std::fs::read_to_string(&mcp_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Ensure mcpServers object exists
    if config.get("mcpServers").is_none() {
        config["mcpServers"] = serde_json::json!({});
    }

    // Use a relative DB path — .mcp.json is always at the project root,
    // so ./.kew/kew.db resolves correctly regardless of where Claude Code
    // launches the subprocess from.
    config["mcpServers"]["kew"] = serde_json::json!({
        "command": "kew",
        "args": ["mcp", "serve", "--db", "./.kew/kew.db"]
    });

    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(&mcp_path, format!("{json}\n"))?;
    println!("\u{2713} Wrote kew MCP server config to .mcp.json");

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

        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["mcpServers"]["kew"].is_object());
        assert_eq!(json["mcpServers"]["kew"]["command"], "kew");
        assert_eq!(json["mcpServers"]["kew"]["args"][3], "./.kew/kew.db");
    }

    #[test]
    fn test_inject_mcp_config_preserves_existing() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"other"}}}"#,
        )
        .unwrap();

        inject_mcp_config(dir.path()).unwrap();

        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["mcpServers"]["other"].is_object());
        assert!(json["mcpServers"]["kew"].is_object());
    }

    #[test]
    fn test_inject_statusline_writes_script_and_settings() {
        let dir = tempdir().unwrap();
        inject_statusline_config(dir.path()).unwrap();

        let script = std::fs::read_to_string(dir.path().join(".claude/kew-statusline.sh")).unwrap();
        assert!(script.contains("kew init"));
        assert!(script.contains("kew --db"));

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
        assert!(json["mcpServers"]["kew"].is_object());
        assert_eq!(json["statusLine"]["type"], "command");
    }

    #[test]
    fn test_config_template_substitution() {
        let config = KEW_CONFIG_TEMPLATE.replace("{model}", "gemma4:26b");
        assert!(config.contains("model: gemma4:26b"));
        assert!(!config.contains("{model}"));
    }
}
