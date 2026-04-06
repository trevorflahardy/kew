//! Core data types for kew's database layer.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Task lifecycle states.
///
/// ```text
/// pending → assigned → running → done
///                              → failed
///          (any) → cancelled
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    Assigned,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Assigned => write!(f, "assigned"),
            Self::Running => write!(f, "running"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl TaskStatus {
    /// Parse a status string, defaulting to `Pending` for unrecognised values.
    ///
    /// This is intentionally lossy — it never returns an error. Unknown strings
    /// (e.g. from a future schema version) silently become `Pending` rather than
    /// crashing the reader. Use only when deserialising from the database.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "assigned" => Self::Assigned,
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

/// LLM provider backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Ollama,
    Claude,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ollama => write!(f, "ollama"),
            Self::Claude => write!(f, "claude"),
        }
    }
}

impl Provider {
    /// Parse a provider string, defaulting to `Ollama` for unrecognised values.
    ///
    /// Intentionally lossy — unknown values (e.g. a future provider not yet in
    /// this binary) fall back to Ollama rather than panicking. Use only when
    /// deserialising from the database.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "claude" => Self::Claude,
            _ => Self::Ollama,
        }
    }
}

/// A unit of work: one prompt sent to one LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub parent_id: Option<String>,
    pub chain_id: Option<String>,
    pub chain_index: Option<i32>,
    pub status: TaskStatus,
    pub model: String,
    pub provider: Provider,
    pub system_prompt: Option<String>,
    pub prompt: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub context_keys: Vec<String>,
    pub share_as: Option<String>,
    pub files_locked: Vec<String>,
    pub worker_id: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub duration_ms: Option<i64>,
    pub agent: Option<String>,
}

/// Parameters for creating a new task.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub model: String,
    pub provider: Provider,
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub context_keys: Vec<String>,
    pub share_as: Option<String>,
    pub files_locked: Vec<String>,
    pub parent_id: Option<String>,
    pub chain_id: Option<String>,
    pub chain_index: Option<i32>,
}

/// Shared context entry stored between agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub key: String,
    pub namespace: String,
    pub content: String,
    pub content_hash: Option<String>,
    pub summary: Option<String>,
    pub metadata: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// File lock preventing concurrent edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLock {
    pub file_path: String,
    pub task_id: String,
    pub locked_at: i64,
    pub expires_at: Option<i64>,
}
