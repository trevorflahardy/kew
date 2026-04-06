# Kew — Real Local Agent Orchestration

> **Kew** (like a queue, like Kew Gardens where things actually grow)
> A CLI tool that spawns real local LLM agents that do real work.

---

## Context: What We Learned From Auditing Ruflo/Claude-Flow V3

We performed a full audit of the ruflo/claude-flow v3 codebase (558K lines TypeScript, 20 sub-packages, 5900+ commits). Here's what we found:

### What's wrong with existing approaches (ruflo as case study)

1. **The "swarm" is bookkeeping theater.** MCP tools like `swarm_init` and `agent_spawn` write JSON state files (`.claude-flow/swarm/swarm-state.json`, `.claude-flow/agents/store.json`). No process reads these files to make decisions. The actual work is done by Claude Code's native Agent/Task tool, which works fine without any of this.

2. **Providers exist but are disconnected.** A fully implemented `OllamaProvider` (with streaming, tool calling, health checks, circuit breakers) and `ProviderManager` (with load balancing, failover, cost routing) exist in `@claude-flow/providers/`. But `ProviderManager` is **never imported** by the CLI, swarm, or hooks systems. Zero references. It's a finished library sitting in a drawer.

3. **"60+ agents" is fiction.** The CLI validates 15 agent types. Only 5 have YAML configs. Unknown types silently fall back to a generic "coder" agent. The "60+" number comes from Claude Code's own agent type list, not ruflo's implementation.

4. **Security tools built but not applied.** `SafeExecutor`, `PathValidator`, `InputValidator` are well-designed but the git worker uses raw `child_process` functions with shell interpretation, the memory system writes to disk without path validation, and shell hooks interpolate unescaped variables. The right primitives exist but aren't wired in.

5. **Memory system is actually real.** SQLite + better-sqlite3, HNSW vector search (real algorithm, not brute-force), hybrid backend with dual-write. This is the one genuinely functional piece. But "150x-12,500x faster" claims are borrowed from AgentDB's paper, not measured in v3.

6. **The fundamental design flaw:** The entire system is a coordination framework without execution. Agents are JSON records, not processes. The swarm is state tracking pretending to be orchestration. Nobody built the part where an actual LLM receives a prompt and generates a completion inside the agent framework.

### The core pattern across ALL vaporware orchestration tools

```
README promises > Architectural diagrams > Type definitions >
JSON state management > ... gap ... > actual LLM execution never happens
```

They all build the framework (topologies, consensus, byzantine fault tolerance) without building the process (a worker that calls an LLM and returns results).

---

## What Kew Does Differently

### Kew is process-first, framework-never.

No topologies. No consensus protocols. No byzantine fault tolerance. No JSON state files pretending to be orchestration.

A worker calls an LLM. A database tracks the work. A CLI lets you use it. That's the whole thing.

---

## Architecture

```
User / Claude Code
       |
       v
+----------------------------------------------+
|  CLI (kew)                                    |
|  - kew run --model gemma4 --wait "prompt"     |
|  - kew chain --step "A" --step "B"            |
|  - kew status                                 |
+------------------+---------------------------+
                   |
                   v
+----------------------------------------------+
|  Coordinator (long-running daemon)            |
|  - Pulls tasks from DB queue                  |
|  - Spawns worker threads                      |
|  - Routes models (local vs API)               |
|  - Manages worker pool (N concurrent)         |
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
|  Shared SQLite DB (single file, WAL mode)     |
|  - tasks: queue, status, assignment           |
|  - results: agent output                      |
|  - context: shared knowledge between agents   |
|  - messages: inter-agent communication        |
|  - file_locks: prevent edit conflicts         |
+----------------------------------------------+
```

---

## The Database Is The Bus

No message brokers. No Redis. No custom IPC. No JSON files. SQLite WAL mode gives you concurrent readers + one writer, which is exactly what an agent pool needs.

```sql
-- Task queue
CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  parent_id TEXT,
  status TEXT DEFAULT 'pending',  -- pending | assigned | running | done | failed
  agent_id TEXT,
  model TEXT,                     -- 'gemma4', 'codellama', 'claude-sonnet'
  system_prompt TEXT,
  prompt TEXT,
  result TEXT,
  error TEXT,
  context_keys TEXT,              -- JSON array of context keys this task needs
  files_locked TEXT,              -- JSON array of files this agent owns
  created_at INTEGER DEFAULT (unixepoch('now')),
  started_at INTEGER,
  completed_at INTEGER
);

-- Shared context between agents
CREATE TABLE context (
  key TEXT PRIMARY KEY,
  namespace TEXT,
  content TEXT,
  embedding BLOB,                 -- float32 vector for similarity search
  metadata TEXT,                  -- JSON
  created_by TEXT,                -- which agent/task wrote this
  created_at INTEGER DEFAULT (unixepoch('now'))
);

-- Inter-agent messages
CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  from_agent TEXT,
  to_agent TEXT,                  -- NULL = broadcast
  content TEXT,
  read INTEGER DEFAULT 0,
  created_at INTEGER DEFAULT (unixepoch('now'))
);

-- File locks (prevent two agents editing same file)
CREATE TABLE file_locks (
  file_path TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  locked_at INTEGER DEFAULT (unixepoch('now'))
);

-- Indexes
CREATE INDEX idx_tasks_status ON tasks(status);
CREATE INDEX idx_tasks_parent ON tasks(parent_id);
CREATE INDEX idx_context_namespace ON context(namespace);
CREATE INDEX idx_messages_to ON messages(to_agent, read);
```

**Why SQLite:**

- Single file, zero configuration, no server process
- WAL mode = concurrent reads + serial writes (perfect for agent pools)
- Survives crashes (tasks stay in DB if worker dies)
- Inspectable: `sqlite3 kew.db "SELECT status, count(*) FROM tasks GROUP BY status"`
- `better-sqlite3` in Node.js: synchronous API, fast, no callback hell
- Workers read context while coordinator writes tasks, no contention

---

## How Claude Code Waits For Sub-Agents

### Recommended: Blocking CLI (simplest, works today)

```bash
# Claude Code runs this via Bash tool:
kew run --model gemma4 --wait "Refactor the auth module to use JWT"
# Blocks until Gemma 4 returns. Result prints to stdout.
# Claude Code reads stdout as the tool result. Done.
```

This is the entire integration. No MCP server, no polling, no callbacks. Claude Code's Bash tool runs a blocking command and reads the output.

### For parallel work:

```bash
# Claude Code runs this:
kew run --parallel --wait \
  --task "Write unit tests for user.ts:gemma4" \
  --task "Write unit tests for auth.ts:gemma4" \
  --task "Security review of api.ts:gemma4"
# Spawns 3 workers, waits for ALL to finish, prints all results.
```

### For task chains (output feeds into next step):

```bash
kew chain --wait \
  --step "Analyze the codebase structure:gemma4" \
  --step "Design a refactoring plan based on the analysis:gemma4" \
  --step "Implement the refactoring:gemma4"
# Each step's result is stored as context for the next step.
```

### MCP server (for tighter integration later)

```bash
kew mcp start
# Exposes tools: kew_submit, kew_result, kew_status
# Claude Code calls these as MCP tools instead of Bash
```

Build this last. The CLI is sufficient for v1.

---

## Worker Implementation

This is the part everyone skips. It's the whole point of the tool.

```typescript
// worker.ts — a real process that calls a real LLM

interface Task {
  id: string;
  model: string;
  systemPrompt: string;
  prompt: string;
  contextKeys?: string[];
  shareResult?: boolean;
  namespace?: string;
}

class AgentWorker {
  private db: Database;

  constructor(
    private ollamaUrl: string,
    dbPath: string,
  ) {
    this.db = new Database(dbPath);
    this.db.pragma("journal_mode = WAL");
  }

  async execute(task: Task): Promise<string> {
    // 1. Load shared context this task needs
    const context = this.loadContext(task.contextKeys || []);

    // 2. Build messages with context injected
    const messages: ChatMessage[] = [
      { role: "system", content: task.systemPrompt },
    ];

    for (const c of context) {
      messages.push({
        role: "user",
        content: `[Shared context: ${c.key}]\n${c.content}`,
      });
    }

    messages.push({ role: "user", content: task.prompt });

    // 3. ACTUALLY CALL THE LLM
    const response = await fetch(`${this.ollamaUrl}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model: task.model,
        messages,
        stream: false,
        options: { temperature: 0.3, num_predict: 4096 },
      }),
    });

    if (!response.ok) {
      throw new Error(
        `Ollama error: ${response.status} ${await response.text()}`,
      );
    }

    const data = await response.json();
    const result = data.message.content;

    // 4. Mark task done in DB
    this.db
      .prepare(
        "UPDATE tasks SET status = ?, result = ?, completed_at = unixepoch() WHERE id = ?",
      )
      .run("done", result, task.id);

    // 5. Optionally share result as context for other agents
    if (task.shareResult) {
      this.storeContext(
        `result:${task.id}`,
        task.namespace || "default",
        result,
        task.model,
      );
    }

    return result;
  }

  private loadContext(keys: string[]): Array<{ key: string; content: string }> {
    if (keys.length === 0) return [];
    const placeholders = keys.map(() => "?").join(",");
    return this.db
      .prepare(
        `SELECT key, content FROM context WHERE key IN (${placeholders})`,
      )
      .all(...keys) as Array<{ key: string; content: string }>;
  }

  private storeContext(
    key: string,
    namespace: string,
    content: string,
    agent: string,
  ): void {
    this.db
      .prepare(
        `INSERT OR REPLACE INTO context (key, namespace, content, created_by) VALUES (?, ?, ?, ?)`,
      )
      .run(key, namespace, content, agent);
  }
}
```

---

## Solving Context Passing (The Hard Problem)

Local models have limited context windows. The shared DB solves this with three patterns:

### Pattern 1: Explicit context keys (default)

```
Task A: "Analyze auth module" -> result stored as context key "auth-analysis"
Task B: "Write tests" -> loads context key "auth-analysis" before prompting
```

Each agent gets only what it needs. Configured per-task:

```bash
kew run --model gemma4 --share-as "auth-analysis" --wait "Analyze the auth module"
kew run --model gemma4 --context "auth-analysis" --wait "Write tests based on the analysis"
```

### Pattern 2: Summary compaction

When an agent produces too much output for the next agent's context:

```bash
kew chain --wait --compact \
  --step "Full analysis of the codebase (be thorough):gemma4" \
  --step "Design refactoring plan:gemma4"
# --compact flag: between steps, run a summarization pass to compress
# 5000 tokens -> 500 token summary -> fits in next agent's context
```

### Pattern 3: Embedding-based retrieval (v2)

For large accumulated context pools, add vector similarity search:

```bash
kew context search "authentication patterns" --top-k 5
# Returns 5 most relevant context entries by cosine similarity
```

This requires embeddings (Ollama supports them via `/api/embeddings`). Build this in v2, not v1.

**For Gemma 4 specifically:** Pattern 1 is usually sufficient. Gemma 4 has a decent context window and good instruction following. Start with explicit context keys.

---

## Model Routing

```typescript
// Simple but real routing
const ROUTING_RULES = {
  // Fast local work
  simple: { model: "gemma4", reason: "fast, free, good enough" },
  code: { model: "gemma4", reason: "good at code generation" },
  test: { model: "gemma4", reason: "test writing is formulaic" },

  // Needs strong reasoning
  architect: { model: "claude-sonnet", reason: "complex system design" },
  security: { model: "claude-sonnet", reason: "safety-critical analysis" },
};

function autoRoute(taskPrompt: string): string {
  // Start simple. Get fancy later if needed.
  if (taskPrompt.includes("security") || taskPrompt.includes("vulnerability")) {
    return "claude-sonnet";
  }
  if (
    taskPrompt.includes("architect") ||
    taskPrompt.includes("design system")
  ) {
    return "claude-sonnet";
  }
  return "gemma4"; // default to local
}
```

---

## File Coordination

Two agents cannot edit the same file:

```typescript
function acquireLock(db: Database, filePath: string, agentId: string): boolean {
  // Atomic: INSERT OR IGNORE ensures only one agent gets the lock
  const result = db
    .prepare(
      "INSERT OR IGNORE INTO file_locks (file_path, agent_id) VALUES (?, ?)",
    )
    .run(filePath, agentId);
  return result.changes > 0;
}

function releaseLock(db: Database, filePath: string, agentId: string): void {
  db.prepare("DELETE FROM file_locks WHERE file_path = ? AND agent_id = ?").run(
    filePath,
    agentId,
  );
}

// Stale lock cleanup (if a worker crashes)
function cleanStaleLocks(db: Database, maxAgeSeconds: number = 300): void {
  db.prepare("DELETE FROM file_locks WHERE locked_at < unixepoch() - ?").run(
    maxAgeSeconds,
  );
}
```

---

## CLI Interface

```bash
# Daemon management
kew start                    # Start coordinator daemon
kew stop                     # Stop daemon gracefully
kew status                   # Show workers, pending tasks, DB stats

# Run tasks
kew run --model gemma4 --wait "prompt here"
kew run --model gemma4 --wait --system "You are a senior engineer" "prompt"
kew run --model gemma4 --wait --file ./prompt.md   # read prompt from file

# Parallel tasks
kew run --parallel --wait \
  --task "task 1:gemma4" \
  --task "task 2:gemma4" \
  --task "task 3:codellama"

# Task chains
kew chain --wait \
  --step "analyze:gemma4" \
  --step "plan:gemma4" \
  --step "implement:gemma4"

# Context management
kew context list
kew context get <key>
kew context set <key> <value>
kew context search "query" --top-k 5   # (v2: needs embeddings)

# Async mode (for advanced use)
kew submit --model gemma4 "prompt"      # returns task ID
kew result <task-id>                    # check result
kew wait <task-id>                      # block until done
kew list                                # list all tasks
```

---

## Project Structure

```
kew/
  package.json
  tsconfig.json
  src/
    cli.ts              # Commander.js entry point          (~150 lines)
    coordinator.ts      # Task queue, worker pool mgmt      (~200 lines)
    worker.ts           # LLM execution (the real part)     (~150 lines)
    db.ts               # SQLite schema + typed queries      (~100 lines)
    models.ts           # Ollama client + model routing      (~100 lines)
    context.ts          # Shared context read/write          (~80 lines)
    locks.ts            # File lock management               (~50 lines)
    types.ts            # Shared type definitions             (~50 lines)
  tests/
    coordinator.test.ts
    worker.test.ts
    context.test.ts
    integration.test.ts
```

---

## Dependencies (minimal)

```json
{
  "name": "kew",
  "version": "0.1.0",
  "bin": { "kew": "./dist/cli.js" },
  "dependencies": {
    "better-sqlite3": "^11.0.0",
    "commander": "^12.0.0"
  },
  "devDependencies": {
    "typescript": "^5.0.0",
    "@types/better-sqlite3": "^7.0.0",
    "vitest": "^2.0.0"
  }
}
```

Two runtime dependencies. That's it.

---

## Build Order (Priority)

| Phase | What                            | Lines | Validates                           |
| ----- | ------------------------------- | ----- | ----------------------------------- |
| 1     | DB schema + types               | ~150  | Schema creates, queries work        |
| 2     | Worker (calls Ollama)           | ~150  | Gemma 4 returns real output         |
| 3     | Coordinator (task queue + pool) | ~200  | Tasks dequeue and execute           |
| 4     | CLI (`kew run --wait`)          | ~150  | End-to-end: CLI to Ollama to stdout |
| 5     | Context sharing                 | ~80   | Agent B reads Agent A's output      |
| 6     | File locking                    | ~50   | Two agents can't edit same file     |
| 7     | Parallel + chain modes          | ~100  | Multi-task workflows work           |
| 8     | MCP server (optional)           | ~150  | Claude Code uses as MCP tool        |

### Validation test after Phase 4:

```bash
ollama pull gemma4
kew start
kew run --model gemma4 --wait "Write a Python function that checks if a number is prime"
# If you get real code back from Gemma 4: it works.
# If you get a JSON file saying an agent exists: you built ruflo again.
```

---

## Key Principles

1. **Process first, framework never.** A worker that calls `fetch('localhost:11434')` is worth more than 10,000 lines of topology management.
2. **SQLite is the bus.** One file, WAL mode. No message brokers, no Redis, no custom IPC.
3. **Blocking CLI for Claude Code.** `kew run --wait` via Bash tool. Read stdout. Done.
4. **Honest about capabilities.** If it supports 3 models, say 3. Not 60.
5. **Two runtime dependencies.** `better-sqlite3` + `commander`. Everything else is unnecessary.
6. **Validate with real LLM calls.** If Gemma 4 doesn't return text, it's not working.
