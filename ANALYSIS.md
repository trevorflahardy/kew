# kew — Full Codebase Analysis

> Generated 2026-04-06 via kew agents (watcher, error-finder, security) analyzing actual source.

---

## Architecture Overview

### Component Layers & Data Flow

```
MCP Tool Call (Claude Code)
  └─► KewMcpServer (src/mcp/server.rs) — rmcp #[tool_router] dispatch
        └─► Task created in SQLite (db::tasks::create_task)
              └─► Pool::submit_all_and_wait (src/worker/pool.rs)
                    └─► Worker::execute (src/worker/worker.rs)
                          ├─► context loaded: db::context::get_context_many
                          ├─► file locks: db::locks::acquire_lock
                          ├─► LLM call: OllamaClient | ClaudeClient
                          ├─► result stored: db::tasks::mark_done
                          ├─► share_as: db::context::put_context
                          └─► embedding: db::vectors::store_embedding (best-effort)
```

### Key Design Decisions

| Concern         | Implementation                                                                                        |
| --------------- | ----------------------------------------------------------------------------------------------------- |
| Concurrency     | `tokio::sync::mpsc` channels, N worker tasks                                                          |
| LLM routing     | `router::route()` — `claude-` prefix → Anthropic API, else → Ollama                                   |
| Agent config    | Hierarchical: explicit `--agent` → keyword auto-detect → defaults                                     |
| Memory (KV)     | SQLite `context` table, namespaced, injected into LLM prompts as `[Shared context: key]`              |
| Memory (vector) | `nomic-embed-text` via Ollama, stored in `vectors` table, cosine similarity search                    |
| Chain pipeline  | Sequential steps, each step auto-injects previous step output via `{chain_id}-step-{i-1}` context key |
| MCP protocol    | `rmcp` crate, `#[tool_router]` macro for tool dispatch                                                |

---

## Bugs & Logic Errors

### HIGH: Pool double-start panic (`src/worker/pool.rs`)

`task_rx` and `result_rx` are `Option<Receiver<_>>` consumed via `.take()` inside `start()`. If `submit_all_and_wait` is called a second time (or concurrently), `start()` is called again and `.expect("pool already started")` panics.

**Fix:** Decouple pool lifecycle — start once on construction, or guard with an `AtomicBool` and return `Err` instead of panicking.

---

### HIGH: Race condition panic on task claim (`src/cli/run.rs:262`)

```rust
db::tasks::claim_next_pending(&conn, "cli")
    .expect("just-created task should be claimable")  // ← will panic
```

In any multi-worker scenario, another worker can claim the task in the window between `create_task` and `claim_next_pending`. This panics the entire CLI process.

**Fix:** Retry with the specific task ID, or `claim_task_by_id` instead of `claim_next_pending`.

---

### MEDIUM: Cancelled task state divergence (`src/worker/worker.rs`)

After `client.chat(chat_req).await` returns, if the task was cancelled in the DB during that await point, `mark_done` correctly uses `WHERE status = 'running'` (a no-op), but `WorkResult` is still returned as `Ok(text)` to the Pool. The Pool treats it as success while the DB has it as cancelled.

**Fix:** After the await, re-check task status from DB before calling `mark_done` or `put_context` for `share_as`.

---

### MEDIUM: SQLite variable limit in `get_context_many` (`src/db/context.rs`)

```rust
format!("... WHERE key IN ({})", placeholders.join(","))
```

Hits `SQLITE_MAX_VARIABLE_NUMBER` (999 by default) if more than 999 context keys are requested. Silent runtime failure.

**Fix:** Chunk keys into batches of 500.

---

### MEDIUM: `acquire_lock` error swallowed (`src/worker/worker.rs`)

`acquire_lock` returns `rusqlite::Result<bool>`. The `Err` variant is silently treated the same as `Ok(false)`, potentially leaving tasks stuck waiting for locks that will never be acquired.

**Fix:** Propagate `Err` to `fail_task` so the task is properly marked failed.

---

### LOW: Silent embedding failure (`src/worker/worker.rs`)

Embedding errors log at `debug!` level only. If Ollama is unavailable, the vector index silently degrades — `kew context search` returns stale/incomplete results with no user-visible warning.

**Fix:** Promote to `warn!` so users know vector search capability is degrading.

---

## Security Findings

### HIGH: Path traversal via `--file` in MCP tool (`src/cli/run.rs:87`)

`resolve_prompt` calls `std::fs::read_to_string(path)` with no path validation. The MCP server exposes `kew_run` to Claude Code with a `file` parameter. Any prompt injection targeting Claude Code can instruct it to call `kew_run` with `file: "/etc/passwd"` or `~/.ssh/id_rsa`, exfiltrating arbitrary host files.

```rust
// Current — no validation:
return std::fs::read_to_string(path)

// Fix: canonicalize and check prefix
let base = std::env::current_dir()?.canonicalize()?;
let resolved = path.canonicalize()?;
anyhow::ensure!(resolved.starts_with(&base), "path outside working directory");
```

---

### MEDIUM: Agent keyword injection via prompt content

`detect_agent_from_prompt` routes tasks based on keywords found in user-controlled prompt strings. An attacker can craft prompts that force the `security` or `developer` agent to handle a task, altering its system prompt and potentially its behavior. Low practical impact since agent system prompts are local files, but it's an unintended trust boundary crossing.

**Fix:** Only apply keyword auto-detection when `agent` is not explicitly provided — which is already the case — but document clearly that prompt content influences routing.

---

### LOW: API key stored as plain `String`

`ClaudeClient { api_key: String }` stores the Anthropic API key in a plain heap string. No zeroization on drop. If a process dump or memory profiler runs, the key is trivially readable.

**Fix:** Use the `secrecy` crate's `Secret<String>` with `ZeroizeOnDrop`.

---

### LOW: No context namespace isolation between projects

`kew_context_set/get` share a single SQLite DB. Running kew in two different projects at the same `db_path` allows one project's context to bleed into another's.

**Fix:** Auto-namespace context by project root hash, or expose `--namespace` on MCP tools.

---

## Improvement Opportunities

| Area                   | Suggestion                                                                                       |
| ---------------------- | ------------------------------------------------------------------------------------------------ |
| Pool lifecycle         | Make `Pool` reusable across multiple `submit` calls without restart                              |
| Cancellation           | Add a `kew cancel <task_id>` CLI + MCP tool; have workers check a cancellation flag during await |
| Chain error recovery   | Support `on_error: continue\|stop\|retry` per chain step                                         |
| Embedding retry        | Background retry queue for failed embeddings                                                     |
| `--file` in MCP        | Restrict or remove the `file` parameter from the MCP-exposed `kew_run` tool                      |
| Progress streaming     | Stream partial LLM output back via MCP progress notifications instead of blocking until done     |
| Tier config validation | Validate `kew_config.yaml` tier→model mappings at startup, not at first use                      |
| Agent hot-reload       | Watch `.kew/agents/` for changes without requiring restart                                       |
