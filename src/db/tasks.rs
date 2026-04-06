//! Task CRUD operations against SQLite.

use rusqlite::{params, Connection, OptionalExtension};

use super::models::{NewTask, Provider, Task, TaskStatus};

/// Parse a JSON array string into a Vec<String>, returning empty vec on failure.
fn parse_json_array(s: Option<&str>) -> Vec<String> {
    s.and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default()
}

/// Read a Task from a rusqlite Row.
fn task_from_row(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    let context_keys_raw: Option<String> = row.get("context_keys")?;
    let files_locked_raw: Option<String> = row.get("files_locked")?;
    let status_raw: String = row.get("status")?;
    let provider_raw: String = row.get("provider")?;

    Ok(Task {
        id: row.get("id")?,
        parent_id: row.get("parent_id")?,
        chain_id: row.get("chain_id")?,
        chain_index: row.get("chain_index")?,
        status: TaskStatus::from_str_lossy(&status_raw),
        model: row.get("model")?,
        provider: Provider::from_str_lossy(&provider_raw),
        system_prompt: row.get("system_prompt")?,
        prompt: row.get("prompt")?,
        result: row.get("result")?,
        error: row.get("error")?,
        context_keys: parse_json_array(context_keys_raw.as_deref()),
        share_as: row.get("share_as")?,
        files_locked: parse_json_array(files_locked_raw.as_deref()),
        worker_id: row.get("worker_id")?,
        created_at: row.get("created_at")?,
        started_at: row.get("started_at")?,
        completed_at: row.get("completed_at")?,
        prompt_tokens: row.get("prompt_tokens")?,
        completion_tokens: row.get("completion_tokens")?,
        duration_ms: row.get("duration_ms")?,
    })
}

/// Insert a new task into the queue.
pub fn create_task(conn: &Connection, new: &NewTask) -> rusqlite::Result<Task> {
    let id = ulid::Ulid::new().to_string();
    let context_keys_json = serde_json::to_string(&new.context_keys).unwrap_or_default();
    let files_locked_json = serde_json::to_string(&new.files_locked).unwrap_or_default();

    conn.execute(
        "INSERT INTO tasks (id, parent_id, chain_id, chain_index, model, provider, system_prompt, prompt, context_keys, share_as, files_locked)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            new.parent_id,
            new.chain_id,
            new.chain_index,
            new.model,
            new.provider.to_string(),
            new.system_prompt,
            new.prompt,
            context_keys_json,
            new.share_as,
            files_locked_json,
        ],
    )?;

    get_task(conn, &id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// Fetch a task by ID.
pub fn get_task(conn: &Connection, id: &str) -> rusqlite::Result<Option<Task>> {
    conn.query_row("SELECT * FROM tasks WHERE id = ?1", params![id], task_from_row)
        .optional()
}

/// Atomically claim the next pending task for a worker.
///
/// Uses a single UPDATE...RETURNING statement so two workers never claim
/// the same task — SQLite guarantees statement-level atomicity.
pub fn claim_next_pending(conn: &Connection, worker_id: &str) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        "UPDATE tasks
         SET status = 'assigned', worker_id = ?1, started_at = unixepoch('now')
         WHERE id = (
             SELECT id FROM tasks
             WHERE status = 'pending'
             ORDER BY created_at ASC
             LIMIT 1
         )
         RETURNING *",
        params![worker_id],
        task_from_row,
    )
    .optional()
}

/// Transition a task to 'running'.
pub fn mark_running(conn: &Connection, task_id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE tasks SET status = 'running' WHERE id = ?1 AND status = 'assigned'",
        params![task_id],
    )
}

/// Mark a task as done with its result.
pub fn mark_done(
    conn: &Connection,
    task_id: &str,
    result: &str,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    duration_ms: Option<i64>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE tasks SET status = 'done', result = ?2, completed_at = unixepoch('now'),
         prompt_tokens = ?3, completion_tokens = ?4, duration_ms = ?5
         WHERE id = ?1 AND status = 'running'",
        params![task_id, result, prompt_tokens, completion_tokens, duration_ms],
    )
}

/// Mark a task as failed with an error message.
pub fn mark_failed(conn: &Connection, task_id: &str, error: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE tasks SET status = 'failed', error = ?2, completed_at = unixepoch('now') WHERE id = ?1 AND status IN ('assigned', 'running')",
        params![task_id, error],
    )
}

/// List tasks, optionally filtered by status.
pub fn list_tasks(
    conn: &Connection,
    status: Option<&TaskStatus>,
    limit: usize,
) -> rusqlite::Result<Vec<Task>> {
    let mut tasks = Vec::new();

    if let Some(s) = status {
        let mut stmt =
            conn.prepare("SELECT * FROM tasks WHERE status = ?1 ORDER BY created_at DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![s.to_string(), limit as i64], task_from_row)?;
        for row in rows {
            tasks.push(row?);
        }
    } else {
        let mut stmt = conn.prepare("SELECT * FROM tasks ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], task_from_row)?;
        for row in rows {
            tasks.push(row?);
        }
    }

    Ok(tasks)
}

/// Count tasks grouped by status.
pub fn count_by_status(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT status, count(*) FROM tasks GROUP BY status")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::run_migrations(&conn).unwrap();
        conn
    }

    fn sample_new_task() -> NewTask {
        NewTask {
            model: "gemma4:26b".into(),
            provider: Provider::Ollama,
            prompt: "Write a hello world in Python".into(),
            system_prompt: None,
            context_keys: vec![],
            share_as: None,
            files_locked: vec![],
            parent_id: None,
            chain_id: None,
            chain_index: None,
        }
    }

    #[test]
    fn test_create_and_get_task() {
        let conn = test_conn();
        let task = create_task(&conn, &sample_new_task()).unwrap();

        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.model, "gemma4:26b");
        assert_eq!(task.prompt, "Write a hello world in Python");
        assert!(task.result.is_none());

        let fetched = get_task(&conn, &task.id).unwrap().unwrap();
        assert_eq!(fetched.id, task.id);
    }

    #[test]
    fn test_claim_next_pending() {
        let conn = test_conn();
        let task = create_task(&conn, &sample_new_task()).unwrap();

        let claimed = claim_next_pending(&conn, "worker-1").unwrap().unwrap();
        assert_eq!(claimed.id, task.id);
        assert_eq!(claimed.status, TaskStatus::Assigned);
        assert_eq!(claimed.worker_id.as_deref(), Some("worker-1"));
        assert!(claimed.started_at.is_some());

        // Second claim returns None — no more pending tasks
        let second = claim_next_pending(&conn, "worker-2").unwrap();
        assert!(second.is_none());
    }

    #[test]
    fn test_no_double_claim() {
        let conn = test_conn();
        create_task(&conn, &sample_new_task()).unwrap();

        let a = claim_next_pending(&conn, "w1").unwrap();
        let b = claim_next_pending(&conn, "w2").unwrap();
        assert!(a.is_some());
        assert!(b.is_none());
    }

    #[test]
    fn test_mark_done() {
        let conn = test_conn();
        let task = create_task(&conn, &sample_new_task()).unwrap();
        claim_next_pending(&conn, "w1").unwrap();
        mark_running(&conn, &task.id).unwrap();
        mark_done(&conn, &task.id, "print('hello')", Some(10), Some(20), Some(500)).unwrap();

        let done = get_task(&conn, &task.id).unwrap().unwrap();
        assert_eq!(done.status, TaskStatus::Done);
        assert_eq!(done.result.as_deref(), Some("print('hello')"));
        assert_eq!(done.prompt_tokens, Some(10));
        assert!(done.completed_at.is_some());
    }

    #[test]
    fn test_mark_failed() {
        let conn = test_conn();
        let task = create_task(&conn, &sample_new_task()).unwrap();
        claim_next_pending(&conn, "w1").unwrap();
        mark_running(&conn, &task.id).unwrap();
        mark_failed(&conn, &task.id, "connection refused").unwrap();

        let failed = get_task(&conn, &task.id).unwrap().unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("connection refused"));
    }

    #[test]
    fn test_list_tasks() {
        let conn = test_conn();
        create_task(&conn, &sample_new_task()).unwrap();
        create_task(&conn, &sample_new_task()).unwrap();

        let all = list_tasks(&conn, None, 10).unwrap();
        assert_eq!(all.len(), 2);

        let pending = list_tasks(&conn, Some(&TaskStatus::Pending), 10).unwrap();
        assert_eq!(pending.len(), 2);

        let done = list_tasks(&conn, Some(&TaskStatus::Done), 10).unwrap();
        assert!(done.is_empty());
    }

    #[test]
    fn test_context_keys_roundtrip() {
        let conn = test_conn();
        let mut new = sample_new_task();
        new.context_keys = vec!["auth-analysis".into(), "db-schema".into()];
        new.share_as = Some("test-result".into());

        let task = create_task(&conn, &new).unwrap();
        assert_eq!(task.context_keys, vec!["auth-analysis", "db-schema"]);
        assert_eq!(task.share_as.as_deref(), Some("test-result"));
    }
}
