# kew

Local agent orchestration. Kew is a single Rust binary that runs LLM agents — local via Ollama or cloud via Claude — coordinates them through SQLite, and integrates with Claude Code as a CLI tool and MCP server.

The core loop is simple: a worker claims a task, calls an LLM, stores the result. Every result gets embedded automatically, so later tasks can pull relevant context via vector search without you managing it explicitly.

---

## Quick start

```bash
# Requires Ollama running locally
ollama pull gemma3:27b
cargo build --release
./target/release/kew run -m gemma3:27b -w "Write a prime checker in Python"
```

For Claude:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
kew run -m claude-sonnet-4-20250514 -w "Explain Rust lifetimes in 3 sentences"
```

Models starting with `claude-` route to the Anthropic API; everything else goes to Ollama.

---

## How it works

Each task follows this path:

1. Task arrives (CLI, MCP, or chain step)
2. Worker atomically claims it via `UPDATE...RETURNING` — no double-claiming
3. Context loads: explicit keys + optional vector similarity search over past results
4. File locks acquired if specified
5. LLM called via `reqwest` to Ollama `/api/chat` or Claude Messages API
6. Result stored: status, output, token counts, duration — all in SQLite
7. Result embedded with `nomic-embed-text` for future retrieval
8. Locks released

Workers are tokio tasks in a pool (default: 4 concurrent), not OS processes. No IPC overhead, no daemon.

### SQLite as coordination bus

One file: `.kew/kew.db`. WAL mode, survives crashes, inspectable with `sqlite3`.

- `tasks` — work queue with atomic claiming. States: `pending → assigned → running → done/failed/cancelled`
- `context` — key-value store for inter-agent knowledge sharing
- `file_locks` — TTL-based locks preventing concurrent file edits
- `embeddings` — 768-dim float vectors (cosine similarity computed in Rust)

### Vector search / RAG

Every completed task result is embedded. New tasks with `--auto-context` search past results by cosine similarity and inject the top matches as context. No external vector database — just SQLite BLOBs and a Rust cosine similarity function.

```bash
kew context set "auth-design" "We use JWT tokens with 15-minute expiry..."
kew context search "how does authentication work?" --top-k 5
kew run -m gemma3:27b -w "Refactor the auth middleware" --auto-context
```

---

## Chains

Sequential execution where each step's output feeds into the next:

```bash
kew chain \
  --step "Analyze the current auth module:gemma3:27b" \
  --step "Write a refactoring plan:claude-sonnet-4-20250514" \
  --step "Generate the refactored code:claude-sonnet-4-20250514"
```

Each step's result is stored as `{chain_id}-step-{N}` and loaded by the following step. The chain stops on first failure.

---

## MCP server

Kew runs as a Model Context Protocol server, letting Claude Code call it directly:

```json
{
  "mcpServers": {
    "kew": {
      "command": "kew",
      "args": ["mcp", "serve"],
      "env": { "ANTHROPIC_API_KEY": "sk-ant-..." }
    }
  }
}
```

Available tools:

| Tool                 | Description                                              |
| -------------------- | -------------------------------------------------------- |
| `kew_run`            | Execute a prompt through any model, returns result       |
| `kew_context_get`    | Read a shared context entry by key                       |
| `kew_context_set`    | Write a shared context entry                             |
| `kew_context_search` | Vector similarity search over stored knowledge           |
| `kew_status`         | Task counts, context entries, embedding stats            |
| `kew_doctor`         | Health check — Ollama reachable, models available, DB ok |
| `kew_list_agents`    | List available agents with keyword hints                 |

All tools are blocking. The server uses `rmcp` (official Rust MCP SDK) with stdio transport.

---

## CLI reference

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
    --no-mcp                  Skip MCP config injection
    --no-statusline           Skip status line setup
    --no-gitignore            Skip .gitignore update
    --model <model>           Default model for generated config
```

Output modes for `kew run`:

- `--wait`: raw LLM output to stdout — what Claude Code reads via Bash
- `--json`: `{ task_id, status, result, duration_ms, prompt_tokens, completion_tokens }`
- `--porcelain`: single-line `key=value` pairs for shell scripts and status bars
- Default: spinner while running, formatted result with colors

---

## Status line

After `kew init`, Claude Code shows a live status bar:

```
◆ kew  ▶ 2 ⏳3 ✓15 ✗1  ctx:8 emb:42 tok:14.2k  4.1MB
```

Fields: running tasks, pending tasks, done, failed, context entries, embeddings, total tokens used by agents, DB size. Token count accumulates across all completed tasks so you can see the running cost of agent work in the session.

---

## Project initialization

```bash
kew init
```

Creates `.kew/` with the SQLite database, scaffolds `kew_config.yaml`, injects MCP server config into `.claude/settings.local.json`, installs the status line script, and adds `.kew/` to `.gitignore`.

---

## Agents

Built-in agents (YAML configs compiled into the binary):

| Agent          | Role                           |
| -------------- | ------------------------------ |
| `developer`    | Production code writer         |
| `debugger`     | Systematic root-cause analysis |
| `docs-writer`  | Documentation                  |
| `security`     | Vulnerability auditor          |
| `doc-audit`    | Documentation gap finder       |
| `tester`       | Test suite writer              |
| `watcher`      | Progress tracker               |
| `error-finder` | Adversarial bug detector       |

Override or add agents by dropping YAML files in `.kew/agents/<name>.yaml` (project-local) or `~/.config/kew/agents/<name>.yaml` (user-global).

---

## File locking

```bash
kew run -m gemma3:27b -w "Refactor auth" --lock src/auth.rs
# Another agent trying to lock the same file fails immediately
```

Locks are TTL-based (default 600s), released on task completion (success or failure), and auto-expire.

---

## Technology

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

Feature flags:

```toml
[features]
default = ["tui", "mcp", "vectors"]
tui = ["dep:ratatui", "dep:crossterm"]
mcp = ["dep:rmcp", "dep:schemars"]
vectors = ["dep:zerocopy"]
```

Build without optional features: `cargo build --release --no-default-features`

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
