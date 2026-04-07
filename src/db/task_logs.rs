//! Append-only log chunks for live task output streaming.
//!
//! Workers write chunks to this table during execution so the TUI can
//! display live progress without waiting for the task to complete.

use rusqlite::{params, Connection};

/// Append a log chunk for a task. Non-fatal — callers should ignore errors.
pub fn append_chunk(conn: &Connection, task_id: &str, chunk: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO task_logs (task_id, chunk) VALUES (?1, ?2)",
        params![task_id, chunk],
    )?;
    Ok(())
}

/// Fetch all log chunks for a task in insertion order.
pub fn get_chunks(conn: &Connection, task_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT chunk FROM task_logs WHERE task_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![task_id], |row| row.get(0))?;
    rows.collect()
}
