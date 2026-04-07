# kew Improvement Plan

Three self-contained workstreams. Each can be implemented independently.

---

## 1. Statusline Colors & Symbols

**File:** `.claude/kew-statusline.sh`

Claude Code statuslines support ANSI escape sequences. The current script outputs plain text with emoji — it works but has no visual hierarchy.

### Current output
```
◆ kew  ▶2 ⏳1 ✓14  ctx:3 emb:42  db:1.2M
```

### Target output
Running agents pulse visually, failed count is red-bold, idle is muted.

```
◆ kew  \e[1;36m▶2\e[0m \e[33m⏳1\e[0m \e[32m✓14\e[0m  ctx:3 emb:42  db:1.2M
```

### Changes

Replace the `parts` assembly block in `kew-statusline.sh`:

```sh
BOLD='\033[1m'
RED='\033[31m'
GREEN='\033[32m'
YELLOW='\033[33m'
CYAN='\033[36m'
DIM='\033[2m'
RESET='\033[0m'

parts=""
if [ "${running:-0}" -gt 0 ]; then
  parts="${parts}${CYAN}${BOLD}▶${running}${RESET} "
fi
if [ "${pending:-0}" -gt 0 ]; then
  parts="${parts}${YELLOW}⏳${pending}${RESET} "
fi
if [ "${done_count:-0}" -gt 0 ]; then
  parts="${parts}${GREEN}✓${done_count}${RESET} "
fi
if [ "${failed:-0}" -gt 0 ]; then
  parts="${parts}${RED}${BOLD}✗${failed}${RESET} "
fi
if [ -z "$parts" ]; then
  parts="${DIM}idle${RESET} "
fi
parts="${parts} ${DIM}ctx:${context:-0} emb:${embeddings:-0} db:${db_size:-?}${RESET}"
if [ -n "$agents" ]; then
  parts="${parts} ${DIM}[${agents}]${RESET}"
fi
```

Also add the active agent name to the porcelain output (requires a small change to
`src/cli/status.rs` to emit `agent=<name>` for the single running task if any).

**Effort:** 1–2 hours, zero Rust changes needed for basic colors.

---

## 2. File Reading for Agents (`kew_read_file` MCP tool + `files` in `kew_run`)

### Problem

Agents are prompt-in / text-out with no filesystem access. Claude Code must manually
read files and pre-load them via `kew_context_set` before calling `kew_run`. This is
tedious and prevents agents from self-directing their research.

### Solution: two-part

#### Part A — `kew_read_file` MCP tool

A new tool Claude Code can call to read a file and get its content directly, without
using the Read tool itself. Primarily useful for loading file content into `kew_context_set`
in one step, or passing inline to `kew_run`'s `system` field.

```rust
// src/mcp/server.rs — new tool
#[derive(Deserialize, schemars::JsonSchema)]
struct ReadFileParams {
    /// Path to read. Absolute or relative to the project root.
    path: String,
    /// Optional line range start (1-indexed, inclusive)
    start_line: Option<usize>,
    /// Optional line range end (1-indexed, inclusive)
    end_line: Option<usize>,
}

#[derive(Serialize, schemars::JsonSchema)]
struct ReadFileResult {
    path: String,
    content: String,
    lines: usize,
    truncated: bool,
}
```

Security constraint: resolve the path against the project root (cwd) and reject any
path that escapes it via `..` traversal. Hard cap at 200 KB / ~5000 lines to avoid
flooding context.

#### Part B — `files` field in `kew_run`

Allow `kew_run` to auto-load files into the agent's context without a separate
`kew_context_set` round-trip. The worker reads each listed path and prepends the
content as a user message before the prompt.

```rust
// Add to RunParams:
/// File paths to auto-read and inject as context before the prompt.
/// Paths are relative to the project root. Max 10 files, 100 KB each.
#[serde(default)]
files: Vec<String>,
```

Worker change in `src/worker/worker.rs` — after loading DB context keys, before
building the message array:

```rust
for path in &task.files {
    let abs = project_root.join(path);
    if let Ok(content) = std::fs::read_to_string(&abs) {
        let truncated = truncate_to_limit(&content, 100_000);
        messages.push(ChatMessage {
            role: "user".into(),
            content: format!("[File: {path}]\n```\n{truncated}\n```"),
        });
    }
}
```

#### Workflow after this change

```jsonc
// Before: Claude manually pre-loads
kew_context_set({ key: "src", content: "<entire file>" })
kew_run({ prompt: "Review auth.rs for bugs", context: ["src"] })

// After: one call, agent gets the file directly
kew_run({ prompt: "Review auth.rs for bugs", files: ["src/auth.rs"] })
```

### Files to change

| File | Change |
|------|--------|
| `src/mcp/server.rs` | Add `kew_read_file` tool; add `files` to `RunParams` |
| `src/db/models.rs` | Add `files: Vec<String>` to `NewTask` and `Task` |
| `src/db/schema.rs` | Add `files_to_read TEXT` column (JSON array) to tasks table, migration |
| `src/worker/worker.rs` | Read and inject files before building message array |

**Effort:** ~1 day. The DB schema change needs a migration version bump.

---

## 3. Vector DB Indexing — Pre-train + Adaptive Live Updates

### Problem

The vector DB only grows from completed task results. A fresh project has zero
embeddings, so `kew_context_search` returns nothing useful until many tasks have
run. Agents cannot find relevant files via semantic search — the corpus doesn't
include source files at all.

### Solution: `kew index` command with optional watch mode

#### 3a. `kew index <path>` — one-shot indexing

Walk a directory tree, read eligible files, embed each, store with
`source_type = "file"` and `key = "file:<relative-path>"`.

```
kew index src/                    # index all source files
kew index . --ext rs,md,toml      # only these extensions
kew index src/ --force            # re-embed even if already indexed
```

Implementation sketch:

```rust
// src/cli/index.rs (new file)
pub async fn run_index(args: IndexArgs) -> anyhow::Result<()> {
    let root = args.path.canonicalize()?;
    let files = collect_files(&root, &args.extensions, &args.ignore);
    let ollama = OllamaClient::new(&args.ollama_url);
    let conn = open_db()?;

    let bar = ProgressBar::new(files.len() as u64);
    for path in files {
        let key = format!("file:{}", path.strip_prefix(&root)?.display());

        // Skip if already indexed (unless --force)
        if !args.force && embedding_exists(&conn, &key)? {
            bar.inc(1);
            continue;
        }

        let content = fs::read_to_string(&path)?;
        let truncated = truncate_to_tokens(&content, 4096); // ~16 KB
        let embedding = ollama.embed("nomic-embed-text", &[truncated]).await?;
        store_embedding(&conn, &key, "file", None, &embedding[0], "nomic-embed-text")?;

        // Also store raw content in context table so agents can retrieve it
        put_context(&conn, &key, "file", &content, None)?;

        bar.inc(1);
    }
    println!("Indexed {} files", files.len());
    Ok(())
}
```

File eligibility:
- Respects `.gitignore` (use the `ignore` crate, already common in Rust tooling)
- Default extensions: `rs, ts, js, go, py, md, toml, yaml, json, sql`
- Skip: binary files, files > 500 KB, `target/`, `node_modules/`, `.git/`

#### 3b. `kew index --watch` — adaptive live updates

After initial indexing, watch the directory for changes using the `notify` crate.
On file write/create/rename events, re-embed and update the context entry.

```
kew index src/ --watch            # index then watch indefinitely
kew index src/ --watch --daemon   # same, detached to background
```

Implementation:

```rust
use notify::{RecommendedWatcher, RecursiveMode, Watcher, Event, EventKind};

let (tx, rx) = std::sync::mpsc::channel();
let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
watcher.watch(&root, RecursiveMode::Recursive)?;

for event in rx {
    if let Ok(Event { kind: EventKind::Modify(_) | EventKind::Create(_), paths, .. }) = event {
        for path in paths {
            if is_eligible(&path) {
                reindex_file(&path, &root, &conn, &ollama).await?;
            }
        }
    }
}
```

#### 3c. `kew_context_search` improvement — return file content

Currently `kew_context_search` returns keys and scores only. After indexing, add a
`include_content: bool` option that returns the stored context entry content alongside
each result. This lets Claude Code or agents retrieve relevant file snippets directly
from a semantic query.

```jsonc
// Call
kew_context_search({ query: "authentication token storage", top_k: 3, include_content: true })

// Returns
{
  "results": [
    { "key": "file:src/auth.rs", "score": 0.92, "content": "pub struct Session { ... }" },
    { "key": "file:src/db/sessions.rs", "score": 0.87, "content": "..." }
  ]
}
```

#### 3d. `kew init` integration

`kew init` should prompt:

```
Index this project's source files now? [y/N]
  This embeds your codebase so agents can search it semantically.
  Takes ~30s for most projects.
```

If yes, run `kew index .` immediately after init. Add `--no-index` flag to skip.

### Files to change / add

| File | Change |
|------|--------|
| `src/cli/index.rs` | New — index command implementation |
| `src/cli/mod.rs` | Register `index` subcommand |
| `src/main.rs` | Dispatch `index` subcommand |
| `src/db/vectors.rs` | Add `embedding_exists()` helper |
| `src/mcp/server.rs` | Add `include_content` to `kew_context_search` |
| `src/cli/init.rs` | Add post-init indexing prompt |
| `Cargo.toml` | Add `notify`, `ignore`, `indicatif` crates |

**Effort:** ~2–3 days. The watch mode is optional; the one-shot indexer is the priority.

---

## Implementation Order

1. **Statusline colors** — 2 hours, pure shell, zero risk, immediate visual benefit
2. **`kew_read_file` + `files` in `kew_run`** — 1 day, unlocks real agent file access
3. **`kew index` one-shot** — 1 day, makes semantic search actually useful
4. **`kew index --watch`** — 1 day, adaptive updates (can ship separately)
5. **`kew_context_search` with content** — half day, pairs with (3)

Items 2 and 3 are the highest leverage. An agent that can read files and search the
codebase semantically is qualitatively more useful than one that can only consume
pre-loaded context blobs.
