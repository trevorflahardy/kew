# 🎯 Kew

**Real local agent orchestration. No theater.**

Kew is a single Rust binary that spawns real LLM agents — local (Ollama) and cloud (Claude) — coordinates them via SQLite, learns from past work through embedded vector search, and integrates with Claude Code as both a CLI tool and MCP server.

Every other multi-agent framework builds the *bookkeeping* — topologies, consensus protocols, JSON state files — without building the **process**: a worker that calls an LLM and returns results. Kew skips the theater and does the work.

---

## ✨ What Makes Kew Different

- 🔧 **Phase 2 calls a real LLM.** Not phase 8. Not "future work."
- 🧪 **Zero config to start.** Install Ollama, install kew, run. No daemon, no config file, no setup wizard.
- 📦 **Single Rust binary.** ~10MB, instant startup, no runtime dependencies.
- 🧠 **Automatic learning.** Every completed task result gets embedded. Future tasks benefit via vector similarity search. No explicit "training."
- 🗄️ **SQLite is the bus.** One file. Survives crashes. Inspectable with `sqlite3`. WAL mode for concurrent access.
- 🔌 **MCP native.** Claude Code can call kew tools directly via the Model Context Protocol.

---

## 🚀 Quick Start

```bash
# 1. Have Ollama running with a model pulled
ollama pull gemma3:27b

# 2. Build kew
cargo build --release

# 3. Run your first agent
./target/release/kew run -m gemma3:27b -w "Write a prime checker in Python"
```

That's it. Real code comes back from a real LLM. No config files, no daemon processes, no API keys (for local models).

### 🔑 Using Claude (Cloud)

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
kew run -m claude-sonnet-4-20250514 -w "Explain Rust lifetimes in 3 sentences"
```

Kew automatically routes models starting with `claude-` to the Anthropic API and everything else to Ollama.

---

## 📖 How It Works

### 🏗️ The Architecture

Kew is built around a simple pipeline that every task follows:

1. **Task arrives** — via CLI, MCP tool, or chain step
2. **Worker claims it** — atomic SQLite `UPDATE...RETURNING` prevents double-claiming
3. **Context loads** — explicit key-value entries + optional vector similarity search over past results
4. **File locks acquired** — prevents concurrent agents from editing the same files
5. **LLM gets called** — `reqwest` POST to Ollama `/api/chat` or Claude Messages API
6. **Result stored** — status, output, token counts, duration all recorded in SQLite
7. **Result shared** — optionally stored as named context for other tasks; always embedded for future retrieval
8. **Locks released** — other agents can now access those files

### 🧵 Worker Pool

Workers are **tokio tasks**, not OS processes. The pool uses `mpsc` channels to distribute work across N concurrent workers (default 4). Each worker independently claims tasks, calls LLMs, and stores results. The pool runs entirely in-process with zero IPC overhead.

### 🗄️ SQLite as Coordination Bus

Everything lives in one SQLite file (`.kew/kew.db`). WAL mode enables concurrent reads while writes are serialized. The schema includes:

- **`tasks`** — The work queue. Status enum: `pending → assigned → running → done/failed/cancelled`. Atomic claiming via `UPDATE...RETURNING` ensures no task runs twice.
- **`context`** — Key-value store for shared knowledge between agents. Namespaced, with metadata tracking who created each entry.
- **`file_locks`** — Prevents two agents from editing the same file simultaneously. TTL-based with automatic expiry cleanup.
- **`embeddings`** — Vector storage for semantic search. 768-dimensional float vectors (from `nomic-embed-text`) stored as BLOBs, with cosine similarity computed in Rust.

### 🧠 Learning via Vector Search

Every completed task result is automatically embedded using Ollama's `/api/embed` endpoint (model: `nomic-embed-text`, 768 dimensions). These embeddings are stored alongside the result in SQLite.

When a new task uses `--auto-context`, kew searches past results by cosine similarity and injects the most relevant ones as context. This is **RAG over your own work product** — no fine-tuning, no external vector database, just embedded retrieval that gets more useful the more you use kew.

```bash
# Embed and search manually
kew context set "auth-design" "We use JWT tokens with 15-minute expiry..."
kew context search "how does authentication work?" --top-k 5

# Or let it happen automatically
kew run -m gemma3:27b -w "Refactor the auth middleware" --auto-context
```

---

## 🔗 Chains

Sequential multi-step execution where each step's output becomes the next step's context:

```bash
kew chain \
  --step "Analyze the current auth module:gemma3:27b" \
  --step "Write a refactoring plan based on the analysis:claude-sonnet-4-20250514" \
  --step "Generate the refactored code:claude-sonnet-4-20250514"
```

Each step automatically shares its result as `{chain_id}-step-{N}`, and the next step loads the previous step's output. If any step fails, the chain stops immediately.

---

## 🔌 MCP Server

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

### Available MCP Tools

| Tool | What it does |
|------|-------------|
| 🏃 `kew_run` | Execute a prompt through any model, get the result back |
| 📖 `kew_context_get` | Read a shared context entry by key |
| ✏️ `kew_context_set` | Write a shared context entry |
| 🔍 `kew_context_search` | Vector similarity search over stored knowledge |
| 📊 `kew_status` | Task counts, context entries, embedding stats |
| 🩺 `kew_doctor` | Health check — is Ollama running? Are models available? |

All tools are blocking — Claude Code calls them and gets results back in the same turn. The MCP server uses `rmcp` (the official Rust MCP SDK) with stdio transport.

---

## 🖥️ CLI Reference

```
kew run [prompt]                          Execute a task through an LLM
    -m, --model <model>                   Model name (default: gemma3:27b)
    -w, --wait                            Block until complete
    -s, --system <prompt>                 System prompt
    -f, --file <path>                     Read prompt from file
    -c, --context <key>                   Load context key (repeatable)
    --share-as <key>                      Store result as context
    --lock <path>                         Acquire file lock (repeatable)
    --auto-context                        Vector search for relevant context
    --top-k <n>                           Number of vector results (default: 5)
    --json                                JSON output
    -q, --quiet                           No spinner

kew chain                                 Sequential multi-step execution
    --step <"prompt:model">               Step spec (repeatable)
    -m, --model <model>                   Default model for steps
    -s, --system <prompt>                 Shared system prompt

kew context list|get|set|delete|search|clear
                                          Manage shared context

kew status                                Interactive TUI dashboard (ratatui)
    --brief                               Text summary, no TUI
    --porcelain                           Machine-readable for status bars

kew mcp serve                             Start MCP server (stdio)

kew doctor                                Health check

kew init                                  Initialize kew for a project
    --no-mcp                              Skip MCP config injection
    --no-statusline                       Skip status line setup
    --no-gitignore                        Skip .gitignore modification
    --model <model>                       Default model for config
```

### 📤 Output Modes

- **`--wait` mode**: Raw LLM output to stdout. No decoration. This is what Claude Code reads via Bash.
- **`--json` mode**: Structured JSON with task_id, status, result, duration_ms, token counts.
- **Interactive**: `indicatif` spinner while working, then formatted result with `console` colors.
- **`--porcelain`**: Single-line `key=value` pairs for shell scripts and status bars.

---

## 📊 TUI Dashboard

`kew status` launches an interactive terminal dashboard built with `ratatui` + `crossterm`:

- 📈 Summary bar with task counts by status
- 📋 Task table with colored status indicators, models, durations
- ⌨️ Press `q` to quit, auto-refreshes every second

Use `kew status --brief` for a quick text summary without entering the TUI.

---

## ⚙️ Technology Stack

| Component | Crate | Why |
|-----------|-------|-----|
| 🔄 Async runtime | `tokio` | Powers the worker pool, reqwest, and MCP server |
| 🖥️ CLI | `clap` (derive) | Ergonomic argument parsing with derive macros |
| 🌐 HTTP | `reqwest` | Async HTTP for Ollama and Claude API calls |
| 🗄️ Database | `rusqlite` (bundled) | SQLite compiled into the binary, zero external deps |
| 📡 MCP | `rmcp` | Official Rust MCP SDK with stdio transport |
| 🖼️ TUI | `ratatui` + `crossterm` | Terminal dashboard with sub-ms rendering |
| ⏳ Progress | `indicatif` + `console` | Spinners, progress bars, and colored output |
| 🔢 IDs | `ulid` | Time-sortable unique IDs, no coordination needed |
| 📝 Serialization | `serde` + `serde_json` | Standard Rust serialization throughout |
| 🔐 Schemas | `schemars` | JSON Schema generation for MCP tool parameters |
| ⚡ Vectors | `zerocopy` | Zero-copy f32 vector handling for embeddings |
| ❌ Errors | `thiserror` + `anyhow` | Typed errors in libraries, ergonomic errors in CLI |
| 📋 Logging | `tracing` | Structured, async-aware logging with env filter |

### 🏗️ Feature Flags

```toml
[features]
default = ["tui", "mcp", "vectors"]
tui = ["dep:ratatui", "dep:crossterm"]       # TUI dashboard
mcp = ["dep:rmcp", "dep:schemars"]           # MCP server
vectors = ["dep:zerocopy"]                    # Vector search
```

Build without optional features for a smaller binary:
```bash
cargo build --release --no-default-features   # CLI only, no TUI/MCP/vectors
```

---

## 🧪 Testing

78 tests covering every layer of the application:

```bash
cargo test                    # Run all tests
cargo clippy -- -D warnings   # Lint with zero warnings
```

### Test Coverage

| Layer | Tests | What's covered |
|-------|-------|----------------|
| 🗄️ Database | 17 | Migrations, task CRUD, atomic claiming, no-double-claim, context CRUD, file locks, vector store/search/upsert, cosine similarity, blob roundtrip |
| 🤖 LLM | 11 | Claude request serialization, response deserialization, system prompt extraction, error handling, model routing (Ollama/Claude/unknown) |
| 🔌 MCP | 12 | Server info, tool listing, schema validation, run execution, context get/set, search with embeddings, status, doctor |
| ⚡ Worker | 10 | Task execution, context loading, context sharing, lock acquire/release, lock conflicts, LLM failure handling, Claude routing, missing provider errors, chain context passing, chain failure stops |
| 🖥️ CLI | 18 | Timeout parsing, prompt resolution (arg/file/precedence), chain step parsing (plain/model/claude/multiple), gitignore creation/append/dedup, MCP config injection, statusline injection, config template |

All worker and MCP tests use **mock LLM clients** that return deterministic responses — no external services needed. Database tests use SQLite `:memory:` databases.

---

## 🔄 LLM Provider Routing

Kew supports two LLM providers with automatic routing:

### 🦙 Ollama (Local)
- Any model name not starting with `claude-` routes to Ollama
- Calls `/api/chat` for completions, `/api/embed` for embeddings
- Default URL: `http://localhost:11434` (configurable via `--ollama-url` or `KEW_OLLAMA_URL`)
- Supports all Ollama models: gemma3, llama3, codellama, mistral, phi3, etc.

### 🧠 Claude (Cloud)
- Models starting with `claude-` route to the Anthropic Messages API
- Requires `ANTHROPIC_API_KEY` environment variable or `--claude-key` flag
- System prompts extracted from message array to top-level API field (per Anthropic spec)
- Available models: `claude-sonnet-4-20250514`, `claude-haiku-4-5-20251001`, `claude-opus-4-20250514`
- Does not support embeddings — embedding always uses Ollama's `nomic-embed-text`

---

## 🔒 File Locking

When multiple agents work on a codebase simultaneously, file locks prevent conflicts:

```bash
# Agent 1 locks src/auth.rs while working
kew run -m gemma3:27b -w "Refactor auth" --lock src/auth.rs

# Agent 2 trying to lock the same file will fail immediately
kew run -m gemma3:27b -w "Add tests for auth" --lock src/auth.rs
# Error: could not acquire lock on src/auth.rs
```

Locks are TTL-based (default 600 seconds) and automatically cleaned up on expiry. They're released when the task completes, whether it succeeds or fails.

---

## 🏠 Project Initialization

```bash
kew init
```

This sets up kew for a project directory:
- 📁 Creates `.kew/` directory with SQLite database
- 📝 Scaffolds `kew_config.yaml` with sensible defaults
- 🔌 Injects MCP server config into `.claude/settings.local.json`
- 📊 Installs a status line script for Claude Code
- 🙈 Adds `.kew/` to `.gitignore`

Each step can be skipped with `--no-mcp`, `--no-statusline`, or `--no-gitignore`.

---

## 📊 Status Line

After `kew init`, Claude Code shows a live status bar:

```
◆ kew  ▶ 2 ⏳3 ✓15 ✗1  ctx:8 emb:42  DB:ok
```

This tells you at a glance: 2 running tasks, 3 pending, 15 done, 1 failed, 8 context entries, 42 embeddings, database accessible.

---

## 🏥 Health Check

```bash
kew doctor
```

Checks:
- ✅ Ollama reachable and responding
- ✅ Models available and loaded
- ✅ Database accessible and migrated
- ✅ Claude API key valid (if configured)

---

## 📜 License

MIT
