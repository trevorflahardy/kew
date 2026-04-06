//! Database layer: SQLite with WAL mode as the coordination bus.
//!
//! All agent coordination flows through this single SQLite file.
//! WAL mode gives concurrent readers + serial writers, which is
//! exactly what an agent pool needs.

pub mod context;
pub mod locks;
pub mod models;
pub mod schema;
pub mod tasks;
pub mod vectors;

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration failed at version {version}: {source}")]
    Migration {
        version: i64,
        source: rusqlite::Error,
    },
}

/// Thread-safe database handle. Wraps a rusqlite Connection in Arc<Mutex<>>
/// for use with tokio::task::spawn_blocking.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open (or create) a database at the given path with WAL mode and migrations.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    DbError::Sqlite(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                        Some(format!("cannot create directory {}: {e}", parent.display())),
                    ))
                })?;
            }
        }

        let conn = Connection::open(path)?;
        configure_connection(&conn)?;
        run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Get a reference to the inner connection (for synchronous operations).
    ///
    /// # Panics
    /// Panics if the mutex is poisoned (another thread panicked while holding it).
    /// In practice this only happens if a previous operation panicked mid-transaction.
    /// Use with `tokio::task::spawn_blocking` in async contexts — do NOT hold the
    /// returned guard across `.await` points or you will deadlock.
    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("database mutex poisoned")
    }
}

/// Set WAL mode and recommended PRAGMAs for concurrent agent access.
fn configure_connection(conn: &Connection) -> Result<(), DbError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// Run all pending migrations.
pub(crate) fn run_migrations(conn: &Connection) -> Result<(), DbError> {
    // Ensure schema_version table exists (it's part of migration 001,
    // but we need to check if ANY migrations have run)
    let has_schema_table: bool = conn
        .query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    let current_version: i64 = if has_schema_table {
        conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0)
    } else {
        0
    };

    for (sql, version) in schema::MIGRATIONS {
        if *version > current_version {
            conn.execute_batch(sql).map_err(|e| DbError::Migration {
                version: *version,
                source: e,
            })?;
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                [version],
            )?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_in_memory() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        // Verify WAL mode
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "memory"); // In-memory DBs report "memory" not "wal"

        // Verify tables exist
        let table_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('tasks', 'context', 'file_locks', 'schema_version', 'embeddings')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 5);
    }

    #[test]
    fn test_migrations_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        run_migrations(&conn).unwrap();
        // Running again should be a no-op
        run_migrations(&conn).unwrap();

        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 2);
    }
}
