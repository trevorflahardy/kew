kew — Local Agent Orchestration

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

Or via the MCP `kew_run` + `context` array when steps share named results.

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
  fast: gemma3:27b         # summaries, routing, classification
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
