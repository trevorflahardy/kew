kew — Local Agent Orchestration

kew runs local LLM agents alongside Claude Code. Delegate background research, code generation, testing, and doc tasks to kew rather than doing everything inline.

## MCP Tools (prefer these over CLI)

kew is registered as an MCP server at project init. Call these directly:

| Tool                 | What it does                                                           |
| -------------------- | ---------------------------------------------------------------------- |
| `kew_run`            | Run a prompt through a specialist agent; blocks and returns the result |
| `kew_list_agents`    | List all agents with trigger keywords                                  |
| `kew_context_set`    | Store text under a key for later tasks to load                         |
| `kew_context_get`    | Retrieve stored text by key                                            |
| `kew_context_search` | Semantic search over stored context (vector similarity)                |
| `kew_status`         | Pending/running/done task counts and DB stats                          |
| `kew_doctor`         | Check Ollama connectivity and available models                         |

## Spawning Agents

Pass `agent` explicitly, or let keyword routing pick one automatically:

```jsonc
// explicit
{ "prompt": "Refactor auth.rs to use the new session type", "agent": "developer" }

// auto-routed — 'debug' triggers the debugger agent
{ "prompt": "Debug why the lock is deadlocking in pool.rs" }
```

**Auto-routing keywords** (omit `agent` and these phrases trigger the right specialist):

| Agent          | Trigger keywords                                                                         |
| -------------- | ---------------------------------------------------------------------------------------- |
| `developer`    | implement, build this, write code, add feature, refactor, create a function/struct/class |
| `debugger`     | debug, broken, not working, crash, root cause, diagnose, fix the bug, why is             |
| `docs-writer`  | document, write docs, add docs, explain this, write readme                               |
| `security`     | security, vulnerability, exploit, injection, auth bypass, cve                            |
| `doc-audit`    | doc audit, documentation gap, documentation quality, missing docs, audit doc             |
| `tester`       | write test, add test, unit test, test coverage, test suite, write specs                  |
| `watcher`      | watch, track progress, summarize progress, what's happening, status report, observe      |
| `error-finder` | find error, potential bug, what could go wrong, pre-emptive, review for bug, find bug    |

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

| Request type                         | kew pattern                                                                   |
| ------------------------------------ | ----------------------------------------------------------------------------- |
| Independent parallel work            | Multiple `kew_run` calls with different agents (fire in parallel)             |
| Sequential pipeline                  | `kew chain --step ... --step ...`                                             |
| Research → implement                 | `watcher`/`error-finder` → `share_as` key → `developer` with `context: [key]` |
| Write code → test it → check docs    | `kew chain`: `developer` → `tester` → `doc-audit`                             |
| Answer a question about the codebase | `kew_run` with `agent: watcher`, read the result                              |
| Fix a bug                            | `kew_run` with `agent: debugger` → review output → apply with Edit            |
| Update docs                          | `kew_run` with `agent: docs-writer`, `share_as` → review → write to file      |

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
  fast: gemma3:27b # low-latency: summaries, routing, classification
  code: gemma4:26b # code generation and debugging
  smart: claude-sonnet-4-6 # complex reasoning, architecture decisions
  embed: nomic-embed-text # embeddings only (Ollama)
```

In agent YAMLs use `tier:` not a raw model name so swapping models only requires editing config:

```yaml
name: developer
tier: code # resolved to model at runtime via kew_config.yaml tiers
```

Claude does not auto-select models at runtime — agents declare their tier, you map tiers to models.

## Background Audits — Always Running

**Even when working alone (no subagents), fire background kew checks before finishing any task.**
These are fire-and-forget — launch them, keep working, check results at the end.

| When you…                   | Fire in background                                                                    |
| --------------------------- | ------------------------------------------------------------------------------------- |
| Edit or write code          | `kew_run { agent: "error-finder", prompt: "Review <files> for potential bugs" }`     |
| Touch auth / IO / user input | `kew_run { agent: "security", prompt: "Audit <files> for security issues" }`         |
| Add a feature               | `kew_run { agent: "tester", prompt: "Identify missing test coverage in <files>" }`   |
| Change public APIs / docs   | `kew_run { agent: "doc-audit", prompt: "Check doc quality in <files>" }`             |

Store results with `share_as: "bg/<check>"`. Before closing the task, call `kew_context_get` and surface any findings to the user.

---

## Subteams — Departments & Employees

For tasks with 3+ independent workstreams, spawn a **department lead** Claude subagent per category.
**Each lead's only job is to spawn and manage kew workers — leads do NOT do LLM work themselves.**
Results bubble up through shared context keys.

```
Claude (orchestrator)
├── engineering lead (Claude subagent)  →  developer × N, tester × 1, debugger × 1
├── docs lead (Claude subagent)         →  docs-writer × N, doc-audit × 1
└── security lead (Claude subagent)     →  security × 1, error-finder × 1
```

### Lead Rules — non-negotiable

1. **Leads spawn kew workers, period.** A lead that reads files and writes fixes itself has broken the model — it is a boss, not an individual contributor.
2. **Leads launch all kew workers in parallel**, collect via `kew_context_get`, review output, then return a combined summary to the orchestrator.
3. **Leads never tell workers to skip kew.** Worker prompts must not contain "don't use kew" or "do it directly".
4. **Orchestrator reviews before applying.** Leads return summaries; Claude applies changes after verification.

### Writing a lead's prompt — required structure

```
You are the <dept> lead. Your job:
1. Spawn these kew workers IN PARALLEL (call kew_run for each simultaneously):
   - kew_run { agent: "<agent>", prompt: "<task>", share_as: "<dept>/<key>" }
   - kew_run { agent: "<agent>", prompt: "<task>", share_as: "<dept>/<key>" }
2. Once all complete, retrieve each result with kew_context_get.
3. Review the results for correctness — flag hallucinations or errors.
4. Return a single combined summary to the orchestrator.

Do NOT implement anything yourself. Your value is coordination and review, not execution.
```

**Context key namespacing** — use department paths to avoid collisions:

| Department  | Key pattern    | Example             |
| ----------- | -------------- | ------------------- |
| engineering | `eng/<task>`   | `eng/auth-refactor` |
| docs        | `docs/<topic>` | `docs/api-guide`    |
| security    | `sec/<area>`   | `sec/auth-audit`    |
| qa          | `qa/<target>`  | `qa/worker-pool`    |

**When to use subteams:** 3+ independent workstreams that can run in parallel. For 1-2, prefer `kew chain` or direct parallel `kew_run` calls.

## Custom Agents

Drop a YAML file in `.kew/agents/` to override a built-in or add a new specialist:

```yaml
name: my-agent
description: Short description shown in `kew agent list`
tier: code # use a tier name from kew_config.yaml (preferred over raw model)
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
