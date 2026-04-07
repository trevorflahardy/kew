//! Context key-value store operations.

use rusqlite::{params, Connection, OptionalExtension};

use super::models::ContextEntry;

fn context_from_row(row: &rusqlite::Row) -> rusqlite::Result<ContextEntry> {
    Ok(ContextEntry {
        key: row.get("key")?,
        namespace: row.get("namespace")?,
        content: row.get("content")?,
        content_hash: row.get("content_hash")?,
        summary: row.get("summary")?,
        metadata: row.get("metadata")?,
        created_by: row.get("created_by")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// Store or update a context entry.
pub fn put_context(
    conn: &Connection,
    key: &str,
    namespace: &str,
    content: &str,
    created_by: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO context (key, namespace, content, created_by)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(key) DO UPDATE SET
             content = excluded.content,
             updated_at = unixepoch('now')",
        params![key, namespace, content, created_by],
    )?;
    Ok(())
}

/// Retrieve a context entry by key.
pub fn get_context(conn: &Connection, key: &str) -> rusqlite::Result<Option<ContextEntry>> {
    conn.query_row(
        "SELECT * FROM context WHERE key = ?1",
        params![key],
        context_from_row,
    )
    .optional()
}

/// Load multiple context entries by keys.
///
/// Keys are chunked into batches of 500 to stay well under SQLite's
/// SQLITE_MAX_VARIABLE_NUMBER limit (default 999).
pub fn get_context_many(conn: &Connection, keys: &[String]) -> rusqlite::Result<Vec<ContextEntry>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }

    let mut all_entries = Vec::new();
    for chunk in keys.chunks(500) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT * FROM context WHERE key IN ({})",
            placeholders.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|k| k as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), context_from_row)?;
        for row in rows {
            all_entries.push(row?);
        }
    }
    Ok(all_entries)
}

/// List all context entries, optionally filtered by namespace.
pub fn list_context(
    conn: &Connection,
    namespace: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<ContextEntry>> {
    let mut entries = Vec::new();

    if let Some(ns) = namespace {
        let mut stmt = conn.prepare(
            "SELECT * FROM context WHERE namespace = ?1 ORDER BY updated_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![ns, limit as i64], context_from_row)?;
        for row in rows {
            entries.push(row?);
        }
    } else {
        let mut stmt = conn.prepare("SELECT * FROM context ORDER BY updated_at DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], context_from_row)?;
        for row in rows {
            entries.push(row?);
        }
    }

    Ok(entries)
}

/// Delete a context entry by key.
pub fn delete_context(conn: &Connection, key: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute("DELETE FROM context WHERE key = ?1", params![key])?;
    Ok(changed > 0)
}

/// Delete all context entries in a namespace (or all if namespace is None).
pub fn clear_context(conn: &Connection, namespace: Option<&str>) -> rusqlite::Result<usize> {
    if let Some(ns) = namespace {
        conn.execute("DELETE FROM context WHERE namespace = ?1", params![ns])
    } else {
        conn.execute("DELETE FROM context", [])
    }
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

    #[test]
    fn test_put_and_get() {
        let conn = test_conn();
        put_context(
            &conn,
            "auth-analysis",
            "default",
            "The auth module uses JWT.",
            Some("task-1"),
        )
        .unwrap();

        let entry = get_context(&conn, "auth-analysis").unwrap().unwrap();
        assert_eq!(entry.key, "auth-analysis");
        assert_eq!(entry.content, "The auth module uses JWT.");
        assert_eq!(entry.namespace, "default");
        assert_eq!(entry.created_by.as_deref(), Some("task-1"));
    }

    #[test]
    fn test_upsert() {
        let conn = test_conn();
        put_context(&conn, "key1", "default", "version 1", None).unwrap();
        put_context(&conn, "key1", "default", "version 2", None).unwrap();

        let entry = get_context(&conn, "key1").unwrap().unwrap();
        assert_eq!(entry.content, "version 2");
    }

    #[test]
    fn test_get_many() {
        let conn = test_conn();
        put_context(&conn, "a", "default", "content a", None).unwrap();
        put_context(&conn, "b", "default", "content b", None).unwrap();
        put_context(&conn, "c", "default", "content c", None).unwrap();

        let entries = get_context_many(&conn, &["a".into(), "c".into()]).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_list_and_filter() {
        let conn = test_conn();
        put_context(&conn, "k1", "ns1", "content", None).unwrap();
        put_context(&conn, "k2", "ns2", "content", None).unwrap();

        let all = list_context(&conn, None, 10).unwrap();
        assert_eq!(all.len(), 2);

        let ns1 = list_context(&conn, Some("ns1"), 10).unwrap();
        assert_eq!(ns1.len(), 1);
        assert_eq!(ns1[0].key, "k1");
    }

    #[test]
    fn test_delete() {
        let conn = test_conn();
        put_context(&conn, "key1", "default", "content", None).unwrap();

        assert!(delete_context(&conn, "key1").unwrap());
        assert!(!delete_context(&conn, "key1").unwrap());
        assert!(get_context(&conn, "key1").unwrap().is_none());
    }

    #[test]
    fn test_clear() {
        let conn = test_conn();
        put_context(&conn, "a", "ns1", "c", None).unwrap();
        put_context(&conn, "b", "ns2", "c", None).unwrap();

        let cleared = clear_context(&conn, Some("ns1")).unwrap();
        assert_eq!(cleared, 1);
        assert_eq!(list_context(&conn, None, 10).unwrap().len(), 1);
    }
}
