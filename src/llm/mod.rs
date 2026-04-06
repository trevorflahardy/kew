//! LLM client layer: trait definition and provider implementations.
//!
//! Defines the `LlmClient` trait that all providers (Ollama, Claude API)
//! implement. The router maps model names to the correct provider.

pub mod claude;
pub mod ollama;
pub mod router;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider not reachable at {url}: {source}")]
    Unreachable { url: String, source: reqwest::Error },
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("provider error ({status}): {body}")]
    ProviderError { status: u16, body: String },
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub model: String,
    pub done: bool,
    pub total_duration_ns: Option<i64>,
    pub prompt_eval_count: Option<i32>,
    pub eval_count: Option<i32>,
}

/// Execution stats returned alongside the response text.
#[derive(Debug, Clone, Default)]
pub struct CompletionStats {
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub duration_ms: Option<i64>,
}

/// The core trait every LLM provider implements.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a chat completion request and return the full response.
    async fn chat(&self, req: ChatRequest) -> Result<(ChatResponse, CompletionStats), LlmError>;

    /// Generate embeddings for the given texts.
    async fn embed(&self, model: &str, input: &[String]) -> Result<Vec<Vec<f32>>, LlmError>;

    /// List available models.
    async fn list_models(&self) -> Result<Vec<String>, LlmError>;

    /// Health check — is the provider reachable?
    async fn ping(&self) -> Result<(), LlmError>;

    /// Human-readable provider name.
    fn provider_name(&self) -> &str;
}
