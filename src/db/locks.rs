//! File lock management to prevent concurrent agent edits.

use rusqlite::{params, Connection, OptionalExtension};

use super::models::FileLock;

/// Attempt to acquire an exclusive lock on a file path.
///
/// Uses INSERT OR IGNORE so only the first caller wins — atomic.
/// Returns true if the lock was acquired, false if already held.
pub fn acquire_lock(
    conn: &Connection,
    file_path: &str,
    task_id: &str,
    ttl_seconds: i64,
) -> rusqlite::Result<bool> {
    // Atomic: clean expired + insert in one transaction so no race between DELETE and INSERT
    let tx = conn.unchecked_transaction()?;

    tx.execute(
        "DELETE FROM file_locks WHERE expires_at IS NOT NULL AND expires_at < unixepoch('now')",
        [],
    )?;

    let result = tx.execute(
        "INSERT OR IGNORE INTO file_locks (file_path, task_id, expires_at)
         VALUES (?1, ?2, unixepoch('now') + ?3)",
        params![file_path, task_id, ttl_seconds],
    )?;

    tx.commit()?;
    Ok(result > 0)
}

/// Release a lock held by a specific task.
pub fn release_lock(conn: &Connection, file_path: &str, task_id: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "DELETE FROM file_locks WHERE file_path = ?1 AND task_id = ?2",
        params![file_path, task_id],
    )?;
    Ok(changed > 0)
}

/// Release all locks held by a task.
pub fn release_all_locks(conn: &Connection, task_id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM file_locks WHERE task_id = ?1",
        params![task_id],
    )
}

/// Check who holds the lock on a file. Returns the task ID if locked, None if free.
///
/// Expired locks are treated as free (not returned).
pub fn check_lock(conn: &Connection, file_path: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT task_id FROM file_locks
         WHERE file_path = ?1 AND (expires_at IS NULL OR expires_at >= unixepoch('now'))",
        params![file_path],
        |row| row.get(0),
    )
    .optional()
}

/// Clean up all expired locks.
pub fn clean_expired_locks(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM file_locks WHERE expires_at IS NOT NULL AND expires_at < unixepoch('now')",
        [],
    )
}

/// List all active locks.
pub fn list_locks(conn: &Connection) -> rusqlite::Result<Vec<FileLock>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM file_locks WHERE expires_at IS NULL OR expires_at >= unixepoch('now') ORDER BY locked_at"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FileLock {
            file_path: row.get("file_path")?,
            task_id: row.get("task_id")?,
            locked_at: row.get("locked_at")?,
            expires_at: row.get("expires_at")?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::run_migrations(&conn).unwrap();
        // Insert a dummy task so FK constraint is satisfied
        conn.execute(
            "INSERT INTO tasks (id, model, provider, prompt) VALUES ('t1', 'gemma4', 'ollama', 'test')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, model, provider, prompt) VALUES ('t2', 'gemma4', 'ollama', 'test')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_acquire_and_release() {
        let conn = test_conn();

        assert!(acquire_lock(&conn, "/src/main.rs", "t1", 300).unwrap());
        // Second acquire on same path fails
        assert!(!acquire_lock(&conn, "/src/main.rs", "t2", 300).unwrap());
        // Different path succeeds
        assert!(acquire_lock(&conn, "/src/lib.rs", "t2", 300).unwrap());

        // Release and re-acquire
        assert!(release_lock(&conn, "/src/main.rs", "t1").unwrap());
        assert!(acquire_lock(&conn, "/src/main.rs", "t2", 300).unwrap());
    }

    #[test]
    fn test_release_all() {
        let conn = test_conn();
        acquire_lock(&conn, "/a.rs", "t1", 300).unwrap();
        acquire_lock(&conn, "/b.rs", "t1", 300).unwrap();

        let released = release_all_locks(&conn, "t1").unwrap();
        assert_eq!(released, 2);
        assert!(list_locks(&conn).unwrap().is_empty());
    }

    #[test]
    fn test_list_locks() {
        let conn = test_conn();
        acquire_lock(&conn, "/a.rs", "t1", 300).unwrap();
        acquire_lock(&conn, "/b.rs", "t2", 300).unwrap();

        let locks = list_locks(&conn).unwrap();
        assert_eq!(locks.len(), 2);
    }
}
