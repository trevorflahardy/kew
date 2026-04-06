//! Vector storage and similarity search.
//!
//! Embeddings are stored as f32 BLOBs in SQLite. Cosine similarity
//! is computed in Rust — fast enough for thousands of vectors at
//! 768 dimensions (<1ms).

use rusqlite::{params, Connection, OptionalExtension};

/// A stored embedding with its metadata.
#[derive(Debug, Clone)]
pub struct EmbeddingEntry {
    pub key: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub dimensions: usize,
}

/// A search result with similarity score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub key: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub score: f32,
}

/// Store an embedding vector.
pub fn store_embedding(
    conn: &Connection,
    key: &str,
    source_type: &str,
    source_id: Option<&str>,
    embedding: &[f32],
    model: &str,
) -> rusqlite::Result<()> {
    let blob = embedding_to_blob(embedding);
    conn.execute(
        "INSERT INTO embeddings (key, source_type, source_id, embedding, dimensions, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(key) DO UPDATE SET
             embedding = excluded.embedding,
             dimensions = excluded.dimensions,
             model = excluded.model,
             created_at = unixepoch('now')",
        params![key, source_type, source_id, blob, embedding.len() as i64, model],
    )?;
    Ok(())
}

/// Search for the top-k most similar embeddings by cosine similarity.
pub fn search_similar(
    conn: &Connection,
    query_embedding: &[f32],
    source_type: Option<&str>,
    top_k: usize,
) -> rusqlite::Result<Vec<SearchResult>> {
    let sql = if source_type.is_some() {
        "SELECT key, source_type, source_id, embedding FROM embeddings WHERE source_type = ?1"
    } else {
        "SELECT key, source_type, source_id, embedding FROM embeddings"
    };

    let mut stmt = conn.prepare(sql)?;

    let extract_row = |row: &rusqlite::Row| -> rusqlite::Result<(String, String, Option<String>, Vec<u8>)> {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    };

    let mut scored: Vec<SearchResult> = Vec::new();

    if let Some(st) = source_type {
        let rows = stmt.query_map(params![st], extract_row)?;
        for row in rows {
            let (key, st, sid, blob) = row?;
            let stored = blob_to_embedding(&blob);
            let score = cosine_similarity(query_embedding, &stored);
            scored.push(SearchResult { key, source_type: st, source_id: sid, score });
        }
    } else {
        let rows = stmt.query_map([], extract_row)?;
        for row in rows {
            let (key, st, sid, blob) = row?;
            let stored = blob_to_embedding(&blob);
            let score = cosine_similarity(query_embedding, &stored);
            scored.push(SearchResult { key, source_type: st, source_id: sid, score });
        }
    }

    // Sort by score descending, take top_k
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);

    Ok(scored)
}

/// Delete an embedding by key.
pub fn delete_embedding(conn: &Connection, key: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute("DELETE FROM embeddings WHERE key = ?1", params![key])?;
    Ok(changed > 0)
}

/// Count stored embeddings.
pub fn count_embeddings(conn: &Connection) -> rusqlite::Result<usize> {
    conn.query_row("SELECT COUNT(*) FROM embeddings", [], |row| {
        row.get::<_, i64>(0).map(|c| c as usize)
    })
}

/// Check if an embedding exists for a key.
pub fn has_embedding(conn: &Connection, key: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM embeddings WHERE key = ?1",
        params![key],
        |_| Ok(true),
    )
    .optional()
    .map(|opt| opt.unwrap_or(false))
}

// --- Internal helpers ---

fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
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
    fn test_store_and_search() {
        let conn = test_conn();

        // Store 3 vectors
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![0.9, 0.1, 0.0]; // similar to v1
        let v3 = vec![0.0, 0.0, 1.0]; // orthogonal to v1

        store_embedding(&conn, "task-1", "result", Some("t1"), &v1, "test").unwrap();
        store_embedding(&conn, "task-2", "result", Some("t2"), &v2, "test").unwrap();
        store_embedding(&conn, "task-3", "result", Some("t3"), &v3, "test").unwrap();

        // Search with query similar to v1
        let query = vec![1.0, 0.0, 0.0];
        let results = search_similar(&conn, &query, None, 2).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "task-1"); // exact match
        assert_eq!(results[1].key, "task-2"); // close match
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_filter_by_source_type() {
        let conn = test_conn();

        store_embedding(&conn, "ctx-1", "context", None, &[1.0, 0.0], "test").unwrap();
        store_embedding(&conn, "res-1", "result", Some("t1"), &[1.0, 0.0], "test").unwrap();

        let results = search_similar(&conn, &[1.0, 0.0], Some("result"), 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "res-1");
    }

    #[test]
    fn test_upsert_embedding() {
        let conn = test_conn();

        store_embedding(&conn, "k1", "result", None, &[1.0, 0.0], "test").unwrap();
        store_embedding(&conn, "k1", "result", None, &[0.0, 1.0], "test").unwrap();

        let count = count_embeddings(&conn).unwrap();
        assert_eq!(count, 1);

        // Search should find the updated vector
        let results = search_similar(&conn, &[0.0, 1.0], None, 1).unwrap();
        assert!(results[0].score > 0.99);
    }

    #[test]
    fn test_cosine_similarity_fn() {
        // Identical vectors
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 0.001);
        // Orthogonal vectors
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 0.001);
        // Opposite vectors
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_blob_roundtrip() {
        let original = vec![1.0f32, -2.5, 3.14159, 0.0];
        let blob = embedding_to_blob(&original);
        let recovered = blob_to_embedding(&blob);
        assert_eq!(original, recovered);
    }
}
