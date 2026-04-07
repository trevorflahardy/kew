//! SQL migration strings for kew's database schema.

/// Migration 001: Core tables for task queue, context, file locks.
pub const MIGRATION_001_CORE: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER DEFAULT (unixepoch('now'))
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
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
    context_keys TEXT,
    share_as TEXT,
    files_locked TEXT,
    worker_id TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch('now')),
    started_at INTEGER,
    completed_at INTEGER,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    duration_ms INTEGER
);

CREATE TABLE IF NOT EXISTS context (
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

CREATE TABLE IF NOT EXISTS file_locks (
    file_path TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    locked_at INTEGER NOT NULL DEFAULT (unixepoch('now')),
    expires_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_chain ON tasks(chain_id, chain_index);
CREATE INDEX IF NOT EXISTS idx_tasks_created ON tasks(created_at);
CREATE INDEX IF NOT EXISTS idx_context_namespace ON context(namespace);
CREATE INDEX IF NOT EXISTS idx_file_locks_task ON file_locks(task_id);
"#;

/// Migration 002: Embeddings table for vector search.
pub const MIGRATION_002_VECTORS: &str = r#"
CREATE TABLE IF NOT EXISTS embeddings (
    key TEXT PRIMARY KEY,
    source_type TEXT NOT NULL CHECK(source_type IN ('context','result')),
    source_id TEXT,
    embedding BLOB NOT NULL,
    dimensions INTEGER NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch('now'))
);

CREATE INDEX IF NOT EXISTS idx_embeddings_source ON embeddings(source_type);
"#;

/// Migration 003: Add agent name column to tasks.
pub const MIGRATION_003_AGENT: &str = r#"
ALTER TABLE tasks ADD COLUMN agent TEXT;
"#;

/// Migration 004: Add files_to_read column to tasks for auto file injection.
pub const MIGRATION_004_FILES: &str = r#"
ALTER TABLE tasks ADD COLUMN files_to_read TEXT;
"#;

/// Migration 005: Add 'file' source type to embeddings for project indexing.
///
/// SQLite cannot ALTER a CHECK constraint, so we recreate the table.
pub const MIGRATION_005_FILE_EMBEDDINGS: &str = r#"
PRAGMA foreign_keys = OFF;
CREATE TABLE IF NOT EXISTS embeddings_v2 (
    key TEXT PRIMARY KEY,
    source_type TEXT NOT NULL CHECK(source_type IN ('context','result','file')),
    source_id TEXT,
    embedding BLOB NOT NULL,
    dimensions INTEGER NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch('now'))
);
INSERT OR IGNORE INTO embeddings_v2 SELECT * FROM embeddings;
DROP TABLE embeddings;
ALTER TABLE embeddings_v2 RENAME TO embeddings;
CREATE INDEX IF NOT EXISTS idx_embeddings_source ON embeddings(source_type);
PRAGMA foreign_keys = ON;
"#;

/// All migrations in order.
pub const MIGRATIONS: &[(&str, i64)] = &[
    (MIGRATION_001_CORE, 1),
    (MIGRATION_002_VECTORS, 2),
    (MIGRATION_003_AGENT, 3),
    (MIGRATION_004_FILES, 4),
    (MIGRATION_005_FILE_EMBEDDINGS, 5),
];
