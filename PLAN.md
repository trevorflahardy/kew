# Kew: Implementation Plan

## Real Local Agent Orchestration — Rust Single Binary

---

## Context

**Problem:** Every multi-agent orchestration tool is bookkeeping theater. They build frameworks (topologies, consensus, JSON state files) without building the process — a worker that calls an LLM and returns results. Ruflo/claude-flow has 558K lines of TypeScript and a `ProviderManager` that's never imported. OpenAI Swarm is explicitly "not production." CrewAI and LangGraph are Python/JS server-oriented, not local-first CLI tools.

**What Kew does:** A single Rust binary that spawns real local LLM agents (Ollama) and API agents (Claude), coordinates them via SQLite, learns from past work via embedded vector search, and integrates with Claude Code as both a CLI tool and MCP server.

**Why Rust:** Maximum performance, smallest binaries, memory-safe without GC pauses, excellent async ecosystem (tokio), strong type system that encodes invariants at compile time. `cargo-dist` gives us Homebrew + GitHub Releases with zero manual work.

**Scope:** Workers are async tasks calling LLM APIs (Ollama `/api/chat`, Claude Messages API). Not spawning Claude Code CLI instances — this is API-level orchestration.

**Distribution:** Homebrew tap + GitHub Releases via `cargo-dist`. Cross-platform binaries for macOS (arm64/amd64), Linux (amd64/arm64), Windows.

---

## Architecture Overview

```
User / Claude Code
       |
       v (Bash tool: `kew run --wait "prompt"`)
       v (MCP tool: `kew_run`)
+----------------------------------------------+
|  CLI (kew) — clap + ratatui                  |
|  Single Rust binary, zero config to start    |
+------------------+---------------------------+
                   |
                   v
+----------------------------------------------+
|  Worker Pool (tokio tasks, not processes)     |
|  - N concurrent workers (default 4)          |
|  - mpsc channel work queue                   |
|  - CancellationToken propagates to reqwest   |
+------+----------+-----------+----------------+
       |          |           |
  +----v---+ +---v----+ +---v-----+
  |Worker 1| |Worker 2| |Worker 3 |
  |Gemma 4 | |Codellama| |Claude   |
  |Ollama  | |Ollama  | |API      |
  +----+---+ +---+----+ +---+-----+
       |          |           |
       v          v           v
+----------------------------------------------+
|  SQLite DB (WAL mode, single file)           |
|  + sqlite-vec (embedded vector search)       |
|  - tasks: queue, status, assignment          |
|  - context: shared knowledge + embeddings    |
|  - vec_context / vec_results: vector indexes |
|  - file_locks: prevent edit conflicts        |
+----------------------------------------------+
```

### How Claude Code Waits

**Mode 1 — Blocking CLI (v1, primary):**

```bash
kew run --model gemma4:26b --wait "Refactor the auth module"
```

Claude Code's Bash tool runs this. Blocks until LLM returns. Result prints to stdout. Claude Code reads stdout. Default Bash timeout 120s, configurable to 600s.

**Mode 2 — MCP Server (Phase 8, tighter integration):**

```json
{ "mcpServers": { "kew": { "command": "kew", "args": ["mcp", "serve"] } } }
```

MCP tools are naturally blocking. Claude Code calls `kew_run`, handler spawns worker, waits for completion, returns JSON result. Uses `rmcp` crate (official Rust MCP SDK, 4.7M downloads).

### Learning Over Time (Embedded Vector Search)

Every completed task result is embedded (via Ollama `/api/embed`) and stored in `vec_results`. Future tasks with `--auto-context` retrieve relevant past work via cosine similarity. This is RAG over accumulated work product — no fine-tuning, no pre-training, just embedded retrieval that gets more useful over time.

**Stack:** `sqlite-vec` (pure C extension loaded via `rusqlite`) + Ollama embeddings (`nomic-embed-text`, 768 dimensions). Zero external infrastructure.

---

## Technology Stack

| Component         | Crate                            | Why                                                            |
| ----------------- | -------------------------------- | -------------------------------------------------------------- |
| Async runtime     | `tokio`                          | Industry standard, powers reqwest/rmcp                         |
| CLI framework     | `clap` (derive)                  | Most popular Rust CLI, derive macros for ergonomic arg parsing |
| TUI dashboard     | `ratatui` + `crossterm`          | Sub-ms rendering, used by Netflix, 2100+ dependents            |
| Spinners/progress | `indicatif`                      | Beautiful progress bars and spinners for --wait mode           |
| Terminal styling  | `console`                        | Colors, emoji, terminal width detection                        |
| HTTP client       | `reqwest`                        | Async, built on hyper, excellent for Ollama/Claude API         |
| SQLite            | `rusqlite` (bundled)             | Ships SQLite in binary, mature, well-maintained                |
| Vector search     | `sqlite-vec`                     | Pure C SQLite extension, loaded via rusqlite                   |
| MCP server        | `rmcp`                           | Official Rust MCP SDK, 4.7M downloads, tokio-native            |
| Serialization     | `serde` + `serde_json`           | Standard Rust serialization                                    |
| IDs               | `ulid`                           | Sortable unique IDs, no coordination needed                    |
| Config            | `toml` + `serde`                 | Human-readable config files                                    |
| Error handling    | `thiserror` + `anyhow`           | thiserror for library errors, anyhow for CLI                   |
| Logging           | `tracing` + `tracing-subscriber` | Structured logging, integrates with tokio                      |
| Distribution      | `cargo-dist`                     | Automated GitHub Releases + Homebrew tap                       |

---

## Project Structure

```
kew/
├── Cargo.toml
├── Cargo.lock
├── dist-workspace.toml                # cargo-dist configuration
├── agents/                            # Built-in agent type definitions
│   ├── coder.yaml                     # General code generation
│   ├── tester.yaml                    # Test writing specialist
│   ├── reviewer.yaml                  # Code review / security audit
│   ├── architect.yaml                 # System design & planning
│   ├── documenter.yaml                # Documentation writer
│   ├── refactorer.yaml                # Refactoring specialist
│   └── analyst.yaml                   # Codebase analysis
├── src/
│   ├── main.rs                        # Entry point, tokio::main
│   ├── cli/                           # Command layer
│   │   ├── mod.rs                     # Clap App definition
│   │   ├── run.rs                     # `kew run` command
│   │   ├── chain.rs                   # `kew chain` command
│   │   ├── context.rs                 # `kew context` subcommands
│   │   ├── status.rs                  # `kew status` TUI dashboard
│   │   ├── submit.rs                  # `kew submit` (async)
│   │   ├── result.rs                  # `kew result` / `kew wait`
│   │   ├── list.rs                    # `kew list`
│   │   ├── init.rs                    # `kew init` — project setup + MCP injection
│   │   ├── mcp.rs                     # `kew mcp serve`
│   │   └── doctor.rs                  # `kew doctor` health check
│   ├── db/                            # Database layer
│   │   ├── mod.rs                     # Database, open, migrate, connection pool
│   │   ├── schema.rs                  # SQL migration strings
│   │   ├── tasks.rs                   # Task CRUD + atomic claiming
│   │   ├── context.rs                 # Context key-value + vector ops
│   │   ├── locks.rs                   # File lock acquire/release
│   │   └── models.rs                  # Structs: Task, ContextEntry, FileLock, enums
│   ├── worker/                        # Execution layer
│   │   ├── mod.rs
│   │   ├── worker.rs                  # Single task execution (THE critical code)
│   │   ├── pool.rs                    # Tokio task pool with mpsc channels
│   │   └── chain.rs                   # Chain execution logic
│   ├── llm/                           # LLM client layer
│   │   ├── mod.rs                     # LlmClient trait
│   │   ├── ollama.rs                  # Ollama HTTP client (/api/chat, /api/embed)
│   │   ├── claude.rs                  # Claude API client (Messages API)
│   │   └── router.rs                  # Model name -> provider routing
│   ├── context/                       # Context management
│   │   ├── mod.rs
│   │   ├── store.rs                   # Hybrid: explicit keys + vector search
│   │   ├── embedder.rs                # Calls Ollama /api/embed
│   │   └── compactor.rs              # Summary compaction for chains
│   ├── agents/                        # Agent type system
│   │   ├── mod.rs                     # AgentType loading + resolution
│   │   ├── types.rs                   # AgentDef struct, parsing, validation
│   │   └── registry.rs               # Built-in + project + user agent registry
│   ├── mcp/                           # MCP server
│   │   ├── mod.rs
│   │   ├── server.rs                  # MCP server setup (stdio, rmcp)
│   │   └── tools.rs                   # 8 tool definitions + handlers
│   ├── tui/                           # Terminal UI
│   │   ├── mod.rs
│   │   ├── dashboard.rs               # Ratatui live dashboard
│   │   └── styles.rs                  # Color theme, layouts
│   └── config.rs                      # Config loading (flags > env > kew_config.yaml > defaults)
├── tests/
│   ├── db_test.rs                     # DB integration tests
│   ├── worker_test.rs                 # Worker with mock LLM
│   ├── cli_test.rs                    # CLI invocation tests
│   └── mcp_test.rs                    # MCP protocol tests
├── ARCHITECTURE.md                    # Design philosophy doc
├── Makefile                           # Dev shortcuts
└── .github/
    └── workflows/
        ├── ci.yml                     # Test + lint on PR
        └── release.yml                # cargo-dist on tag push
```

**Per-project layout** (created by `kew init`):

```
your-project/
├── kew_config.yaml                    # Project-level kew configuration
├── .kew/                              # Local kew state (gitignored)
│   ├── kew.db                         # SQLite database for this project
│   └── agents/                        # Project-specific custom agent types
│       └── my-domain-expert.yaml
└── ...
```

**Estimated total: ~3,500 lines of Rust.** More than Go due to explicit error handling and type definitions, but stronger guarantees at compile time.

---

## Data Model (SQLite Schema)

### Migration 001: Core Tables

```sql
CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER DEFAULT (unixepoch('now'))
);

CREATE TABLE tasks (
    id TEXT PRIMARY KEY,                    -- ULID
    parent_id TEXT REFERENCES tasks(id),
    chain_id TEXT,
    chain_index INTEGER,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK(status IN ('pending','assigned','running','done','failed','cancelled')),
    model TEXT NOT NULL,
    provider TEXT NOT NULL DEFAULT 'ollama'
        CHECK(provider IN ('ollama','claude')),
    system_prompt TEXT,
    prompt TEXT NOT NULL,
    result TEXT,
    error TEXT,
    context_keys TEXT,                      -- JSON array
    share_as TEXT,
    files_locked TEXT,                      -- JSON array
    worker_id TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch('now')),
    started_at INTEGER,
    completed_at INTEGER,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    duration_ms INTEGER
);

CREATE TABLE context (
    key TEXT PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT 'default',
    content TEXT NOT NULL,
    content_hash TEXT,
    summary TEXT,
    metadata TEXT,
    created_by TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch('now')),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch('now'))
);

CREATE TABLE file_locks (
    file_path TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    locked_at INTEGER NOT NULL DEFAULT (unixepoch('now')),
    expires_at INTEGER
);

CREATE INDEX idx_tasks_status ON tasks(status);
CREATE INDEX idx_tasks_chain ON tasks(chain_id, chain_index);
CREATE INDEX idx_tasks_created ON tasks(created_at);
CREATE INDEX idx_context_namespace ON context(namespace);
```

### Migration 002: Vector Search (sqlite-vec)

```sql
CREATE VIRTUAL TABLE vec_context USING vec0(
    context_key TEXT PRIMARY KEY,
    embedding float[768] distance_metric=cosine
);

CREATE VIRTUAL TABLE vec_results USING vec0(
    task_id TEXT PRIMARY KEY,
    embedding float[768] distance_metric=cosine
);
```

---

## Agent Types

### Philosophy

Agent types are pre-configured personas — a system prompt, preferred model, and behavioral strategy bundled into a reusable YAML file. They save you from writing `--system "You are a senior engineer who writes thorough tests..."` every time.

### Resolution Order

1. **Project-specific:** `.kew/agents/*.yaml` (highest priority, project-local overrides)
2. **Project config inline:** `kew_config.yaml` agent definitions
3. **Built-in defaults:** Embedded in the binary from `agents/*.yaml` (compiled via `include_str!`)

### Agent Definition Format

```yaml
# agents/tester.yaml
name: tester
description: "Writes thorough unit and integration tests"
model: gemma4:26b # Default model (overridable via --model)
provider: ollama # ollama | claude
system_prompt: |
  You are a senior test engineer. You write thorough, well-structured tests.
  Rules:
  - Cover edge cases and error paths, not just happy paths
  - Use descriptive test names that explain the scenario
  - Keep tests independent — no shared mutable state
  - Prefer table-driven tests where patterns repeat
strategy:
  temperature: 0.2 # Lower = more deterministic for tests
  max_tokens: 4096
  context_mode: auto # none | explicit | auto (vector search)
  top_k: 3 # For auto context mode
```

### Built-in Agent Types

| Agent        | Model         | Purpose                                                        |
| ------------ | ------------- | -------------------------------------------------------------- |
| `coder`      | gemma4:26b    | General code generation and implementation                     |
| `tester`     | gemma4:26b    | Unit/integration test writing                                  |
| `reviewer`   | gemma4:26b    | Code review, security audit, best practices                    |
| `architect`  | claude-sonnet | System design, architecture decisions (needs strong reasoning) |
| `documenter` | gemma4:26b    | Documentation, READMEs, inline comments                        |
| `refactorer` | gemma4:26b    | Refactoring, cleanup, DRY improvements                         |
| `analyst`    | gemma4:26b    | Codebase analysis, dependency audits                           |

### Usage

```bash
# Use a named agent type instead of raw --system + --model
kew run --agent tester --wait "Write tests for src/auth.rs"

# Override the agent's default model
kew run --agent tester --model codellama --wait "Write tests for src/auth.rs"

# In parallel with different agents
kew run --parallel --wait \
  --task "Write tests for auth.rs:tester" \
  --task "Review auth.rs for vulnerabilities:reviewer" \
  --task "Document auth.rs public API:documenter"

# Chain with agents
kew chain --wait \
  --step "Analyze the codebase structure:analyst" \
  --step "Design a refactoring plan:architect" \
  --step "Implement the refactoring:coder"
```

### Custom Project Agents

Drop a YAML file in `.kew/agents/` to define project-specific agents:

```yaml
# .kew/agents/domain-expert.yaml
name: domain-expert
description: "Understands our domain-specific business rules"
model: gemma4:26b
system_prompt: |
  You are an expert in our fintech payment processing domain.
  Key concepts: PSP (payment service provider), settlement, reconciliation.
  Our codebase uses the hexagonal architecture pattern.
  Always consider PCI-DSS compliance when touching payment data.
strategy:
  temperature: 0.3
  context_mode: auto
```

---

## Project Configuration (kew_config.yaml)

### Purpose

`kew_config.yaml` lives in the project root and configures kew's behavior for that project. It's the equivalent of `.eslintrc` or `pyproject.toml` — project-level settings that every team member shares (committed to git, unlike `.kew/` which is gitignored).

### Format

```yaml
# kew_config.yaml — project-level kew configuration

# Default settings for all tasks in this project
defaults:
  model: gemma4:26b # Default model if --model not specified
  provider: ollama # Default provider
  workers: 4 # Default concurrent workers
  timeout: 5m # Default task timeout
  auto_context: false # Enable vector search by default
  top_k: 5 # Default vector search results

# Ollama configuration
ollama:
  url: http://localhost:11434 # Ollama API endpoint
  embedding_model: nomic-embed-text # Model for generating embeddings
  pull_on_missing: true # Auto-pull models if not found locally

# Claude API configuration (optional)
claude:
  model: claude-sonnet-4-20250514 # Default Claude model
  max_tokens: 8192 # Default max tokens for Claude

# Model aliases — shorthand names for your team
aliases:
  fast: gemma4:26b # "kew run -m fast" → gemma4:26b
  smart: claude-sonnet-4-20250514 # "kew run -m smart" → claude-sonnet
  code: codellama # "kew run -m code" → codellama

# Agent type overrides (override built-in defaults for this project)
agents:
  coder:
    model: codellama # This project prefers codellama for code gen
    system_prompt_append: |
      This project uses Rust with tokio for async. Follow existing patterns.
  tester:
    system_prompt_append: |
      We use the `#[tokio::test]` macro. Tests go in a `tests` module at the bottom of each file.

# Inline custom agents (alternative to .kew/agents/*.yaml files)
custom_agents:
  - name: migration-expert
    description: "Handles database migration tasks"
    model: gemma4:26b
    system_prompt: |
      You write SQL migrations for our SQLite database.
      Always include both up and down migrations.
      Use IF NOT EXISTS for safety.

# File patterns — auto-lock files matching these globs when agents touch them
auto_lock:
  - "*.rs"
  - "Cargo.toml"
  - "Cargo.lock"
```

### Config Resolution

Priority (highest to lowest):

1. CLI flags (`--model`, `--workers`, etc.)
2. Environment variables (`KEW_MODEL`, `KEW_OLLAMA_URL`, `ANTHROPIC_API_KEY`)
3. `kew_config.yaml` in current directory (walk up to find it, like `.gitignore`)
4. `~/.config/kew/config.yaml` (global user config)
5. Compiled defaults

---

## `kew init` — Project Setup Command

### What It Does

`kew init` is a one-command setup that makes a project kew-ready and injects MCP hooks into Claude Code's local settings.

```bash
$ kew init
✓ Created .kew/ directory
✓ Created .kew/kew.db (SQLite database)
✓ Created .kew/agents/ (custom agent directory)
✓ Generated kew_config.yaml (project config)
✓ Added .kew/ to .gitignore
✓ Injected kew MCP server into .claude/settings.local.json
✓ Ollama is running at http://localhost:11434
✓ Model gemma4:26b is available

Ready. Try: kew run -m gemma4:26b -w "Say hello"
```

### Step-by-Step

1. **Create `.kew/` directory** — local state, gitignored
2. **Initialize `.kew/kew.db`** — run schema migrations, set WAL mode
3. **Create `.kew/agents/`** — empty dir for project-specific agent YAMLs
4. **Scaffold `kew_config.yaml`** — generate a commented template with sensible defaults. If one already exists, skip (don't overwrite).
5. **Append `.kew/` to `.gitignore`** — if `.gitignore` exists and doesn't already contain it
6. **Inject MCP server config into Claude Code settings:**
   - Locate `.claude/settings.local.json` (project-level) or create it
   - Add/update the `mcpServers.kew` entry:
     ```json
     {
       "mcpServers": {
         "kew": {
           "command": "kew",
           "args": ["mcp", "serve", "--db", ".kew/kew.db"]
         }
       }
     }
     ```
   - Merge, don't overwrite — preserve existing MCP servers
7. **Health check** — verify Ollama is running, check if default model is pulled
   - If Ollama is running but model is missing and `pull_on_missing: true`, offer to pull it

### Flags

```
kew init
    --no-mcp                           # Skip MCP injection into Claude settings
    --no-gitignore                     # Skip .gitignore modification
    --global                           # Init global config at ~/.config/kew/ instead
    --model <string>                   # Set default model in kew_config.yaml
    --force                            # Overwrite existing kew_config.yaml
```

### Idempotent

Running `kew init` twice is safe. It skips steps that are already done and only creates/modifies what's missing.

---

## Core Rust Types

```rust
// src/db/models.rs

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Pending, Assigned, Running, Done, Failed, Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    Ollama, Claude,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub parent_id: Option<String>,
    pub chain_id: Option<String>,
    pub chain_index: Option<i32>,
    pub status: TaskStatus,
    pub model: String,
    pub provider: Provider,
    pub system_prompt: Option<String>,
    pub prompt: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub context_keys: Vec<String>,
    pub share_as: Option<String>,
    pub files_locked: Vec<String>,
    pub worker_id: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub duration_ms: Option<i64>,
}

// src/llm/mod.rs

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
    async fn chat_stream(&self, req: ChatRequest, tx: mpsc::Sender<String>) -> Result<ChatResponse>;
    async fn embed(&self, model: &str, input: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn list_models(&self) -> Result<Vec<String>>;
    async fn ping(&self) -> Result<()>;
}
```

---

## Worker Execution Flow

```
CLI: `kew run --model gemma4:26b --wait "Write tests for auth"`
  │
  ├─ 1. Create Task in DB (status = pending, id = ULID)
  ├─ 2. Worker claims task (atomic UPDATE...RETURNING, status = running)
  ├─ 3. Load explicit context (SELECT FROM context WHERE key IN ...)
  ├─ 4. [Optional] Vector search for relevant past results (--auto-context)
  ├─ 5. Acquire file locks (INSERT OR IGNORE)
  ├─ 6. Build messages: [system_prompt, context entries, user prompt]
  ├─ 7. *** CALL THE LLM *** (reqwest POST to Ollama or Claude API)
  ├─ 8. Store result in DB (status = done, result = LLM output)
  ├─ 9. If share_as set: INSERT INTO context + generate embedding
  ├─ 10. Embed result into vec_results (always, for future retrieval)
  ├─ 11. Release file locks
  └─ 12. Print result to stdout
```

### Worker Pool (Tokio)

```rust
// src/worker/pool.rs — conceptual

pub struct Pool {
    db: Arc<Database>,
    clients: Arc<HashMap<Provider, Box<dyn LlmClient>>>,
    ctx_store: Arc<ContextStore>,
    size: usize,
    task_tx: mpsc::Sender<Task>,
    result_rx: mpsc::Receiver<WorkResult>,
}

pub struct WorkResult {
    pub task_id: String,
    pub result: Result<String, WorkerError>,
    pub stats: ExecutionStats,
}

impl Pool {
    pub fn new(db: Arc<Database>, clients: ..., size: usize) -> Self;
    pub fn start(&self) -> JoinHandle<()>;       // Spawns N tokio tasks
    pub async fn submit(&self, task: Task);       // Send via mpsc channel
    pub async fn wait_all(&mut self) -> Vec<WorkResult>;  // Collect all results
    pub async fn shutdown(self);                  // Graceful shutdown
}
```

**Key Rust-specific design:** `rusqlite` is synchronous. DB operations run inside `tokio::task::spawn_blocking` to avoid blocking the async runtime. The `Database` wrapper provides async methods that internally use spawn_blocking:

```rust
// src/db/mod.rs

pub struct Database {
    conn: Arc<Mutex<Connection>>,  // rusqlite Connection
}

impl Database {
    pub async fn claim_next_pending(&self, worker_id: &str) -> Result<Option<Task>> {
        let conn = self.conn.clone();
        let wid = worker_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            // Atomic: single UPDATE...RETURNING statement
            conn.query_row(
                "UPDATE tasks SET status = 'assigned', worker_id = ?1, started_at = unixepoch('now')
                 WHERE id = (SELECT id FROM tasks WHERE status = 'pending' ORDER BY created_at LIMIT 1)
                 RETURNING *",
                [&wid],
                |row| Task::from_row(row),
            ).optional()
        }).await?
    }
}
```

---

## CLI Command Structure

```
kew init                                # Setup project: .kew/, config, MCP injection
    --no-mcp                            # Skip MCP injection into Claude settings
    --no-gitignore                      # Skip .gitignore modification
    --global                            # Init global config at ~/.config/kew/
    --model <string>                    # Set default model in kew_config.yaml
    --force                             # Overwrite existing kew_config.yaml

kew run [prompt]                        # Execute task, print result
    --model, -m <string>                # Model name or alias (default from config)
    --agent, -a <name>                  # Use named agent type (e.g. tester, reviewer)
    --wait, -w                          # Block until done
    --system, -s <string>               # System prompt (overrides agent's)
    --file, -f <path>                   # Read prompt from file
    --context, -c <key>                 # Load context key (repeatable)
    --share-as <key>                    # Store result as context key
    --lock <path>                       # File lock (repeatable)
    --parallel, -p                      # Parallel mode
    --task, -t <"prompt:agent_or_model"># Task spec (repeatable) — accepts agent name or model
    --workers, -n <int>                 # Concurrent workers (default: 4)
    --timeout <duration>                # Max wait (default: 5m)
    --auto-context                      # Vector search for relevant context
    --top-k <int>                       # Vector results (default: 5)
    --json                              # JSON output
    --quiet, -q                         # No spinner

kew chain --step "prompt:agent" ...     # Sequential chain (steps accept agent names)
kew submit [prompt]                     # Async: returns task ID
kew result <task-id>                    # Get result
kew wait <task-id>                      # Block until done
kew list                                # List tasks
kew status                              # Interactive TUI dashboard (ratatui)
kew context list|get|set|delete|search|clear
kew agents list                         # List available agent types (built-in + project)
kew agents show <name>                  # Show agent definition details
kew mcp serve                           # Start MCP server (stdio)
kew doctor                              # Health check
kew version
```

### Output Design

- **--wait mode:** Raw LLM output to stdout. No decoration. This is what Claude Code reads.
- **--json mode:** `{ "task_id": "...", "status": "done", "result": "...", "duration_ms": 2340 }`
- **Interactive:** `indicatif` spinner, then formatted result with `console` colors.

---

## MCP Server (8 Tools)

Built with `rmcp` crate. All tools block until completion.

| Tool                 | Description                   |
| -------------------- | ----------------------------- |
| `kew_run`            | Execute prompt, return result |
| `kew_chain`          | Sequential task chain         |
| `kew_parallel`       | Multiple tasks in parallel    |
| `kew_context_get`    | Read shared context           |
| `kew_context_set`    | Write shared context          |
| `kew_context_search` | Vector similarity search      |
| `kew_status`         | System status                 |
| `kew_doctor`         | Health check                  |

### Claude Code Config

```json
{
  "mcpServers": {
    "kew": {
      "command": "kew",
      "args": ["mcp", "serve"],
      "env": { "ANTHROPIC_API_KEY": "sk-..." }
    }
  }
}
```

---

## Build Phases

### Phase 1: Foundation (DB + Types)

**~500 lines**

- Files: `Cargo.toml`, `src/main.rs`, `src/db/*`, `src/config.rs`, `src/db/models.rs`
- Validates: Schema creates in `:memory:`, task CRUD works, migrations run
- Key crates: `rusqlite` (bundled), `ulid`, `serde`, `serde_json`, `tokio`, `thiserror`, `anyhow`

### Phase 2: LLM Client (Actually Call Ollama)

**~400 lines**

- Files: `src/llm/mod.rs`, `src/llm/ollama.rs`, `src/llm/router.rs`
- Validates: Gemma4 returns real text
- Key crates: `reqwest`, `async-trait`

### Phase 3: Worker (Task Execution)

**~400 lines**

- Files: `src/worker/worker.rs`, `src/worker/pool.rs`
- Validates: Task goes pending -> running -> done with real LLM output in DB

### Phase 4: CLI (`kew run --wait`) + Config + Init — THE LITMUS TEST

**~600 lines**

- Files: `src/cli/mod.rs`, `src/cli/run.rs`, `src/cli/init.rs`, `src/config.rs`, `src/main.rs` (wired up)
- Key crates: `clap` (derive), `indicatif`, `console`, `serde_yaml`
- Includes:
  - `kew run --wait` end-to-end blocking execution
  - `kew init` — creates `.kew/`, scaffolds `kew_config.yaml`, appends `.gitignore`
  - `kew_config.yaml` parsing with resolution chain (CLI > env > config > defaults)
  - `kew doctor` — health check (Ollama reachable? Model pulled? DB writable?)
- Validates:

```bash
$ cargo build --release
$ ./target/release/kew init
✓ Created .kew/ directory
✓ Generated kew_config.yaml
$ ./target/release/kew run -m gemma4:26b -w "Write a prime checker in Python"
# Real code comes back = it works
# JSON state file appears = you built ruflo again
```

### Phase 5: Context Sharing (Explicit Keys)

**~250 lines**

- Files: `src/context/store.rs`, `src/cli/context.rs`
- Validates: `kew run --share-as "X" ...` then `kew run --context "X" ...` works

### Phase 6: Agent Types + Chain Mode + File Locking

**~500 lines**

- Files: `src/agents/mod.rs`, `src/agents/types.rs`, `src/agents/registry.rs`, `agents/*.yaml` (7 built-in definitions), `src/cli/chain.rs`, `src/worker/chain.rs`
- Includes:
  - Agent type YAML parsing and validation
  - Registry: built-in (embedded via `include_str!`) + `.kew/agents/` + `kew_config.yaml` inline agents
  - `--agent` flag on `kew run` and agent names in `--task` specs
  - `kew agents list` and `kew agents show <name>`
  - Model alias resolution from `kew_config.yaml`
  - Chain mode with agent names per step
  - File locking with auto-lock patterns from config
- Validates:

```bash
$ kew agents list
  coder       gemma4:26b       General code generation
  tester      gemma4:26b       Unit/integration test writing
  reviewer    gemma4:26b       Code review, security audit
  architect   claude-sonnet    System design (strong reasoning)
  documenter  gemma4:26b       Documentation writer
  refactorer  gemma4:26b       Refactoring specialist
  analyst     gemma4:26b       Codebase analysis

$ kew run --agent tester -w "Write tests for src/auth.rs"
# Uses tester's system prompt, model, and strategy settings

$ kew chain --wait \
    --step "Analyze the codebase:analyst" \
    --step "Design improvements:architect" \
    --step "Implement changes:coder"
```

### Phase 7: Vector Search + Learning

**~400 lines**

- Files: `src/context/embedder.rs`, `src/db/schema.rs` (migration 002)
- Key crate: `sqlite-vec` (loaded as rusqlite extension), `zerocopy` for zero-copy vector passing
- Validates: `kew context search "authentication"` returns relevant past results

### Phase 8: MCP Server + Init MCP Injection

**~400 lines**

- Files: `src/mcp/server.rs`, `src/mcp/tools.rs`, `src/cli/mcp.rs`, update `src/cli/init.rs`
- Key crate: `rmcp`
- Includes:
  - MCP server with 8 blocking tools (stdio transport)
  - `kew_run` MCP tool accepts `agent` parameter (use named agent types from MCP)
  - Update `kew init` to inject MCP config into `.claude/settings.local.json`
  - Merge logic: read existing JSON, add/update `mcpServers.kew`, write back without clobbering other servers
- Validates:

```bash
$ kew init
✓ Injected kew MCP server into .claude/settings.local.json

# Claude Code now sees kew_run, kew_chain, kew_parallel as native MCP tools
```

### Phase 9: TUI Dashboard + Polish

**~400 lines**

- Files: `src/tui/dashboard.rs`, `src/tui/styles.rs`, remaining CLI commands
- Key crates: `ratatui`, `crossterm`
- Validates: `kew status` shows beautiful live dashboard

### Phase 10: Claude API Client + Distribution

**~300 lines**

- Files: `src/llm/claude.rs`, `dist-workspace.toml`, `.github/workflows/release.yml`
- Key crate: `cargo-dist` for automated releases
- Validates: `kew run -m claude-sonnet-4-20250514 -w "prompt"` routes to Anthropic API
- Validates: `brew install futur/tap/kew` works

---

## Dependencies (Cargo.toml)

```toml
[package]
name = "kew"
version = "0.1.0"
edition = "2021"
description = "Real local agent orchestration"
license = "MIT"

[[bin]]
name = "kew"
path = "src/main.rs"

[dependencies]
# Async
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"

# CLI
clap = { version = "4", features = ["derive"] }

# HTTP
reqwest = { version = "0.12", features = ["json"] }

# Database
rusqlite = { version = "0.32", features = ["bundled"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"                                    # kew_config.yaml + agent definitions
toml = "0.8"

# IDs
ulid = "1"

# Error handling
thiserror = "2"
anyhow = "1"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# TUI & terminal (Phase 4+)
indicatif = "0.17"
console = "0.15"

# TUI dashboard (Phase 9)
ratatui = { version = "0.29", optional = true }
crossterm = { version = "0.28", optional = true }

# MCP server (Phase 8)
rmcp = { version = "1", features = ["server", "transport-io"], optional = true }

# Vector search (Phase 7)
zerocopy = { version = "0.8", optional = true }

[features]
default = ["tui", "mcp", "vectors"]
tui = ["dep:ratatui", "dep:crossterm"]
mcp = ["dep:rmcp"]
vectors = ["dep:zerocopy"]
```

**Feature flags** keep early phases lean. Phase 1-4 can build with `--no-default-features`. Full build includes everything.

---

## Testing Strategy

### Layer 1 — Unit Tests (no external deps)

- DB operations on `:memory:`
- Task state machine transitions
- Config parsing, model routing
- JSON serialization of context keys
- Run: `cargo test`

### Layer 2 — Service Tests (mock LLM client)

```rust
struct MockLlmClient {
    response: String,
    latency: Duration,
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
        tokio::time::sleep(self.latency).await;
        Ok(ChatResponse { message: ChatMessage { role: "assistant".into(), content: self.response.clone() }, .. })
    }
}
```

- Worker execution, pool concurrency, chain execution
- Two tasks racing on `claim_next_pending` get different tasks (no double-claim)

### Layer 3 — Integration Tests (require Ollama)

```rust
#[tokio::test]
#[ignore] // Requires Ollama running
async fn test_end_to_end_ollama() {
    let output = Command::new("./target/release/kew")
        .args(["run", "-m", "gemma4:26b", "-w", "Say hello"])
        .output().await.unwrap();
    assert!(output.status.success());
    assert!(!output.stdout.is_empty());
}
```

- Run: `cargo test -- --ignored`

### Key Test Scenarios

1. Schema migration applies cleanly (`:memory:`)
2. Atomic task claiming — no double-claim under contention
3. Worker produces correct ChatRequest from Task fields
4. Context keys loaded and injected as messages
5. File locks: first acquire succeeds, second fails
6. Chain: step N+1 sees step N's output as context
7. Pool(4) runs 4 tasks concurrently (timing assertion)
8. MCP initialize handshake succeeds
9. Vector search returns semantically relevant results

---

## Verification Plan

After each phase:

```bash
cargo test                        # Unit + service tests
cargo clippy -- -D warnings       # Lint
```

After Phase 4 (litmus test):

```bash
cargo build --release
./target/release/kew doctor       # Check Ollama health
./target/release/kew run -m gemma4:26b -w "Say hello"  # Real LLM call
```

After Phase 8 (MCP):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0.0"}}}' | ./target/release/kew mcp serve
```

After Phase 10 (distribution):

```bash
cargo dist build                  # Build release artifacts
cargo dist plan                   # Verify release plan
```

---

## Key Rust-Specific Design Decisions

### 1. rusqlite is synchronous — async wrapper needed

`rusqlite` doesn't have async support. All DB calls go through `tokio::task::spawn_blocking` with an `Arc<Mutex<Connection>>`. This is the standard pattern and performs well — SQLite queries are microseconds, the mutex is uncontended 99% of the time.

### 2. sqlite-vec loaded as extension

```rust
use rusqlite::Connection;

fn open_db(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // Load sqlite-vec extension
    unsafe { conn.load_extension_enable()?; }
    sqlite_vec::load(&conn)?;  // From sqlite-vec crate
    unsafe { conn.load_extension_disable()?; }
    Ok(conn)
}
```

### 3. Error types with thiserror

```rust
#[derive(Debug, thiserror::Error)]
pub enum KewError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("LLM error: {0}")]
    Llm(String),
    #[error("task {0} not found")]
    TaskNotFound(String),
    #[error("Ollama not reachable at {0}")]
    OllamaUnavailable(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
}
```

### 4. Feature flags for incremental builds

Early phases compile with `--no-default-features` — no ratatui, no rmcp, no zerocopy. This keeps compile times fast during initial development. Full build enables everything.

### 5. Zero-copy vector passing

```rust
use zerocopy::AsBytes;
let embedding: Vec<f32> = ollama.embed("nomic-embed-text", &[text]).await?;
// Pass directly to sqlite-vec without copying
stmt.execute(params![key, embedding.as_bytes()])?;
```

---

## What Makes This Different

1. **Phase 2 calls a real LLM.** Not phase 8. Not "future work."
2. **Phase 4 is the litmus test.** Real code from Gemma4 or it doesn't work. No ambiguity.
3. **`kew init` and you're done.** One command sets up the project, injects MCP hooks into Claude Code. Zero manual config.
4. **Named agent types.** `--agent tester` instead of copy-pasting system prompts. 7 built-ins, project-custom via YAML.
5. **Project-level `kew_config.yaml`.** Model aliases, agent overrides, Ollama settings — committed to git, shared by team.
6. **3-tier agent hierarchy.** Claude Code sub-agents spawn kew workers. Expensive models reason, cheap local models execute.
7. **Single Rust binary.** ~10MB, instant startup, no runtime dependencies.
8. **Learning is automatic.** Every result gets embedded. Future tasks benefit. No explicit "training."
9. **The bus is SQLite.** One file. Survives crashes. Inspectable with `sqlite3`.
10. **Homebrew installable.** `brew install futur/tap/kew` from day one of release.
