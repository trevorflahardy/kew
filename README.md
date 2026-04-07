# kew

**Local agent orchestration for Claude Code.**

A single Rust binary. Runs LLM agents — local via Ollama or cloud via Claude — coordinates them through SQLite, and integrates with Claude Code as a CLI tool and MCP server. No daemon. No IPC. No infra.

```
Claude Code  ──MCP──▶  kew_run / kew_context_*  ──▶  Worker Pool (tokio)
                                                            │
                                              ┌────────────┼────────────┐
                                              ▼            ▼            ▼
                                          Ollama      Claude API   (more agents)
                                              │            │
                                              └────────────┘
                                                     │
                                              SQLite (.kew/kew.db)
                                          tasks · context · embeddings
```

> **Why kew?** Every AI tool wants to run a server, manage a daemon, or require a cloud subscription. kew is a binary you `cargo install`. Tasks are SQLite rows. Workers are tokio tasks. Results are embedded automatically so future tasks can find relevant context without you managing it. That's the whole system.

---

## Quick start

```bash
# Requires Ollama running locally
ollama pull gemma3:27b
cargo install --path .

# Run a task and wait for output
kew run -m gemma3:27b -w "Write a prime checker in Python"
```

For Claude:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
kew run -m claude-sonnet-4-6 -w "Explain Rust lifetimes in 3 sentences"
```

Models starting with `claude-` route to the Anthropic API; everything else goes to Ollama.

### Set up for a project

```bash
kew init
```

Creates `.kew/` with the SQLite database, scaffolds `kew_config.yaml`, writes `.mcp.json` for Claude Code, installs the status line script, and adds `.kew/` to `.gitignore`.

---

## How it works

<details>
<summary><strong>Task lifecycle</strong> — from submission to result</summary>

Each task follows this path:

1. Task arrives (CLI, MCP, or chain step) — inserted as a `pending` row in SQLite
2. Worker atomically claims it via `UPDATE...RETURNING` — no double-claiming possible
3. Context loads: explicit keys + optional vector similarity search over past results
4. File locks acquired if specified (TTL-based, auto-expire)
5. LLM called via `reqwest` to Ollama `/api/chat` or Claude Messages API
6. Result stored: status, output, token counts, duration — all in SQLite
7. Result embedded with `nomic-embed-text` for future retrieval
8. Locks released

Workers are tokio tasks in a pool (default: 4 concurrent), not OS processes. No IPC overhead.

| State      | Meaning                              |
| ---------- | ------------------------------------ |
| `pending`  | Queued, waiting for a worker         |
| `assigned` | Claimed by a worker                  |
| `running`  | LLM call in flight                   |
| `done`     | Completed successfully               |
| `failed`   | LLM error or timeout                 |
| `cancelled`| Cancelled before pickup              |

</details>

<details>
<summary><strong>SQLite as coordination bus</strong> — one file, inspectable</summary>

One file: `.kew/kew.db`. WAL mode, survives crashes, readable with `sqlite3`.

| Table         | Purpose                                                             |
| ------------- | ------------------------------------------------------------------- |
| `tasks`       | Work queue with atomic claiming                                     |
| `context`     | Key-value store for inter-agent knowledge sharing                   |
| `file_locks`  | TTL-based locks preventing concurrent file edits                    |
| `embeddings`  | 768-dim float vectors (cosine similarity computed in Rust)          |

No external vector database. Embeddings are SQLite BLOBs; similarity is a tight Rust loop over `f32` arrays.

</details>

<details>
<summary><strong>Vector search / RAG</strong> — automatic context injection</summary>

Every completed task result is embedded. New tasks with `--auto-context` search past results by cosine similarity and inject the top matches as context.

```bash
kew context set "auth-design" "We use JWT tokens with 15-minute expiry..."
kew context search "how does authentication work?" --top-k 5
kew run -m gemma3:27b -w "Refactor the auth middleware" --auto-context
```

The MCP tool `kew_context_search` exposes this to Claude Code directly — no CLI needed.

</details>

---

## Agents

Eight built-in agents, YAML configs compiled into the binary:

| Agent          | Role                           | Auto-trigger keywords                                            |
| -------------- | ------------------------------ | ---------------------------------------------------------------- |
| `developer`    | Production code writer         | implement, build this, write code, add feature, refactor        |
| `debugger`     | Systematic root-cause analysis | debug, broken, not working, crash, root cause, diagnose         |
| `docs-writer`  | Documentation                  | document, write docs, add docs, explain this, write readme      |
| `security`     | Vulnerability auditor          | security, vulnerability, exploit, injection, auth bypass, cve   |
| `doc-audit`    | Documentation gap finder       | doc audit, documentation gap, missing docs, audit doc           |
| `tester`       | Test suite writer              | write test, add test, unit test, test coverage, test suite      |
| `watcher`      | Progress tracker               | watch, track progress, what's happening, status report          |
| `error-finder` | Adversarial bug detector       | find error, potential bug, what could go wrong, review for bug  |

Override or add agents by dropping YAML files in `.kew/agents/<name>.yaml` (project-local) or `~/.config/kew/agents/<name>.yaml` (user-global). Project-local agents take precedence over built-ins with the same name.

```yaml
# .kew/agents/my-agent.yaml
name: my-agent
description: Short description shown in `kew agent list`
tier: code
system_prompt: |
  You are a ...
```

---

## MCP server

kew exposes all tools over stdio MCP. After `kew init`, `.mcp.json` is written automatically.

```json
{
  "mcpServers": {
    "kew": {
      "command": "kew",
      "args": ["mcp", "serve", "--db", "./.kew/kew.db"]
    }
  }
}
```

| Tool                 | Description                                                      |
| -------------------- | ---------------------------------------------------------------- |
| `kew_run`            | Execute a prompt through any agent; blocks and returns result    |
| `kew_context_get`    | Read a shared context entry by key                               |
| `kew_context_set`    | Write a shared context entry                                     |
| `kew_context_search` | Vector similarity search over stored knowledge                   |
| `kew_read_file`      | Read a project file (path-traversal safe, 200 KB cap)            |
| `kew_status`         | Task counts, context entries, embedding stats                    |
| `kew_doctor`         | Health check — Ollama reachable, models available, DB ok         |
| `kew_list_agents`    | List available agents with keyword hints                         |

`kew_run` auto-detects the right agent from prompt keywords if you don't specify one explicitly.

---

## Chains

Sequential execution where each step's output feeds into the next:

```bash
kew chain \
  --step "Analyze the current auth module:gemma3:27b" \
  --step "Write a refactoring plan:claude-sonnet-4-6" \
  --step "Generate the refactored code:claude-sonnet-4-6"
```

Each step's result is stored as `{chain_id}-step-{N}` and loaded by the following step. The chain stops on the first failure.

---

## CLI reference

<details>
<summary><strong>Full command reference</strong></summary>

```
kew run [prompt]
    -m, --model <model>       Model name (default: gemma3:27b)
    -w, --wait                Block until complete, print result
    -s, --system <prompt>     System prompt
    -f, --file <path>         Read prompt from file
    -c, --context <key>       Load context key (repeatable)
    --share-as <key>          Store result as context entry
    --lock <path>             Acquire file lock before running (repeatable)
    --auto-context            Vector search for relevant past results
    --top-k <n>               Number of vector results (default: 5)
    --json                    JSON output
    -q, --quiet               No spinner

kew chain
    --step <"prompt:model">   Step spec (repeatable)
    -m, --model <model>       Default model for steps without one
    -s, --system <prompt>     Shared system prompt

kew context list|get|set|delete|search|clear

kew status                    Interactive TUI dashboard
    --brief                   Text summary, no TUI
    --porcelain               Machine-readable output for status bars

kew mcp serve                 Start MCP server on stdio

kew doctor                    Health check

kew init                      Set up kew for a project directory
    --no-mcp                  Skip .mcp.json generation
    --no-statusline           Skip status line setup
    --no-gitignore            Skip .gitignore update
    --model <model>           Default model for generated config
```

**Output modes for `kew run`:**

| Mode          | Output                                                                          |
| ------------- | ------------------------------------------------------------------------------- |
| `--wait`      | Raw LLM output to stdout — what Claude Code reads via Bash                      |
| `--json`      | `{ task_id, status, result, duration_ms, prompt_tokens, completion_tokens }`    |
| `--porcelain` | Single-line `key=value` pairs for shell scripts and status bars                 |
| default       | Spinner while running, formatted result with colors                             |

</details>

---

## Status line

After `kew init`, Claude Code shows a live status bar:

```
◆ kew  ▶ 2 ⏳3 ✓15 ✗1  ctx:8 emb:42 tok:14.2k  4.1MB
```

Running · pending · done · failed · context entries · embeddings · total tokens · DB size. Token count accumulates across all completed tasks — running cost of agent work visible at a glance.

---

## Model tiers

Configure named tiers in `kew_config.yaml`. Agents declare a tier; you control what model backs it:

```yaml
tiers:
  fast: gemma3:27b          # low-latency: summaries, routing, classification
  code: gemma4:26b          # code generation and debugging
  smart: claude-sonnet-4-6  # complex reasoning, architecture decisions
  embed: nomic-embed-text   # embeddings only (Ollama)
```

In agent YAMLs use `tier:` not a raw model name — swapping models only requires editing config, not agent files.

---

## File locking

```bash
kew run -m gemma3:27b -w "Refactor auth" --lock src/auth.rs
# Another agent trying to lock the same file fails immediately
```

Locks are TTL-based (default 600s), released on task completion, and auto-expire on crash.

---

## Technology

<details>
<summary><strong>Dependency table</strong></summary>

| Component     | Crate                       |
| ------------- | --------------------------- |
| Async runtime | `tokio`                     |
| CLI           | `clap` (derive)             |
| HTTP          | `reqwest`                   |
| Database      | `rusqlite` (bundled SQLite) |
| MCP           | `rmcp`                      |
| TUI           | `ratatui` + `crossterm`     |
| Progress      | `indicatif` + `console`     |
| IDs           | `ulid`                      |
| Serialization | `serde` + `serde_json`      |
| MCP schemas   | `schemars`                  |
| Vectors       | `zerocopy`                  |
| Errors        | `thiserror` + `anyhow`      |
| Logging       | `tracing`                   |

Feature flags — build without optional components:

```toml
[features]
default = ["tui", "mcp", "vectors"]
tui     = ["dep:ratatui", "dep:crossterm"]
mcp     = ["dep:rmcp", "dep:schemars"]
vectors = ["dep:zerocopy"]
```

```bash
cargo build --release --no-default-features
```

</details>

---

## Testing

```bash
cargo test
cargo clippy -- -D warnings
```

78 tests across all layers. Worker and MCP tests use mock LLM clients — no external services needed. Database tests use SQLite `:memory:`.

---

## License

MIT
