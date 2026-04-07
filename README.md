# kew

**Real agent orchestration for Claude Code. Install it. Forget about it. Save tokens.**

kew offloads LLM-heavy work — code generation, debugging, testing, security audits, documentation — from Claude Code to local models running on your machine via Ollama. One `cargo install`, one `kew init`, and Claude Code automatically gains a team of specialist agents that run in parallel on cheap local models while Claude focuses on orchestration.

### What makes kew different

- **Actually autonomous.** `kew init` writes the MCP config, injects a CLAUDE.md that teaches Claude how to use kew, sets up a live status line, and configures the SQLite database. There is nothing else to do — Claude Code discovers kew's tools automatically and starts dispatching work to local agents on its own.
- **Saves tokens at scale.** Every task that kew handles locally is work that Claude doesn't burn API tokens on. Code generation, test writing, security reviews, doc audits — all run through Ollama on your hardware. Claude only sees the final results, not the intermediate reasoning.
- **Real agents, not wrappers.** kew agents run an agentic tool loop — they can read files, search codebases, list directories, and write files mid-generation. They're not one-shot prompt-in/text-out calls. They explore your codebase and produce grounded results.
- **Parallel by default.** A persistent worker pool (default: 4 concurrent tokio tasks) means Claude can fire off a developer, tester, and security auditor simultaneously. Results land in SQLite; Claude collects them when ready.
- **Zero infrastructure.** Single Rust binary. SQLite database. No daemon, no Docker, no cloud subscription, no IPC. Tasks are rows. Context is rows. Embeddings are rows.

```
Claude Code  ──MCP──▶  kew_run / kew_context_*  ──▶  Worker Pool (tokio)
                                                            │
                                              ┌────────────┼────────────┐
                                              ▼            ▼            ▼
                                          Ollama      Claude API   (more providers)
                                              │            │
                                              │    ┌───────┘
                                              │    │  Agentic Tool Loop
                                              │    │  ┌─ read_file ─┐
                                              │    ├──┤  list_dir   ├──▶ filesystem
                                              │    │  ├─ grep ──────┤
                                              │    │  └─ write_file ┘
                                              └────┘
                                                │
                                         SQLite (.kew/kew.db)
                                     tasks · context · embeddings · locks
```

---

## Install

### Homebrew (macOS / Linux)

```bash
brew tap trevorflahardy/kew https://github.com/trevorflahardy/kew
brew install kew
```

### Shell script (macOS / Linux)

```bash
curl -fsSL https://github.com/trevorflahardy/kew/releases/latest/download/install.sh | sh
```

### PowerShell (Windows — auto-adds to PATH)

```powershell
irm https://github.com/trevorflahardy/kew/releases/latest/download/install.ps1 | iex
```

### From source

```bash
cargo install --path .
```

---

## Quick start

```bash
# 1. Install Ollama and pull a model
ollama pull gemma3:27b

# 2. Set up your project — this is the only setup step
kew init
```

That's it. `kew init` does everything:

- Creates `.kew/` with the SQLite database
- Writes `.mcp.json` so Claude Code discovers kew as an MCP server
- Generates a `CLAUDE.md` that teaches Claude how to orchestrate kew agents
- Installs a live status line showing running tasks, token usage, and DB stats
- Adds `.kew/` to `.gitignore`

**Start a new Claude Code session and it just works.** Claude reads the CLAUDE.md, sees the MCP tools, and begins dispatching work to kew agents automatically. You don't need to prompt it — it knows when to use `kew_run` for code generation, testing, debugging, and audits.

### Manual usage (optional)

You can also use kew directly from the CLI:

```bash
# Run a task locally and wait for output
kew run -m gemma3:27b -w "Write a prime checker in Python"

# Use Claude API instead (models starting with claude- route automatically)
export ANTHROPIC_API_KEY="sk-ant-..."
kew run -m claude-sonnet-4-6 -w "Explain Rust lifetimes in 3 sentences"
```

---

## How Claude Code uses kew

When you ask Claude Code to implement a feature, fix a bug, or review code in a kew-enabled project, here's what happens behind the scenes:

```
You: "Add rate limiting to the API and make sure it's secure"

Claude Code (reads CLAUDE.md, sees kew MCP tools):
  ├── kew_run { agent: "developer", prompt: "Implement rate limiting..." }    ─┐
  ├── kew_run { agent: "security",  prompt: "Audit rate limiting for..." }    ─┤ parallel
  └── kew_run { agent: "tester",    prompt: "Write tests for rate limit..." } ─┘
                         │
              All 3 run simultaneously on Ollama (local, free)
              Claude waits, then reviews all results
              Claude applies the code, informed by the security audit
```

Claude stays in the orchestrator role — planning, reviewing, committing. The heavy LLM work (generating code, analyzing security, writing tests) runs locally through kew's worker pool. Your API token spend drops because Claude only processes the final outputs, not the multi-turn reasoning each agent did to get there.

---

## Under the hood

<details>
<summary><strong>Task lifecycle</strong> — from submission to result</summary>

Each task follows this path:

1. Task arrives (CLI, MCP, or chain step) — inserted as a `pending` row in SQLite
2. Worker atomically claims it via `UPDATE...RETURNING` — no double-claiming possible
3. Context loads: explicit keys + optional vector similarity search over past results
4. File locks acquired if specified (TTL-based, auto-expire)
5. **Agentic tool loop** — LLM called with tool definitions (`read_file`, `list_dir`, `grep`, `write_file`). If the model calls a tool, the worker executes it and feeds the result back. Loop repeats until the model produces a final text answer or hits the 25-iteration cap.
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

## Agent tools — real agentic execution

This is what separates kew from prompt wrappers. Agents don't just receive a prompt and return text — they run a **multi-turn tool loop** where the LLM can explore your codebase mid-generation. An agent asked to "refactor the auth module" will read the files, grep for usage patterns, understand the structure, and then write the refactored code — all autonomously.

| Tool         | Description                                                      | Locks required? |
| ------------ | ---------------------------------------------------------------- | --------------- |
| `read_file`  | Read a file with optional line range (100 KB cap, line numbers)  | No              |
| `list_dir`   | List directory contents with types and sizes                     | No              |
| `grep`       | Regex search across files with optional glob filter              | No              |
| `write_file` | Write/overwrite a file (creates parent dirs, 1 MB cap)           | Advisory check  |

### How it works

```
User prompt → LLM (with tool definitions)
                ↓
         ┌──── Does the response contain tool_calls? ────┐
         │ YES                                            │ NO
         ▼                                                ▼
   Execute tools (sandboxed)                       Final text response
   Append results to conversation                  → stored as task result
   Send back to LLM ──────────────────────────────▶ (loop, max 25 iterations)
```

### Security model

- All paths resolve relative to the project root. Path traversal (`../`) is blocked via `canonicalize()` + `starts_with()` checks.
- `write_file` checks advisory locks — if another task holds a lock on the file, the write is rejected.
- Reads are always free — no locks needed. Multiple agents can read the same file concurrently.
- Binary files are skipped by `grep`. Hidden directories (except `.kew`) and `target/`, `node_modules/`, `.git/` are excluded from walks.
- Max 25 tool-call iterations per task to prevent runaway agents.

### Supported providers

Both Ollama and Claude API support tool calling. kew translates tool definitions and results to each provider's native wire format:

- **Ollama** — `tools` array in request, `tool_calls` in assistant message, `role: "tool"` for results
- **Claude** — `tools` with `input_schema`, `tool_use` content blocks, `tool_result` blocks

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

After `kew init`, Claude Code shows a live status bar — no configuration needed:

```
◆ kew  ▶ developer tester ⏳1 ✓15 ✗1  ctx:8 emb:42 tok:14.2k  4.1MB
```

Running agents · pending · done · failed · context entries · embeddings · total local tokens · DB size. The token count shows work done locally that didn't cost API tokens.

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
| Regex         | `regex`                     |
| Logging       | `tracing`                   |

Feature flags — build without optional components:

```toml
[features]
default = ["tui", "mcp", "vectors", "index"]
tui     = ["dep:ratatui", "dep:crossterm"]
mcp     = ["dep:rmcp", "dep:schemars"]
vectors = ["dep:zerocopy"]
index   = ["dep:notify", "dep:ignore"]
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

112 tests across all layers. Worker and MCP tests use mock LLM clients — no external services needed. Database tests use SQLite `:memory:`. The agentic tool loop is tested with a `ToolCallingMock` that simulates multi-round tool calls before producing a final answer.

---

## License

MIT
